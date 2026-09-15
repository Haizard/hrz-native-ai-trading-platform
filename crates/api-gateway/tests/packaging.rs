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
    let dockerfile =
        std::fs::read_to_string(repo_root().join("Dockerfile")).expect("the Dockerfile must exist");

    // Only the runtime stage matters: the builder has the whole tree via
    // `COPY . .`, so a missing copy there would be a different bug.
    let runtime = dockerfile
        .rsplit_once("FROM debian")
        .expect("the Dockerfile must have a debian runtime stage")
        .1;

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
