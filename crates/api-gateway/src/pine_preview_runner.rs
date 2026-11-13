//! Execution backends for server-side Pine-Lite previews.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, Semaphore};

/// Maximum serialized preview request sent to the worker.
pub const MAX_WORKER_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// Maximum serialized preview response read from the worker.
pub const MAX_WORKER_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Maximum evaluations handled before a worker process is recycled.
pub const MAX_WORKER_REQUESTS_PER_PROCESS: u32 = 32;
const WORKER_POOL_SIZE: usize = 4;
const WORKER_TIMEOUT: Duration = Duration::from_secs(15);

/// Preview execution backend shared by the gateway routes.
#[async_trait::async_trait]
pub trait PinePreviewExecutor: Send + Sync {
    /// Execute one preview and return bounded chart evidence.
    async fn execute(
        &self,
        request: sandbox::PinePreviewRequest,
    ) -> Result<sandbox::PinePreview, String>;
}

/// Development and test backend that runs the Wasmtime guest in this process.
pub struct InProcessPinePreview {
    sandbox: Arc<sandbox::Sandbox>,
}

impl InProcessPinePreview {
    /// Use the gateway's existing Wasmtime host for local non-Linux development.
    pub fn new(sandbox: Arc<sandbox::Sandbox>) -> Self {
        Self { sandbox }
    }
}

#[async_trait::async_trait]
impl PinePreviewExecutor for InProcessPinePreview {
    async fn execute(
        &self,
        request: sandbox::PinePreviewRequest,
    ) -> Result<sandbox::PinePreview, String> {
        self.sandbox
            .execute_pine_preview(request)
            .map(|(preview, _)| preview)
            .map_err(|error| error.to_string())
    }
}

/// Bounded child-process pool used by the Linux gateway.
pub struct PinePreviewWorker {
    executable: PathBuf,
    available: Semaphore,
    workers: Vec<Mutex<Option<WorkerProcess>>>,
}

impl PinePreviewWorker {
    /// Resolve the worker from `PINE_PREVIEW_WORKER_PATH` or beside the gateway
    /// executable. Missing workers are startup errors; Linux never falls back
    /// to in-process execution.
    pub fn from_env() -> std::io::Result<Self> {
        let path = match std::env::var_os("PINE_PREVIEW_WORKER_PATH") {
            Some(path) => PathBuf::from(path),
            None => {
                let gateway = std::env::current_exe()?;
                let parent = gateway.parent().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "gateway executable has no parent directory",
                    )
                })?;
                parent.join("pine-preview-worker")
            }
        };
        Self::new(path)
    }

    /// Create a runner for a specific worker executable.
    pub fn new(executable: impl AsRef<Path>) -> std::io::Result<Self> {
        let executable = std::fs::canonicalize(executable)?;
        if !executable.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Pine preview worker path is not a regular file",
            ));
        }
        Ok(Self {
            executable,
            available: Semaphore::new(WORKER_POOL_SIZE),
            workers: (0..WORKER_POOL_SIZE).map(|_| Mutex::new(None)).collect(),
        })
    }

    /// Verify that the worker can enable its kernel restrictions.
    pub async fn validate_isolation(&self) -> Result<(), String> {
        let mut command = Command::new(&self.executable);
        command
            .arg("--self-test")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let output = tokio::time::timeout(WORKER_TIMEOUT, command.output())
            .await
            .map_err(|_| "Pine preview worker isolation self-test timed out".to_string())?
            .map_err(|error| format!("could not start Pine preview worker: {error}"))?;
        if !output.status.success() || output.stdout.as_slice() != b"isolated" {
            return Err(format!(
                "Pine preview worker isolation self-test failed: {}",
                String::from_utf8_lossy(&output.stdout)
            ));
        }
        Ok(())
    }

    async fn execute_inner(
        &self,
        request: sandbox::PinePreviewRequest,
    ) -> Result<sandbox::PinePreview, String> {
        let request = serde_json::to_vec(&request)
            .map_err(|error| format!("could not encode Pine preview request: {error}"))?;
        if request.len() > MAX_WORKER_REQUEST_BYTES {
            return Err(format!(
                "Pine preview request is {} bytes, exceeding the {} byte worker limit",
                request.len(),
                MAX_WORKER_REQUEST_BYTES
            ));
        }

        let _permit = self
            .available
            .acquire()
            .await
            .map_err(|_| "Pine preview worker pool is shutting down".to_string())?;
        for slot in &self.workers {
            if let Ok(mut worker) = slot.try_lock() {
                return self.execute_in_slot(&mut worker, &request).await;
            }
        }
        Err("no Pine preview worker slot was available".to_string())
    }

    async fn execute_in_slot(
        &self,
        slot: &mut Option<WorkerProcess>,
        request: &[u8],
    ) -> Result<sandbox::PinePreview, String> {
        if slot.is_none() {
            *slot = Some(WorkerProcess::spawn(&self.executable)?);
        }

        let response = {
            let worker = slot
                .as_mut()
                .ok_or_else(|| "Pine preview worker failed to initialize".to_string())?;
            tokio::time::timeout(WORKER_TIMEOUT, worker.exchange(request)).await
        };
        let response = match response {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                if let Some(mut worker) = slot.take() {
                    worker.stop().await;
                }
                return Err(error);
            }
            Err(_) => {
                if let Some(mut worker) = slot.take() {
                    worker.stop().await;
                }
                return Err(format!(
                    "Pine preview worker exceeded its {WORKER_TIMEOUT:?} deadline"
                ));
            }
        };

        let worker = slot
            .as_mut()
            .ok_or_else(|| "Pine preview worker exited unexpectedly".to_string())?;
        if let Some(status) = worker
            .child
            .try_wait()
            .map_err(|error| format!("could not inspect Pine preview worker: {error}"))?
        {
            *slot = None;
            return match response {
                WorkerResponse::Success { preview } if status.success() => Ok(preview),
                WorkerResponse::Success { .. } => Err(format!(
                    "Pine preview worker returned success with failing exit status {status}"
                )),
                WorkerResponse::Failure { message } => Err(message),
            };
        }
        let recycle = worker.requests_served >= MAX_WORKER_REQUESTS_PER_PROCESS;
        if recycle {
            let status = worker
                .child
                .wait()
                .await
                .map_err(|error| format!("could not reap Pine preview worker: {error}"))?;
            *slot = None;
            if !status.success() {
                return Err(format!(
                    "Pine preview worker exited with {status} while being recycled"
                ));
            }
        }

        match response {
            WorkerResponse::Success { preview } => Ok(preview),
            WorkerResponse::Failure { message } => Err(message),
        }
    }
}

struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    requests_served: u32,
}

impl WorkerProcess {
    fn spawn(executable: &Path) -> Result<Self, String> {
        let mut command = Command::new(executable);
        command
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start Pine preview worker: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Pine preview worker stdin was unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Pine preview worker stdout was unavailable".to_string())?;
        Ok(Self {
            child,
            stdin,
            stdout,
            requests_served: 0,
        })
    }

    async fn exchange(&mut self, request: &[u8]) -> Result<WorkerResponse, String> {
        let request_len = u32::try_from(request.len())
            .map_err(|_| "Pine preview request is too large for worker IPC".to_string())?;
        self.stdin
            .write_all(&request_len.to_be_bytes())
            .await
            .map_err(|error| format!("could not send request to Pine preview worker: {error}"))?;
        self.stdin
            .write_all(request)
            .await
            .map_err(|error| format!("could not send request to Pine preview worker: {error}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| format!("could not flush Pine preview request: {error}"))?;

        let mut response_len = [0; 4];
        self.stdout
            .read_exact(&mut response_len)
            .await
            .map_err(|error| format!("could not read Pine preview worker response: {error}"))?;
        let response_len = u32::from_be_bytes(response_len) as usize;
        if response_len > MAX_WORKER_RESPONSE_BYTES {
            return Err("Pine preview worker response exceeded the 4 MiB limit".to_string());
        }
        let mut response = vec![0; response_len];
        self.stdout
            .read_exact(&mut response)
            .await
            .map_err(|error| format!("incomplete Pine preview worker response: {error}"))?;
        self.requests_served = self.requests_served.saturating_add(1);
        serde_json::from_slice(&response)
            .map_err(|error| format!("invalid Pine preview worker response: {error}"))
    }

    async fn stop(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

#[async_trait::async_trait]
impl PinePreviewExecutor for PinePreviewWorker {
    async fn execute(
        &self,
        request: sandbox::PinePreviewRequest,
    ) -> Result<sandbox::PinePreview, String> {
        self.execute_inner(request).await
    }
}

/// The JSON response shared by the gateway and the worker executable.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerResponse {
    /// Preview completed successfully.
    Success {
        /// Bounded preview evidence.
        preview: sandbox::PinePreview,
    },
    /// Worker setup or preview execution failed.
    Failure {
        /// Explicit failure message.
        message: String,
    },
}
