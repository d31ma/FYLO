use std::io::{BufReader, ErrorKind, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fylo_storage_native::RootLease;

use super::server_hello::ServerHello;
use super::server_state::ServerState;
use crate::{FrameSession, MachineError, RequestFrame};

/// One authenticated client and its persistent machine session.
///
/// Between frames it waits with a short poll, so it notices shutdown and its
/// idle timeout without another thread having to interrupt its read; no
/// platform wakes a blocked read reliably from outside.
pub(crate) struct ServerConnection {
    stream: TcpStream,
    state: Arc<ServerState>,
    lease: RootLease,
}

impl ServerConnection {
    const POLL_INTERVAL: Duration = Duration::from_millis(200);

    pub(crate) const fn new(stream: TcpStream, state: Arc<ServerState>, lease: RootLease) -> Self {
        Self {
            stream,
            state,
            lease,
        }
    }

    /// Refuse a connection over the limit without reading from it.
    pub(crate) fn refuse_busy(mut stream: TcpStream) {
        let error = MachineError::new(
            "ESERVERBUSY",
            "the server is at its connection limit or stopping",
        );
        drop(stream.write_all(&ServerHello::refused(&error)));
    }

    /// Authenticate, then answer frames until the client or the server stops.
    ///
    /// # Errors
    /// Returns the transport error that ended the connection.
    pub(crate) fn run(mut self) -> std::io::Result<()> {
        self.stream.set_nodelay(true)?;
        self.stream
            .set_write_timeout(Some(self.state.options().idle_timeout()))?;
        let mut reader = BufReader::new(self.stream.try_clone()?);
        let Some(hello) = self.authenticate(&mut reader)? else {
            return Ok(());
        };
        if hello.wants_shutdown() {
            self.state.request_shutdown();
            return Ok(());
        }
        self.serve_frames(&mut reader)
    }

    fn authenticate(
        &mut self,
        reader: &mut BufReader<TcpStream>,
    ) -> std::io::Result<Option<ServerHello>> {
        reader
            .get_ref()
            .set_read_timeout(Some(self.state.options().hello_timeout()))?;
        let frame = Self::read_hello(reader)?;
        let admitted = ServerHello::admit(&frame, self.state.options().token());
        self.reply_to_hello(admitted)
    }

    /// An oversized hello comes back empty and is refused as malformed.
    fn read_hello(reader: &mut BufReader<TcpStream>) -> std::io::Result<Vec<u8>> {
        let mut frame = Vec::new();
        crate::read_frame(reader, ServerHello::MAX_BYTES, &mut frame)?;
        Ok(frame)
    }

    fn reply_to_hello(
        &mut self,
        admitted: Result<ServerHello, MachineError>,
    ) -> std::io::Result<Option<ServerHello>> {
        match admitted {
            Ok(hello) => {
                self.stream.write_all(&ServerHello::accepted())?;
                Ok(Some(hello))
            }
            Err(error) => {
                self.stream.write_all(&ServerHello::refused(&error))?;
                Ok(None)
            }
        }
    }

    fn serve_frames(&mut self, reader: &mut BufReader<TcpStream>) -> std::io::Result<()> {
        let mut session = self.open_session()?;
        while session.is_open() && self.wait_for_frame(reader)? {
            let frame = RequestFrame::read(reader, session.max_request_bytes())?;
            if frame.is_end_of_stream() {
                break;
            }
            let response = self.answer(&mut session, frame)?;
            self.stream.write_all(&response)?;
        }
        Ok(())
    }

    /// True once a frame's first byte is here; false at end of stream, when
    /// the server is stopping, or after the idle timeout.
    fn wait_for_frame(&self, reader: &mut BufReader<TcpStream>) -> std::io::Result<bool> {
        if !reader.buffer().is_empty() {
            return Ok(true);
        }
        let idle_since = Instant::now();
        reader
            .get_ref()
            .set_read_timeout(Some(Self::POLL_INTERVAL))?;
        let mut arrived = Self::poll(reader.get_ref())?;
        while arrived.is_none() && !self.should_stop_waiting(idle_since) {
            arrived = Self::poll(reader.get_ref())?;
        }
        reader
            .get_ref()
            .set_read_timeout(Some(self.state.options().idle_timeout()))?;
        Ok(arrived == Some(true))
    }

    /// `Some(true)` for data, `Some(false)` for end of stream, `None` when
    /// the poll interval passed with nothing. Peeking consumes no byte.
    fn poll(stream: &TcpStream) -> std::io::Result<Option<bool>> {
        match stream.peek(&mut [0_u8; 1]) {
            Ok(read) => Ok(Some(read > 0)),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn should_stop_waiting(&self, idle_since: Instant) -> bool {
        self.state.is_stopping() || idle_since.elapsed() >= self.state.options().idle_timeout()
    }

    fn open_session(&self) -> std::io::Result<FrameSession> {
        let _executor = self.state.execute()?;
        Ok(FrameSession::open(
            self.lease.clone(),
            self.state.application().clone(),
            self.state.options().limits(),
        ))
    }

    fn answer(&self, session: &mut FrameSession, frame: RequestFrame) -> std::io::Result<Vec<u8>> {
        let _executor = self.state.execute()?;
        session.answer(frame)
    }
}
