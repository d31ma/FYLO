use std::sync::Arc;

use fylo_engine::HotSet;
use fylo_storage_native::{RootLease, RootOpenOptions};

use crate::{ApplicationConfig, FrameLimits, FrameOutcome, RequestFrame, Session};

/// A persistent machine session that a network server drives one frame at a
/// time.
///
/// It is the `exec --loop --exclusive-root` session over a root lease the
/// server already holds, so it keeps its query cursors between frames.
/// Frames come from a network client, which cannot name another root or a
/// host path or URL.
pub struct FrameSession {
    session: Session,
    open: bool,
}

impl FrameSession {
    /// Admit the leased root and recover it, as a local session would.
    ///
    /// An admission failure is answered on the first frame, which then ends
    /// the session.
    #[must_use]
    pub fn open(lease: RootLease, application: ApplicationConfig, limits: FrameLimits) -> Self {
        let options = RootOpenOptions::new(lease.root()).with_lease(lease);
        let mut session = Session::open(Some(options), application, (limits, true, true));
        session.remote = true;
        Self {
            session,
            open: true,
        }
    }

    /// Answer reads from `hot_set`, which every session of one server shares.
    ///
    /// The server must clear it around every frame that may write.
    #[must_use]
    pub fn with_hot_set(mut self, hot_set: Arc<HotSet>) -> Self {
        self.session.hot_set = Some(hot_set);
        self
    }

    /// The largest request frame this session accepts.
    #[must_use]
    pub const fn max_request_bytes(&self) -> usize {
        self.session.limits.max_request_bytes
    }

    /// Answer one frame with exactly one LF-terminated response frame.
    ///
    /// # Errors
    /// Returns an error only when the response cannot be encoded.
    pub fn answer(&mut self, frame: RequestFrame) -> std::io::Result<Vec<u8>> {
        let (parts, started) = frame.into_parts();
        let mut response = Vec::new();
        let outcome = self.session.answer_frame(parts, started, &mut response)?;
        self.open = matches!(outcome, FrameOutcome::Handled);
        Ok(response)
    }

    /// False once a frame has ended the session.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }
}
