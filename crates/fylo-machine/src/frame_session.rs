use std::io::{BufRead, Write};

use fylo_storage_native::{RootLease, RootOpenOptions};

use crate::{ApplicationConfig, FrameLimits, FrameOutcome, RequestFrame, Session};

/// A persistent machine session that the host drives one frame at a time.
///
/// It is the `exec --loop --exclusive-root` session over a root lease the
/// host already holds, for a host that reads frames from its own transport.
/// It keeps its query cursors between frames and, unless the application's
/// hot-set budget is zero, answers warm reads from memory. Frames run as the
/// host, exactly as they would on `exec --loop`.
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
        let session = Session::open(Some(options), application, (limits, true, true));
        Self {
            session,
            open: true,
        }
    }

    /// A session over no default root, for an embedder whose storage is its
    /// own host table, such as the browser module: each request names its
    /// root, nothing is leased, and query cursors last as long as the session
    /// does rather than one batch of frames.
    #[must_use]
    pub fn embedded(application: ApplicationConfig, limits: FrameLimits) -> Self {
        let session = Session::open(None, application, (limits, false, true));
        Self {
            session,
            open: true,
        }
    }

    /// Answer every frame `input` holds, as `exec --loop` does, and keep the
    /// session for the next batch.
    ///
    /// # Errors
    /// Returns an error when the transport fails.
    pub fn serve<R: BufRead, W: Write>(
        &mut self,
        input: &mut R,
        output: &mut W,
    ) -> std::io::Result<()> {
        while self.open {
            match self.session.handle_frame(input, output)? {
                FrameOutcome::Handled => {}
                FrameOutcome::Terminate => self.open = false,
                FrameOutcome::Eof => break,
            }
        }
        Ok(())
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
