use std::io::{self, Read, Write};
use std::process::ExitCode;

use api_gateway::pine_preview_runner::{
    WorkerResponse, MAX_WORKER_REQUESTS_PER_PROCESS, MAX_WORKER_REQUEST_BYTES,
};
use sandbox::{PinePreviewRequest, Sandbox};

const MAX_WORKER_RESPONSE_BYTES: usize =
    api_gateway::pine_preview_runner::MAX_WORKER_RESPONSE_BYTES;

fn main() -> ExitCode {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--self-test")) {
        return match self_test() {
            Ok(()) => {
                if io::stdout().write_all(b"isolated").is_ok() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(error) => {
                let _ = io::stdout().write_all(error.as_bytes());
                ExitCode::FAILURE
            }
        };
    }
    if std::env::args_os().nth(1).is_some() {
        let response = WorkerResponse::Failure {
            message: "unknown Pine preview worker argument".to_string(),
        };
        let _ = write_frame(&mut io::stdout(), &response);
        return ExitCode::FAILURE;
    }

    match run_worker() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let response = WorkerResponse::Failure { message: error };
            if write_frame(&mut io::stdout(), &response).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "linux")]
fn self_test() -> Result<(), String> {
    sandbox::worker::apply_resource_limits()
        .map_err(|error| format!("could not apply Pine preview worker resource limits: {error}"))?;
    sandbox::worker::install_filesystem_restrictions()
        .map_err(|error| format!("could not enable filesystem isolation: {error}"))?;
    let _sandbox = Sandbox::new()
        .map_err(|error| format!("could not initialize Wasmtime preview host: {error}"))?;
    let (release, wait) = std::sync::mpsc::channel();
    let verifier = std::thread::spawn(move || {
        wait.recv()
            .map_err(|error| format!("could not start thread-isolation probe: {error}"))?;
        verify_restrictions()
    });
    sandbox::worker::install_syscall_restrictions()
        .map_err(|error| format!("could not enable syscall isolation: {error}"))?;
    verify_restrictions()?;
    release
        .send(())
        .map_err(|error| format!("could not release thread-isolation probe: {error}"))?;
    verifier
        .join()
        .map_err(|_| "worker thread-isolation probe panicked".to_string())?
}

#[cfg(not(target_os = "linux"))]
fn self_test() -> Result<(), String> {
    Err("Pine preview worker isolation self-test is supported only on Linux".to_string())
}

fn run_worker() -> Result<(), String> {
    sandbox::worker::apply_resource_limits()
        .map_err(|error| format!("could not apply Pine preview worker resource limits: {error}"))?;
    sandbox::worker::install_filesystem_restrictions()
        .map_err(|error| format!("could not enable filesystem isolation: {error}"))?;
    let sandbox = Sandbox::new()
        .map_err(|error| format!("could not initialize Wasmtime preview host: {error}"))?;
    sandbox::worker::install_syscall_restrictions()
        .map_err(|error| format!("could not enable syscall isolation: {error}"))?;

    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for _ in 0..MAX_WORKER_REQUESTS_PER_PROCESS {
        let Some(request_bytes) = read_request(&mut input)? else {
            return Ok(());
        };
        let response = match serde_json::from_slice::<PinePreviewRequest>(&request_bytes) {
            Ok(request) => match sandbox.execute_pine_preview(request) {
                Ok((preview, _)) => WorkerResponse::Success { preview },
                Err(error) => WorkerResponse::Failure {
                    message: error.to_string(),
                },
            },
            Err(error) => WorkerResponse::Failure {
                message: format!("invalid Pine preview worker request: {error}"),
            },
        };
        write_frame(&mut output, &response)?;
    }
    Ok(())
}

fn read_request(reader: &mut impl Read) -> Result<Option<Vec<u8>>, String> {
    let mut length = [0; 4];
    match reader.read(&mut length[..1]) {
        Ok(0) => return Ok(None),
        Ok(1) => {}
        Ok(_) => unreachable!("one-byte read cannot return more than one byte"),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
            return read_request(reader);
        }
        Err(error) => {
            return Err(format!(
                "could not read Pine preview request frame: {error}"
            ))
        }
    }
    reader
        .read_exact(&mut length[1..])
        .map_err(|error| format!("incomplete Pine preview request frame: {error}"))?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_WORKER_REQUEST_BYTES {
        return Err(format!(
            "Pine preview request exceeded the {} byte worker limit",
            MAX_WORKER_REQUEST_BYTES
        ));
    }
    let mut request = vec![0; length];
    reader
        .read_exact(&mut request)
        .map_err(|error| format!("incomplete Pine preview request body: {error}"))?;
    Ok(Some(request))
}

fn write_frame(writer: &mut impl Write, response: &WorkerResponse) -> Result<(), String> {
    let response = serde_json::to_vec(response)
        .map_err(|error| format!("could not encode Pine preview result: {error}"))?;
    if response.len() > MAX_WORKER_RESPONSE_BYTES {
        return Err(format!(
            "Pine preview result exceeded the {} byte worker limit",
            MAX_WORKER_RESPONSE_BYTES
        ));
    }
    let length = u32::try_from(response.len())
        .map_err(|_| "Pine preview response is too large for worker IPC".to_string())?;
    writer
        .write_all(&length.to_be_bytes())
        .and_then(|_| writer.write_all(&response))
        .and_then(|_| writer.flush())
        .map_err(|error| format!("could not return Pine preview result: {error}"))
}

#[cfg(target_os = "linux")]
fn verify_restrictions() -> Result<(), String> {
    if std::env::vars_os().next().is_some() {
        return Err("worker inherited environment variables".to_string());
    }
    match std::fs::File::open("/etc/passwd") {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
        Err(error) => return Err(format!("unexpected filesystem restriction error: {error}")),
        Ok(_) => return Err("worker can read host files".to_string()),
    }
    match std::net::TcpStream::connect(("127.0.0.1", 9)) {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {}
        Err(error) => return Err(format!("unexpected network restriction error: {error}")),
        Ok(_) => return Err("worker can create a network connection".to_string()),
    }
    Ok(())
}
