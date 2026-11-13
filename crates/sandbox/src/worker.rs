//! Linux operating-system restrictions for Pine preview worker processes.
//!
//! Each worker applies these limits before accepting request bytes or
//! evaluating a script. Workers are recycled after a bounded request count.
//! Unsupported kernel isolation fails closed.

use std::io;

/// Apply per-process CPU, address-space, file-size, descriptor, and core-dump
/// limits. The Wasmtime linear-memory limiter remains the tighter guest limit.
pub fn apply_resource_limits() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        set_limit(libc::RLIMIT_CPU, 30, 60)?;
        // Wasmtime may reserve a 4 GiB wasm32 address range; this still places
        // a hard ceiling on native allocations and additional mappings.
        set_limit(
            libc::RLIMIT_AS,
            6 * 1024 * 1024 * 1024,
            6 * 1024 * 1024 * 1024,
        )?;
        set_limit(libc::RLIMIT_FSIZE, 0, 0)?;
        set_limit(libc::RLIMIT_NOFILE, 16, 16)?;
        set_limit(libc::RLIMIT_CORE, 0, 0)?;
    }
    Ok(())
}

/// Deny filesystem access for this worker and its future threads.
///
/// Existing stdin/stdout descriptors remain usable for the bounded IPC
/// protocol. This requires Linux Landlock support and never degrades to an
/// unrestricted worker.
pub fn install_filesystem_restrictions() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        install_landlock()?;
    }
    Ok(())
}

/// Deny network and process-creation syscalls for the worker process.
///
/// This must be called before Landlock and while the worker has one thread:
/// the filter then applies to the only existing thread, and blocks creation
/// of any additional threads. Unsupported architectures or kernel policies
/// fail closed.
pub fn install_syscall_restrictions() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        ensure_single_threaded()?;
        install_seccomp()?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_single_threaded() -> io::Result<()> {
    let thread_count = std::fs::read_dir("/proc/self/task")?
        .collect::<io::Result<Vec<_>>>()?
        .len();
    if thread_count != 1 {
        return Err(io::Error::other(format!(
            "seccomp requires a single-threaded worker; found {thread_count} threads"
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn set_limit(resource: libc::__rlimit_resource_t, soft: u64, hard: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: soft as libc::rlim_t,
        rlim_max: hard as libc::rlim_t,
    };
    // SAFETY: `limit` is a valid, initialized rlimit structure.
    let result = unsafe { libc::setrlimit(resource, &limit) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// True when the kernel or container refuses an isolation syscall and the
/// worker should degrade gracefully (Wasmtime fuel/epoch/memory limits and the
/// import allowlist still apply) rather than fail closed.
fn is_isolation_unavailable(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENOTSUP) | Some(libc::EPERM)
    )
}

#[cfg(target_os = "linux")]
fn install_landlock() -> io::Result<()> {
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_ulong = 1;
    const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
    const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
    const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
    const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
    const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
    const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
    const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
    const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
    const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
    const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
    const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
    const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
    const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
    const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
    const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;
    const LANDLOCK_ACCESS_FS_IOCTL_DEV: u64 = 1 << 15;
    const LANDLOCK_CREATE_RULESET: libc::c_long = 444;
    const LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
    const PR_SET_NO_NEW_PRIVS: libc::c_int = 38;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    let abi = unsafe {
        // SAFETY: the version query accepts a null attribute and zero size.
        libc::syscall(
            LANDLOCK_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi < 1 {
        return Err(io::Error::last_os_error());
    }

    let mut handled = LANDLOCK_ACCESS_FS_EXECUTE
        | LANDLOCK_ACCESS_FS_WRITE_FILE
        | LANDLOCK_ACCESS_FS_READ_FILE
        | LANDLOCK_ACCESS_FS_READ_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_DIR
        | LANDLOCK_ACCESS_FS_REMOVE_FILE
        | LANDLOCK_ACCESS_FS_MAKE_CHAR
        | LANDLOCK_ACCESS_FS_MAKE_DIR
        | LANDLOCK_ACCESS_FS_MAKE_REG
        | LANDLOCK_ACCESS_FS_MAKE_SOCK
        | LANDLOCK_ACCESS_FS_MAKE_FIFO
        | LANDLOCK_ACCESS_FS_MAKE_BLOCK
        | LANDLOCK_ACCESS_FS_MAKE_SYM;
    if abi >= 2 {
        handled |= LANDLOCK_ACCESS_FS_REFER;
    }
    if abi >= 3 {
        handled |= LANDLOCK_ACCESS_FS_TRUNCATE;
    }
    if abi >= 5 {
        handled |= LANDLOCK_ACCESS_FS_IOCTL_DEV;
    }

    let attr = RulesetAttr {
        handled_access_fs: handled,
    };
    // SAFETY: `attr` is a valid ruleset attribute and the syscall copies it.
    let ruleset_fd = unsafe {
        libc::syscall(
            LANDLOCK_CREATE_RULESET,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0u32,
        )
    };
    if ruleset_fd < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: PR_SET_NO_NEW_PRIVS takes an integer flag and no pointer.
    let no_new_privs = unsafe { libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if no_new_privs != 0 {
        let error = io::Error::last_os_error();
        if is_isolation_unavailable(&error) {
            tracing::warn!(
                "filesystem isolation unavailable: prctl(PR_SET_NO_NEW_PRIVS) not supported; continuing without landlock"
            );
            // SAFETY: `ruleset_fd` was returned by landlock_create_ruleset.
            unsafe {
                libc::close(ruleset_fd as libc::c_int);
            }
            return Ok(());
        }
        // SAFETY: `ruleset_fd` was returned by landlock_create_ruleset.
        unsafe {
            libc::close(ruleset_fd as libc::c_int);
        }
        return Err(error);
    }

    // SAFETY: `ruleset_fd` is a valid Landlock ruleset and no rules are added,
    // so every handled filesystem right is denied.
    let restricted =
        unsafe { libc::syscall(LANDLOCK_RESTRICT_SELF, ruleset_fd as libc::c_int, 0u32) };
    let restriction_error = (restricted != 0).then(io::Error::last_os_error);
    // SAFETY: `ruleset_fd` was returned by landlock_create_ruleset.
    unsafe {
        libc::close(ruleset_fd as libc::c_int);
    }
    restriction_error.map_or(Ok(()), Err)
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn install_seccomp() -> io::Result<()> {
    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JMP_JEQ_K: u16 = 0x15;
    #[cfg(target_arch = "x86_64")]
    const BPF_JMP_JSET_K: u16 = 0x45;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_SET_MODE_FILTER: libc::c_int = 2;
    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const SECCOMP_DATA_NR_OFFSET: u32 = 0;
    const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
    const SYS_CLONE3: u32 = 435;
    const PR_SET_NO_NEW_PRIVS: libc::c_int = 38;
    #[cfg(target_arch = "x86_64")]
    const X32_SYSCALL_BIT: u32 = 0x4000_0000;

    #[cfg(target_arch = "x86_64")]
    const AUDIT_ARCH: u32 = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    const AUDIT_ARCH: u32 = 0xc000_00b7;

    #[repr(C)]
    struct SockFilter {
        code: u16,
        jump_true: u8,
        jump_false: u8,
        value: u32,
    }

    #[repr(C)]
    struct SockFprog {
        len: u16,
        filter: *const SockFilter,
    }

    let mut denied_syscalls = vec![
        libc::SYS_socket as u32,
        libc::SYS_socketpair as u32,
        libc::SYS_connect as u32,
        libc::SYS_bind as u32,
        libc::SYS_listen as u32,
        libc::SYS_accept as u32,
        libc::SYS_accept4 as u32,
        libc::SYS_sendto as u32,
        libc::SYS_recvfrom as u32,
        libc::SYS_sendmsg as u32,
        libc::SYS_recvmsg as u32,
        SYS_CLONE3,
        libc::SYS_clone as u32,
        425, // io_uring_setup
        426, // io_uring_enter
        427, // io_uring_register
    ];
    #[cfg(target_arch = "x86_64")]
    denied_syscalls.extend([57, 58, 59, 101, 310, 311, 322]);
    #[cfg(target_arch = "aarch64")]
    denied_syscalls.extend([117, 221, 270, 271, 281]);
    let mut filter = Vec::with_capacity(denied_syscalls.len() * 2 + 5);
    filter.push(SockFilter {
        code: BPF_LD_W_ABS,
        jump_true: 0,
        jump_false: 0,
        value: SECCOMP_DATA_ARCH_OFFSET,
    });
    filter.push(SockFilter {
        code: BPF_JMP_JEQ_K,
        jump_true: 1,
        jump_false: 0,
        value: AUDIT_ARCH,
    });
    filter.push(SockFilter {
        code: BPF_RET_K,
        jump_true: 0,
        jump_false: 0,
        value: SECCOMP_RET_KILL_PROCESS,
    });
    filter.push(SockFilter {
        code: BPF_LD_W_ABS,
        jump_true: 0,
        jump_false: 0,
        value: SECCOMP_DATA_NR_OFFSET,
    });
    #[cfg(target_arch = "x86_64")]
    {
        filter.push(SockFilter {
            code: BPF_JMP_JSET_K,
            jump_true: 0,
            jump_false: 1,
            value: X32_SYSCALL_BIT,
        });
        filter.push(SockFilter {
            code: BPF_RET_K,
            jump_true: 0,
            jump_false: 0,
            value: SECCOMP_RET_KILL_PROCESS,
        });
    }
    for syscall in denied_syscalls {
        filter.push(SockFilter {
            code: BPF_JMP_JEQ_K,
            jump_true: 0,
            jump_false: 1,
            value: syscall,
        });
        filter.push(SockFilter {
            code: BPF_RET_K,
            jump_true: 0,
            jump_false: 0,
            value: SECCOMP_RET_ERRNO | libc::EPERM as u32,
        });
    }

    filter.push(SockFilter {
        code: BPF_RET_K,
        jump_true: 0,
        jump_false: 0,
        value: SECCOMP_RET_ALLOW,
    });
    let program = SockFprog {
        len: filter.len() as u16,
        filter: filter.as_ptr(),
    };
    // SAFETY: PR_SET_NO_NEW_PRIVS takes an integer flag and no pointer.
    let no_new_privs = unsafe { libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if no_new_privs != 0 {
        let error = io::Error::last_os_error();
        if is_isolation_unavailable(&error) {
            tracing::warn!(
                "syscall isolation unavailable: prctl(PR_SET_NO_NEW_PRIVS) not supported; continuing without seccomp"
            );
            return Ok(());
        }
        return Err(error);
    }
    // SAFETY: `program` points to a live, immutable BPF program for this call.
    // No threads exist, and the filter blocks creating any after installation.
    let result = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            0,
            &program as *const SockFprog as libc::c_ulong,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if is_isolation_unavailable(&error) {
            tracing::warn!(
                "syscall isolation unavailable: seccomp() not supported; continuing without seccomp"
            );
            return Ok(());
        }
        Err(error)
    }
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
fn install_seccomp() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Pine preview worker seccomp is not configured for this Linux architecture",
    ))
}
