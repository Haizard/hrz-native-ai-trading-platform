//! On-demand chart captures (docs/48): the shell's upload lands here, and the
//! run that asked for the image picks it up by id.
//!
//! The flow, end to end: the agent's `look_at_chart` tool returns a
//! `capture_chart` command with a rendezvous id; the gateway relays it over
//! the agent socket's progress channel; the shell captures the canvas and
//! POSTs the PNG here under the same id; the run polls this buffer briefly
//! and hands the image to the model. Ordinary HTTP for the upload, because
//! the agent socket processes one message at a time and the run in flight is
//! that one message.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ai_agent::chart_context::ChartScreenshot;
use async_trait::async_trait;

/// One uploaded capture: whose screen sent it, the image, and when it landed.
struct CaptureEntry {
    user_id: String,
    shot: ChartScreenshot,
    at: Instant,
}

/// How long an upload stays claimable. A run polls for seconds, so a minute
/// is generous; past it, the entry is a stale picture nobody should see.
const CAPTURE_TTL: Duration = Duration::from_secs(60);

/// The most entries the buffer holds. Captures are a per-run rendezvous, so
/// more than a handful means uploads nobody is waiting for.
const MAX_ENTRIES: usize = 32;

/// The shared buffer: AppState holds one, uploads write, runs read.
#[derive(Clone, Default)]
pub struct CaptureBuffer {
    entries: Arc<Mutex<HashMap<String, CaptureEntry>>>,
}

impl CaptureBuffer {
    /// Store the shell's upload under its rendezvous id, tagged with the
    /// authenticated user so one user's upload can never feed another's run.
    pub fn put(&self, user_id: &str, capture_id: &str, shot: ChartScreenshot) {
        let mut entries = match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // The sweep rides the write: a buffer that only ever grows is a leak
        // wearing a TTL costume.
        entries.retain(|_, entry| entry.at.elapsed() < CAPTURE_TTL);
        if entries.len() >= MAX_ENTRIES {
            if let Some(oldest) = entries
                .iter()
                .max_by_key(|(id, entry)| (entry.at.elapsed(), (*id).clone()))
                .map(|(id, _)| id.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            capture_id.to_string(),
            CaptureEntry {
                user_id: user_id.to_string(),
                shot,
                at: Instant::now(),
            },
        );
    }

    /// The per-user view the run's tool context carries.
    pub fn for_user(&self, user_id: &str) -> Arc<dyn ai_agent::ChartCaptureSource> {
        Arc::new(UserCaptures {
            buffer: self.clone(),
            user_id: user_id.to_string(),
        })
    }
}

/// The run's view: takes uploads, but only its own user's.
struct UserCaptures {
    buffer: CaptureBuffer,
    user_id: String,
}

#[async_trait]
impl ai_agent::ChartCaptureSource for UserCaptures {
    async fn take(&self, capture_id: &str) -> Option<ChartScreenshot> {
        let mut entries = match self.buffer.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let entry = entries.get(capture_id)?;
        // Ownership is the check, not the id: a capture id is guessable in
        // principle, and a cross-user image is a worse failure than a missed
        // one. Stale entries read as absent rather than as old pictures.
        if entry.user_id != self.user_id || entry.at.elapsed() >= CAPTURE_TTL {
            return None;
        }
        entries.remove(capture_id).map(|entry| entry.shot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(label: &str) -> ChartScreenshot {
        ChartScreenshot {
            media_type: "image/png".to_string(),
            data: "aGVsbG8=".to_string(),
            label: Some(label.to_string()),
        }
    }

    #[tokio::test]
    async fn an_upload_is_taken_once_by_its_own_user() {
        let buffer = CaptureBuffer::default();
        buffer.put("u1", "cap-1", shot("first"));
        let source = buffer.for_user("u1");
        let taken = source.take("cap-1").await.expect("the upload is there");
        assert_eq!(taken.label.as_deref(), Some("first"));
        assert!(
            source.take("cap-1").await.is_none(),
            "takes rather than peeks: the second read finds nothing"
        );
    }

    #[tokio::test]
    async fn one_users_upload_never_feeds_anothers_run() {
        let buffer = CaptureBuffer::default();
        buffer.put("u1", "cap-1", shot("private"));
        assert!(buffer.for_user("u2").take("cap-1").await.is_none());
        // And it is still there for its owner afterwards: a stranger's probe
        // must not consume it.
        assert!(buffer.for_user("u1").take("cap-1").await.is_some());
    }
}
