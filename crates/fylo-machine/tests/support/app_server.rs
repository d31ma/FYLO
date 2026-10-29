//! A stand-in for an application server that embeds FYLO, for tests only.
//!
//! FYLO ships no server. An application opens its database in its own
//! process, as it would an `SQLite` file, and answers its own clients. This
//! fixture is the smallest such application: it takes the root's exclusive
//! lease, opens one embedded `FrameSession` (one database handle, like one
//! `SQLite` connection), and lets any number of local TCP clients send it
//! machine frames, which it answers one at a time.

use std::io::{BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use fylo_machine::{ApplicationConfig, FrameLimits, FrameSession, RequestFrame};
use fylo_storage_native::RootLease;

pub struct AppServer {
    address: SocketAddr,
    stopping: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
}

impl AppServer {
    /// Open `root` the way an application would and listen on loopback.
    pub fn start(root: &Path, application: ApplicationConfig) -> Self {
        let lease = RootLease::acquire(root).expect("the app takes the root's lease");
        let session = FrameSession::open(lease, application, FrameLimits::default());
        let session = Arc::new(Mutex::new(session));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("loopback binds");
        let address = listener.local_addr().unwrap();
        let stopping = Arc::new(AtomicBool::new(false));
        let acceptor = {
            let stopping = Arc::clone(&stopping);
            std::thread::spawn(move || Self::accept(&listener, &session, &stopping))
        };
        Self {
            address,
            stopping,
            acceptor: Some(acceptor),
        }
    }

    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    fn accept(listener: &TcpListener, session: &Arc<Mutex<FrameSession>>, stopping: &AtomicBool) {
        let mut connections = Vec::new();
        for stream in listener.incoming() {
            if stopping.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { continue };
            let session = Arc::clone(session);
            connections.push(std::thread::spawn(move || Self::answer(stream, &session)));
        }
        for connection in connections {
            drop(connection.join());
        }
    }

    /// Answer one client's frames until it disconnects.
    fn answer(stream: TcpStream, session: &Mutex<FrameSession>) {
        let Ok(mut writer) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(stream);
        let limit = session.lock().unwrap().max_request_bytes();
        while let Ok(frame) = RequestFrame::read(&mut reader, limit) {
            if frame.is_end_of_stream() {
                return;
            }
            let response = session.lock().unwrap().answer(frame);
            if response.and_then(|bytes| writer.write_all(&bytes)).is_err() {
                return;
            }
        }
    }
}

impl Drop for AppServer {
    /// Stop accepting, wait for every client to leave, and release the root.
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        drop(TcpStream::connect(self.address));
        if let Some(acceptor) = self.acceptor.take() {
            drop(acceptor.join());
        }
    }
}
