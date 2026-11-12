use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use fylo_machine::{
    ApplicationConfig, FrameLimits, InvocationOptions, InvocationRuntime, PROTOCOL_VERSION,
    StrictValue,
};
use fylo_storage_native::{FilesystemProfile, RootOpenOptions};
use serde_json::{Value, json};

/// The host settings one invocation runs with: its root, frame limits and
/// budget (deadline and cleanup reserve).
type InvocationSettings = (RootOpenOptions, FrameLimits, (Instant, Duration));

/// `fylo exec --invocation`: one request in one process, under one bounded
/// root lease (ADR 0011). The answer frame, the exit code and the use of
/// stdout and stderr are the same as `fylo exec --request`.
pub struct InvocationCommand;

impl InvocationCommand {
    const DEADLINE_OPTION: &'static str = "--deadline-ms";
    const RESERVE_OPTION: &'static str = "--cleanup-reserve-ms";
    const REQUEST_OPTION: &'static str = "--request";
    const DEADLINE_CODE: &'static str = "EDEADLINE";
    const BAD_REQUEST_CODE: &'static str = "EBADREQUEST";
    const UNSUPPORTED_CODE: &'static str = "EUNSUPPORTEDOP";
    const FAILED_CODE: &'static str = "EINVOCATION";
    const REQUEST_TOO_LARGE: &'static str =
        "EFRAME_REQUEST_TOO_LARGE: invocation request exceeds the configured frame limit";
    const ERROR_NAME: &'static str = "FyloMachineError";
    const CODE_SEPARATOR: &'static str = ": ";

    /// Answer one request and exit: 0 when the answer frame is `ok`, 1 for
    /// a failure frame or a host configuration error on stderr.
    pub fn run(
        arguments: &[String],
        application: ApplicationConfig,
        profile: FilesystemProfile,
    ) -> ExitCode {
        let started = Instant::now();
        let outcome = Self::answer(arguments, application, (profile, started));
        let succeeded = Self::print(outcome);
        ExitCode::from(u8::from(!succeeded))
    }

    fn answer(
        arguments: &[String],
        application: ApplicationConfig,
        (profile, started): (FilesystemProfile, Instant),
    ) -> Result<Vec<u8>, String> {
        Self::refuse_session_options(arguments)?;
        let request = Self::request(arguments)?;
        let frame = Self::frame(&request)?;
        let settings = Self::settings(arguments, &application, (profile, started))?;
        Ok(Self::execute(settings, application, &frame)
            .unwrap_or_else(|error| Self::failure_frame(&request, &error, started)))
    }

    fn print(outcome: Result<Vec<u8>, String>) -> bool {
        match outcome {
            Ok(answer) => {
                print!("{}", String::from_utf8_lossy(&answer));
                Self::answered_ok(&answer)
            }
            Err(error) => {
                eprintln!("{error}");
                false
            }
        }
    }

    fn answered_ok(answer: &[u8]) -> bool {
        let frame = serde_json::from_slice::<Value>(answer).unwrap_or(Value::Null);
        frame.get("ok") == Some(&Value::Bool(true))
    }

    fn refuse_session_options(arguments: &[String]) -> Result<(), String> {
        if arguments
            .iter()
            .any(|argument| matches!(argument.as_str(), "--loop" | "--exclusive-root"))
        {
            return Err(
                "exec --invocation answers one request; it cannot be combined with --loop or --exclusive-root"
                    .into(),
            );
        }
        Ok(())
    }

    fn request(arguments: &[String]) -> Result<Value, String> {
        let source = super::required_option(arguments, Self::REQUEST_OPTION)?;
        let text = super::read_json_source(source)?;
        let request = serde_json::from_str::<StrictValue>(&text)
            .map_err(|error| format!("invalid request JSON: {error}"))?;
        Self::object_request(request.into_inner())
    }

    fn object_request(request: Value) -> Result<Value, String> {
        if !request.is_object() {
            return Err("machine request body must be a JSON object".into());
        }
        Ok(request)
    }

    fn frame(request: &Value) -> Result<Vec<u8>, String> {
        let mut frame = serde_json::to_vec(request).map_err(|error| error.to_string())?;
        frame.push(b'\n');
        Ok(frame)
    }

    fn settings(
        arguments: &[String],
        application: &ApplicationConfig,
        (profile, started): (FilesystemProfile, Instant),
    ) -> Result<InvocationSettings, String> {
        let root = Self::root(application)?;
        let limits = super::machine_limits(arguments)?.clamped();
        let deadline = Self::milliseconds(arguments, Self::DEADLINE_OPTION)?;
        let reserve = Self::milliseconds(arguments, Self::RESERVE_OPTION)?;
        let deadline = Self::deadline(started, deadline)?;
        let root = RootOpenOptions::new(root).with_profile(profile);
        Ok((root, limits, (deadline, reserve)))
    }

    fn root(application: &ApplicationConfig) -> Result<PathBuf, String> {
        application
            .root
            .clone()
            .ok_or_else(|| "exec --invocation requires --root or fylo.root".into())
    }

    fn milliseconds(arguments: &[String], name: &str) -> Result<Duration, String> {
        let value = super::required_option(arguments, name)?;
        value
            .parse::<u64>()
            .map(Duration::from_millis)
            .map_err(|error| format!("invalid {name}: {error}"))
    }

    fn deadline(started: Instant, budget: Duration) -> Result<Instant, String> {
        started
            .checked_add(budget)
            .ok_or_else(|| format!("{} is too large", Self::DEADLINE_OPTION))
    }

    fn execute(
        (root, limits, budget): InvocationSettings,
        application: ApplicationConfig,
        frame: &[u8],
    ) -> io::Result<Vec<u8>> {
        Self::require_frame_size(frame, limits)?;
        let options = InvocationOptions::new(root, application, limits, budget)?;
        InvocationRuntime::new(options)?.execute(frame)
    }

    /// The same limit and code as `exec --request`; the runtime would refuse
    /// the frame too, but without the frame code.
    fn require_frame_size(frame: &[u8], limits: FrameLimits) -> io::Result<()> {
        if frame.len().saturating_sub(1) > limits.max_request_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                Self::REQUEST_TOO_LARGE,
            ));
        }
        Ok(())
    }

    /// The failure frame `exec --request` writes, for an invocation the
    /// runtime refused or could not finish.
    fn failure_frame(request: &Value, error: &io::Error, started: Instant) -> Vec<u8> {
        let message = error.to_string();
        let code = Self::written_code(&message).unwrap_or_else(|| Self::kind_code(error.kind()));
        let frame = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "ok": false,
            "op": request.get("op"),
            "requestId": request.get("requestId"),
            "durationMs": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "error": { "name": Self::ERROR_NAME, "message": message, "code": code },
        });
        let mut answer = serde_json::to_vec(&frame).unwrap_or_default();
        answer.push(b'\n');
        answer
    }

    /// Storage and machine errors write their code first, as `CODE: message`.
    fn written_code(message: &str) -> Option<&str> {
        message
            .split_once(Self::CODE_SEPARATOR)
            .map(|(code, _)| code)
            .filter(|code| Self::is_code(code))
    }

    fn is_code(text: &str) -> bool {
        text.starts_with('E') && text.bytes().all(Self::is_code_byte)
    }

    const fn is_code_byte(byte: u8) -> bool {
        matches!(byte, b'A'..=b'Z' | b'0'..=b'9' | b'_')
    }

    fn kind_code(kind: io::ErrorKind) -> &'static str {
        match kind {
            io::ErrorKind::TimedOut => Self::DEADLINE_CODE,
            io::ErrorKind::InvalidInput => Self::BAD_REQUEST_CODE,
            io::ErrorKind::Unsupported => Self::UNSUPPORTED_CODE,
            _ => Self::FAILED_CODE,
        }
    }
}
