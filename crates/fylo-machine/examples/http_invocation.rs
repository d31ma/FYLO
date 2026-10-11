//! HTTP rehearsal of the one-frame invocation API for Cloud Run and Azure
//! Functions custom handlers.
//!
//! Qualification harness only, not a supported deployment, like
//! `lambda_invocation`: it listens on `PORT` (Cloud Run) or
//! `FUNCTIONS_CUSTOMHANDLER_PORT` (Azure Functions), runs each POST body as one
//! FYLO core request against `FYLO_ROOT` with the `efs-sidecar-v1` profile, and
//! answers with the response and timing. Every request gets its own
//! `FYLO_REQUEST_TIMEOUT_MS` budget and its own thread, since both platforms
//! send concurrent requests to one instance.
// ponytail: hand-rolled HTTP/1.1, one request per connection; use an HTTP
// server crate if this ever becomes a supported adapter.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use fylo_machine::{ApplicationConfig, FrameLimits, InvocationOptions, InvocationRuntime};
use fylo_storage_native::{FilesystemProfile, RootOpenOptions};
use serde_json::{Value, json};

const CLEANUP_RESERVE: Duration = Duration::from_secs(2);
const MAX_BODY: usize = 8 << 20;

fn main() -> io::Result<()> {
    let port = std::env::var("FUNCTIONS_CUSTOMHANDLER_PORT")
        .or_else(|_| std::env::var("PORT"))
        .unwrap_or_else(|_| "8080".into());
    let timeout = Duration::from_millis(
        std::env::var("FYLO_REQUEST_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(30_000),
    );
    let cold = Instant::now();
    let listener = TcpListener::bind(("0.0.0.0", port.parse::<u16>().map_err(io::Error::other)?))?;
    eprintln!("fylo http invocation listening on {port}");
    for stream in listener.incoming() {
        let stream = stream?;
        std::thread::spawn(move || {
            if let Err(error) = serve(stream, cold, timeout) {
                eprintln!("request failed: {error}");
            }
        });
    }
    Ok(())
}

fn serve(stream: TcpStream, cold: Instant, timeout: Duration) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let method = line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut length = 0;
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':')
            && key.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().map_err(io::Error::other)?;
        }
    }
    if length > MAX_BODY {
        return respond(stream, 413, &json!({ "error": "request body too large" }));
    }
    let mut event = vec![0; length];
    reader.read_exact(&mut event)?;
    if method != "POST" {
        // Readiness and platform probes.
        return respond(stream, 200, &json!({ "ready": true }));
    }
    let started = Instant::now();
    let outcome = invoke(&event, started + timeout);
    let body = json!({
        "instance": std::env::var("HOSTNAME").unwrap_or_default(),
        "sinceColdStartMs": cold.elapsed().as_millis(),
        "elapsedMs": started.elapsed().as_millis(),
        "peakRssKb": peak_rss_kb(),
        "outcome": outcome.unwrap_or_else(|error| json!({ "noCompletedResponse": error.to_string() })),
    });
    respond(stream, 200, &body)
}

fn invoke(event: &[u8], deadline: Instant) -> io::Result<Value> {
    let root = std::env::var("FYLO_ROOT").unwrap_or_else(|_| "/mnt/state/root".into());
    let options = InvocationOptions::new(
        RootOpenOptions::new(root).with_profile(FilesystemProfile::EfsSidecarV1),
        ApplicationConfig::default(),
        FrameLimits::default(),
        (deadline, CLEANUP_RESERVE),
    )?;
    // Clients such as curl may end the body with a newline of their own.
    let mut frame = event.trim_ascii_end().to_vec();
    frame.push(b'\n');
    let response = InvocationRuntime::new(options)?.execute(&frame)?;
    serde_json::from_slice(&response).map_err(io::Error::other)
}

fn respond(mut stream: TcpStream, status: u16, body: &Value) -> io::Result<()> {
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn peak_rss_kb() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()
}
