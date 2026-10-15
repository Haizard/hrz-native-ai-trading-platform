//! On-demand chart captures: the agent asks for a fresh image of the user's
//! screen mid-run, the shell uploads it, and the loop hands it to the model.
//!
//! The channel is asymmetric on purpose. The command to capture rides the
//! progress channel (server -> shell, live, next to `open_chart`), while the
//! image itself comes back over an ordinary HTTP upload (shell -> server),
//! because the agent socket processes one message at a time and a run in
//! flight is that one message. This module is the rendezvous: the run asks
//! for the upload by id, briefly, and gets on with the analysis either way.

use async_trait::async_trait;

use crate::chart_context::ChartScreenshot;

/// Where a run's requested chart captures land, attached by the host.
///
/// Same posture as the drawings and memory sources: `None` on the context
/// means "no screen is attached" and the tool says so, rather than a capture
/// request that waits on nothing.
#[async_trait]
pub trait ChartCaptureSource: Send + Sync {
    /// Take the capture uploaded under `capture_id`, if it has arrived yet.
    ///
    /// Takes rather than peeks: one upload feeds the one run that asked for
    /// it, and a second run that somehow learned the id finds nothing rather
    /// than a stale picture of somebody else's chart.
    async fn take(&self, capture_id: &str) -> Option<ChartScreenshot>;
}

/// The asking user's capture rendezvous, attached by the host.
///
/// Same ownership argument as the other contexts: the buffer is user-bound
/// one layer up (the gateway keys uploads by the authenticated identity), so
/// the agent sees only "a source", never whose screen is behind it.
pub struct CapturesContext {
    source: std::sync::Arc<dyn ChartCaptureSource>,
}

impl CapturesContext {
    /// Bind a capture source to one ask.
    #[must_use]
    pub fn new(source: std::sync::Arc<dyn ChartCaptureSource>) -> Self {
        Self { source }
    }

    /// The source itself, for the tool context.
    pub(crate) fn source(&self) -> &std::sync::Arc<dyn ChartCaptureSource> {
        &self.source
    }
}

impl Clone for CapturesContext {
    fn clone(&self) -> Self {
        Self {
            source: std::sync::Arc::clone(&self.source),
        }
    }
}

impl std::fmt::Debug for CapturesContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The buffer is a channel into the host; printing it would print an
        // address. Its presence is the fact worth logging.
        f.write_str("CapturesContext(..)")
    }
}
