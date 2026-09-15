//! The deployed image must contain what the gateway reads at runtime.
//!
//! ## Why this test exists
//!
//! The gateway serves the MVP chart from `frontend/mvp` and loads the skill
//! library from `skills`, both resolved relative to the working directory. A
//! deploy went out whose image contained neither, and the failure was *quiet*:
//!
//! ```
//! WARN api_gateway: MVP page not served path=frontend/mvp/index.html
//!      error=No such file or directory (os error 2)
//! INFO api_gateway: skills loaded count=0 dir=skills
//! ```
//!
//! The page 404'd and the agent started with an empty methodology library,
//! which reads as "no matching skill" rather than "the files are not in the
//! image". The cause was two lines that were each true when written and had
//! quietly stopped being true:
//!
//! * `.dockerignore` excluded `frontend/` with the note *"not built yet; add
//!   back when Phase 7 lands"* -- and then Phase 5 shipped a page into it;
//! * the runtime stage of the `Dockerfile` copied the two binaries and nothing
//!   else, so `skills/` reached the builder and never reached the image.
//!
//! Neither is a thing a unit test notices, because the code is correct: it
//! reads the right paths and handles absence gracefully. What is wrong is the
//! *packaging*, so that is what this test checks.
//!
//! It parses rather than executes -- no Docker in CI -- so it is a guard
//! against the specific mistake, not a substitute for building the image.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("the repository root must exist")
}

/// Whether a `.dockerignore` pattern excludes `path`.
///
/// An approximation of Docker's rules: it handles the plain directory and file
/// patterns this file actually contains, and deliberately does not attempt
/// globs, `**`, or negation ordering. A guard that is honest about being
/// partial beats one that pretends to be a Docker implementation.
fn excludes(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern.starts_with('#') || pattern.starts_with('!') {
        return false;
    }
    let pattern = pattern.trim_start_matches('/').trim_end_matches('/');
    let path = path.trim_end_matches('/');
    pattern == path || path.starts_with(&format!("{pattern}/"))
}

fn dockerignore_lines() -> Vec<String> {
    std::fs::read_to_string(repo_root().join(".dockerignore"))
        .expect(".dockerignore must exist")
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn the_dockerignore_does_not_exclude_what_the_gateway_serves() {
    // The exact mistake: `frontend/` was excluded for a year of project time
    // after it had stopped being empty.
    let lines = dockerignore_lines();
    for path in ["frontend", "skills", "strategies"] {
        let offending: Vec<&str> = lines
            .iter()
            .filter(|line| excludes(line, path))
            .map(String::as_str)
            .collect();
        assert!(
            offending.is_empty(),
            "`{path}` is excluded by .dockerignore, so it never reaches the build context and \
             cannot be copied into the image. Offending line(s): {offending:?}"
        );
    }
}

#[test]
fn the_runtime_stage_copies_the_assets_the_gateway_reads() {
    let dockerfile = dockerfile();

    // Only the runtime stage matters: the builder has the whole tree via
    // `COPY . .`, so a missing copy there would be a different bug.
    let runtime = runtime_stage(&dockerfile);

    for asset in ["frontend", "skills", "strategies"] {
        assert!(
            runtime.contains(&format!("/app/{asset}")),
            "the runtime stage never copies `{asset}` into /app. The gateway resolves it \
             relative to the working directory, so it will not be there at runtime.\n\
             Runtime stage:\n{runtime}"
        );
    }
}

#[test]
fn the_paths_the_gateway_defaults_to_actually_exist() {
    // If either default is renamed in the code, the copies above would go
    // stale silently -- which is the whole shape of the bug being guarded.
    let root = repo_root();
    assert!(
        root.join("frontend/mvp/index.html").is_file(),
        "the gateway serves `frontend/mvp/index.html`; it is not in the repository"
    );
    assert!(
        root.join("skills").is_dir(),
        "the gateway loads skills from `skills/`; it is not in the repository"
    );

    // And the directory is not merely present: an empty one loads as an empty
    // library and logs a warning rather than failing, so an empty skills/ would
    // still degrade the agent quietly.
    let skills: Vec<_> = walk(&root.join("skills"));
    assert!(
        !skills.is_empty(),
        "`skills/` exists but holds no documents, so the agent would start with an empty library"
    );
}

/// The shell is four files the gateway serves by name, and a `<script>` tag
/// pointing at one that is not there is a blank panel with no error anyone
/// reads. `builder.js` is the newest and the easiest to forget, because it is
/// loaded from `index.html` rather than from a route a test already knows.
#[test]
fn every_file_the_shell_loads_is_served_by_a_route_and_present() {
    let root = repo_root();
    let index = std::fs::read_to_string(root.join("frontend/app/index.html"))
        .expect("frontend/app/index.html must exist");

    // What the page asks for.
    let mut referenced: Vec<String> = Vec::new();
    for line in index.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("<script src=\"") else {
            continue;
        };
        let Some(src) = rest.split('"').next() else {
            continue;
        };
        referenced.push(src.to_string());
    }
    assert!(
        referenced.iter().any(|s| s == "builder.js"),
        "index.html does not load builder.js: {referenced:?}"
    );

    // What the router serves.
    let router = std::fs::read_to_string(root.join("crates/api-gateway/src/lib.rs"))
        .expect("the gateway's lib.rs must exist");
    for src in &referenced {
        assert!(
            router.contains(&format!("\"/{src}\"")),
            "index.html loads `/{src}` but no route serves it"
        );
        assert!(
            root.join("frontend/app").join(src).is_file(),
            "index.html loads `/{src}` but frontend/app/{src} does not exist, so it would 404"
        );
    }
}

/// Every file under `dir`, recursively.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// What a real `docker build` would catch, checked without Docker.
//
// The image has never been built (no container runtime on the development
// machine, and CI builds it only on deploy), so the checks below stand in for
// the ones a build would fail on. They are deliberately the failures that are
// *quiet*: a `--bin` that does not exist fails loudly the moment anyone builds,
// which is why there is no test for it here.
// ---------------------------------------------------------------------------

fn dockerfile() -> String {
    std::fs::read_to_string(repo_root().join("Dockerfile")).expect("the Dockerfile must exist")
}

/// The runtime stage: everything after the `FROM debian` line.
fn runtime_stage(dockerfile: &str) -> &str {
    dockerfile
        .rsplit_once("FROM debian")
        .expect("the Dockerfile must have a debian runtime stage")
        .1
}

/// Every `--bin X` the build line asks cargo for.
fn built_binaries(dockerfile: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = dockerfile;
    while let Some(at) = rest.find("--bin ") {
        rest = &rest[at + "--bin ".len()..];
        names.push(leading_token(rest));
    }
    names
}

/// Every `target/release/X` the runtime stage copies out of the builder.
fn copied_binaries(runtime: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = runtime;
    while let Some(at) = rest.find("target/release/") {
        rest = &rest[at + "target/release/".len()..];
        names.push(leading_token(rest));
    }
    names
}

/// The leading run of characters that can appear in a file name.
fn leading_token(text: &str) -> String {
    text.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .collect()
}

/// The first integer appearing at or after `after`, e.g. the port in
/// `EXPOSE 8080` or `127.0.0.1:8080`.
fn first_number_after(text: &str, after: &str) -> Option<u16> {
    let rest = &text[text.find(after)? + after.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The argument of the final `CMD [...]` line.
fn cmd_argument(dockerfile: &str) -> String {
    let at = dockerfile
        .rfind("CMD [")
        .expect("the Dockerfile must have an exec-form CMD");
    leading_token(&dockerfile[at + "CMD [\"".len()..])
}

#[test]
fn every_binary_the_build_produces_is_copied_into_the_image() {
    // Both directions matter, and they fail differently: a binary copied but
    // never built fails the build (loud, so it is not the point of this test),
    // while a binary built and *not* copied is a silent waste -- the image
    // simply does not have it.
    let dockerfile = dockerfile();
    let built = built_binaries(&dockerfile);
    let copied = copied_binaries(runtime_stage(&dockerfile));

    assert!(!built.is_empty(), "the build line names no `--bin`");
    for name in &built {
        assert!(
            copied.contains(name),
            "`{name}` is built but never copied into the runtime stage, so the image does not \
             have it. Built: {built:?}. Copied: {copied:?}"
        );
    }
}

#[test]
fn the_entrypoint_names_a_script_that_is_copied_and_present() {
    let dockerfile = dockerfile();
    let runtime = runtime_stage(&dockerfile);

    let at = dockerfile
        .find("ENTRYPOINT [\"")
        .expect("the Dockerfile must declare an ENTRYPOINT");
    let script = leading_token(&dockerfile[at + "ENTRYPOINT [\"".len()..]);
    assert!(!script.is_empty(), "the ENTRYPOINT names nothing");

    // ENTRYPOINT naming a file that was never COPYed is a container that starts
    // and immediately dies with "executable file not found" -- a deploy-time
    // failure whose cause is a line in a file nobody re-reads.
    assert!(
        runtime.contains(&format!("/{script}")),
        "the ENTRYPOINT runs `{script}` but the runtime stage never copies it, so every \
         container would fail to start.\nRuntime stage:\n{runtime}"
    );
    assert!(
        repo_root().join(&script).is_file(),
        "the ENTRYPOINT runs `{script}` but it is not in the repository"
    );
}

#[test]
fn the_entrypoint_script_is_lf_so_its_shebang_works() {
    // A CRLF shebang (`#!/bin/sh\r`) makes the kernel look for an interpreter
    // literally named `/bin/sh\r`, and the container dies with "no such file or
    // directory" for a file that is plainly there. `.gitattributes` pins this,
    // but the pin only holds for files it names -- and this one is easy to
    // rewrite on a Windows machine.
    let dockerfile = dockerfile();
    let at = dockerfile
        .find("ENTRYPOINT [\"")
        .expect("the Dockerfile must declare an ENTRYPOINT");
    let script = leading_token(&dockerfile[at + "ENTRYPOINT [\"".len()..]);

    let bytes = std::fs::read(repo_root().join(&script)).expect("the entrypoint must be readable");
    assert!(
        !bytes.windows(2).any(|w| w == b"\r\n"),
        "`{script}` has CRLF line endings; the container would fail with a confusing \
         \"no such file or directory\" from its own shebang"
    );
    assert!(
        bytes.starts_with(b"#!/bin/sh\n") || bytes.starts_with(b"#!/usr/bin/env sh\n"),
        "`{script}` does not start with a shebang, so the kernel cannot run it"
    );
}

#[test]
fn the_entrypoint_can_run_every_command_it_mentions() {
    // The entrypoint applies migrations by running `xtask migrate`, so the image
    // has to contain `xtask` and not only the gateway. Dropping it from the COPY
    // list would leave a container that builds, starts, and then cannot migrate.
    let entrypoint = std::fs::read_to_string(repo_root().join("docker-entrypoint.sh"))
        .expect("docker-entrypoint.sh must exist");
    assert!(
        entrypoint.contains("xtask migrate"),
        "the entrypoint no longer applies migrations; if that is deliberate, this test and the \
         RUN_MIGRATIONS default in the Dockerfile should be removed together"
    );

    let dockerfile = dockerfile();
    let copied = copied_binaries(runtime_stage(&dockerfile));
    assert!(
        copied.contains(&"xtask".to_string()),
        "the entrypoint runs `xtask migrate` but the image never copies `xtask`. Copied: {copied:?}"
    );
    assert!(
        copied.contains(&cmd_argument(&dockerfile)),
        "the default CMD names a binary the image does not copy. Copied: {copied:?}"
    );
}

#[test]
fn the_port_is_the_same_in_every_place_that_names_it() {
    // Three lines name a port and nothing makes them agree. A healthcheck on the
    // wrong port marks the container unhealthy forever while it serves traffic
    // perfectly, and the deploy flaps for no visible reason.
    let dockerfile = dockerfile();
    let exposed = first_number_after(&dockerfile, "EXPOSE ").expect("EXPOSE must name a port");
    let bound =
        first_number_after(&dockerfile, "BIND_ADDR=0.0.0.0:").expect("BIND_ADDR must name a port");
    let probed =
        first_number_after(&dockerfile, "127.0.0.1:").expect("the healthcheck must name a port");

    assert_eq!(exposed, bound, "EXPOSE and BIND_ADDR disagree");
    assert_eq!(exposed, probed, "EXPOSE and the healthcheck disagree");
}

#[test]
fn the_wasm_the_shell_loads_is_committed_and_is_really_a_wasm_module() {
    // The image ships the working tree; nothing in the Dockerfile rebuilds the
    // engine. So a gitignored `chart_engine.wasm` -- the natural instinct, since
    // it *is* a build output -- produces an image that serves a 404 where the
    // chart engine should be, with no error anywhere. That is the same shape of
    // quiet failure this file exists for.
    let wasm = repo_root().join("frontend/app/chart_engine.wasm");
    assert!(
        wasm.is_file(),
        "`frontend/app/chart_engine.wasm` is not in the repository. The gateway serves it and the \
         Dockerfile does not build it, so it has to be committed."
    );

    let bytes = std::fs::read(&wasm).expect("the engine must be readable");
    assert!(
        bytes.starts_with(b"\0asm"),
        "`frontend/app/chart_engine.wasm` is not a wasm module (no \\0asm magic). If it is a Git \
         LFS pointer, the deployed page would fail to instantiate it."
    );

    // An approximation of git's rules, like `excludes` above: the failure mode
    // is a `*.wasm` line, so that is what is looked for. Checked at the three
    // levels that could plausibly carry one -- a repo-wide walk would crawl
    // `target/`, which is fifty thousand files of nothing.
    let mut offenders = Vec::new();
    for dir in ["", "frontend", "frontend/app"] {
        let entry = repo_root().join(dir).join(".gitignore");
        let Ok(text) = std::fs::read_to_string(&entry) else {
            continue;
        };
        for line in text.lines().map(str::trim) {
            if line == "*.wasm" || line == "chart_engine.wasm" || line == "frontend" {
                offenders.push(format!("{dir}/.gitignore: {line}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the chart engine is ignored by git, so it would not reach the image: {offenders:?}"
    );
}
