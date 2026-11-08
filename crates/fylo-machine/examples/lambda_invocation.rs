//! Lambda custom-runtime rehearsal for the one-frame invocation API.
//!
//! Qualification harness only, not a supported deployment: it speaks the
//! Lambda runtime API over plain HTTP on the loopback socket Lambda provides,
//! runs one FYLO core request per event against `FYLO_ROOT` with the
//! `efs-sidecar-v1` profile, and reports the response with timing. Each event
//! gets a fresh budget from the provider deadline.
// ponytail: hand-rolled HTTP/1.1 against the local runtime API only; use a
// Lambda runtime crate if this ever becomes a supported adapter.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fylo_machine::{ApplicationConfig, FrameLimits, InvocationOptions, InvocationRuntime};
use fylo_storage_native::{FilesystemProfile, RootOpenOptions};
use serde_json::{Value, json};

const CLEANUP_RESERVE: Duration = Duration::from_secs(2);

type Headers = Vec<(String, String)>;

fn main() -> io::Result<()> {
    let api = std::env::var("AWS_LAMBDA_RUNTIME_API").map_err(io::Error::other)?;
    let root = std::env::var("FYLO_ROOT").unwrap_or_else(|_| "/mnt/state/root".into());
    // The root sits one level under the EFS mount so its owner lock, which
    // lives beside the root, lands on EFS too. The profile needs the root to
    // exist, and nothing else provisions it on Lambda.
    std::fs::create_dir_all(&root)?;
    let cold = Instant::now();
    let mut warm = false;
    loop {
        let (headers, event) = http(&api, "GET", "/2018-06-01/runtime/invocation/next", b"")?;
        let id = header(&headers, "lambda-runtime-aws-request-id").unwrap_or_default();
        let deadline_ms: u128 = header(&headers, "lambda-runtime-deadline-ms")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_millis();
        let remaining =
            Duration::from_millis(u64::try_from(deadline_ms.saturating_sub(now_ms)).unwrap_or(0));
        let started = Instant::now();
        let outcome = invoke(&root, &event, started + remaining);
        let body = json!({
            "warm": warm,
            "sinceColdStartMs": cold.elapsed().as_millis(),
            "elapsedMs": started.elapsed().as_millis(),
            "remainingAtStartMs": remaining.as_millis(),
            "peakRssKb": peak_rss_kb(),
            "outcome": outcome.unwrap_or_else(|error| json!({ "noCompletedResponse": error.to_string() })),
        });
        warm = true;
        let path = format!("/2018-06-01/runtime/invocation/{id}/response");
        http(&api, "POST", &path, body.to_string().as_bytes())?;
    }
}

fn invoke(root: &str, event: &[u8], deadline: Instant) -> io::Result<Value> {
    let options = InvocationOptions::new(
        RootOpenOptions::new(root).with_profile(FilesystemProfile::EfsSidecarV1),
        ApplicationConfig::default(),
        FrameLimits::default(),
        (deadline, CLEANUP_RESERVE),
    )?;
    let mut frame = event.to_vec();
    frame.push(b'\n');
    let response = InvocationRuntime::new(options)?.execute(&frame)?;
    serde_json::from_slice(&response).map_err(io::Error::other)
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

fn header(headers: &Headers, name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn http(host: &str, method: &str, path: &str, body: &[u8]) -> io::Result<(Headers, Vec<u8>)> {
    let mut stream = TcpStream::connect(host)?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut headers = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            headers.push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
    let mut body = Vec::new();
    reader.read_to_end(&mut body)?;
    Ok((headers, body))
}
