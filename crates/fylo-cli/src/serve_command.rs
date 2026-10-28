use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use fylo_machine::{ApplicationConfig, FrameLimits, MachineServer, ServeOptions, ServeToken};
use fylo_storage_native::NativeWriteRoot;
use serde_json::{Value, json};

/// `fylo serve`: own one root and answer the machine protocol over TCP.
///
/// `fylo serve --stop` asks a running server to stop gracefully. Killing the
/// process is also safe: it is recovered like any crashed FYLO process.
pub struct ServeCommand;

impl ServeCommand {
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

    pub fn run(arguments: &[String], application: ApplicationConfig) -> ExitCode {
        let outcome = if arguments.iter().any(|argument| argument == "--stop") {
            Self::stop(arguments)
        } else {
            Self::serve(arguments, application)
        };
        Self::exit_code(outcome)
    }

    fn serve(arguments: &[String], application: ApplicationConfig) -> Result<(), String> {
        let root = Self::root(&application)?;
        let options = Self::options(arguments)?;
        let server = MachineServer::bind(&root, application, options)
            .map_err(|error| format!("fylo serve cannot start: {error}"))?;
        Self::announce(&server)?;
        server
            .run()
            .map_err(|error| format!("fylo serve stopped uncleanly: {error}"))?;
        NativeWriteRoot::close_logs()
            .map_err(|error| format!("fylo serve could not checkpoint its logs: {error}"))
    }

    fn stop(arguments: &[String]) -> Result<(), String> {
        let address = Self::address(Self::required(arguments, "--connect")?)?;
        let token = Self::token(arguments)?;
        let token = String::from_utf8_lossy(token.secret()).into_owned();
        let mut stream = TcpStream::connect_timeout(&address, Self::CONNECT_TIMEOUT)
            .map_err(|error| format!("cannot reach fylo serve at {address}: {error}"))?;
        let hello = json!({ "fyloServe": 1, "token": token, "shutdown": true });
        writeln!(stream, "{hello}").map_err(|error| error.to_string())?;
        Self::require_accepted(&stream)
    }

    fn require_accepted(stream: &TcpStream) -> Result<(), String> {
        let mut reply = String::new();
        BufReader::new(stream)
            .read_line(&mut reply)
            .map_err(|error| error.to_string())?;
        let reply: Value = serde_json::from_str(&reply).map_err(|error| error.to_string())?;
        if reply["ok"] == json!(true) {
            return Ok(());
        }
        Err(format!("fylo serve refused to stop: {}", reply["error"]))
    }

    /// One JSON line on stdout says the server is ready, with the real port.
    fn announce(server: &MachineServer) -> Result<(), String> {
        let ready = json!({
            "listening": server.local_addr().to_string(),
            "root": server.root(),
        });
        let mut output = std::io::stdout().lock();
        writeln!(output, "{ready}")
            .and_then(|()| output.flush())
            .map_err(|error| error.to_string())
    }

    fn root(application: &ApplicationConfig) -> Result<PathBuf, String> {
        application
            .root
            .clone()
            .ok_or_else(|| "fylo serve requires --root or fylo.root".to_owned())
    }

    fn options(arguments: &[String]) -> Result<ServeOptions, String> {
        let listen = Self::address(Self::required(arguments, "--listen")?)?;
        let token = Self::token(arguments)?;
        let mut options = ServeOptions::new(listen, token).with_limits(Self::limits(arguments)?);
        if let Some(count) = Self::number(arguments, "--max-connections")? {
            options = options.with_max_connections(Self::to_usize(count)?);
        }
        if let Some(milliseconds) = Self::number(arguments, "--idle-timeout-ms")? {
            options = options.with_idle_timeout(Duration::from_millis(milliseconds));
        }
        if let Some(bytes) = Self::size(arguments, "--hot-set-bytes")? {
            options = options.with_hot_set_budget(bytes);
        }
        Ok(options)
    }

    fn limits(arguments: &[String]) -> Result<FrameLimits, String> {
        let defaults = FrameLimits::default();
        Ok(FrameLimits {
            max_request_bytes: Self::size(arguments, "--max-request-bytes")?
                .unwrap_or(defaults.max_request_bytes),
            max_response_bytes: Self::size(arguments, "--max-response-bytes")?
                .unwrap_or(defaults.max_response_bytes),
        })
    }

    /// The secret comes from a file, never the command line, where any
    /// local user could read it from the process list.
    fn token(arguments: &[String]) -> Result<ServeToken, String> {
        let path = Path::new(Self::required(arguments, "--token-file")?);
        ServeToken::from_file(path)
            .map_err(|error| format!("cannot use --token-file {}: {error}", path.display()))
    }

    fn address(text: &str) -> Result<SocketAddr, String> {
        text.to_socket_addrs()
            .map_err(|error| format!("invalid address {text}: {error}"))?
            .next()
            .ok_or_else(|| format!("address {text} resolves to nothing"))
    }

    fn size(arguments: &[String], name: &str) -> Result<Option<usize>, String> {
        Self::number(arguments, name)?
            .map(Self::to_usize)
            .transpose()
    }

    fn to_usize(value: u64) -> Result<usize, String> {
        usize::try_from(value).map_err(|error| error.to_string())
    }

    fn number(arguments: &[String], name: &str) -> Result<Option<u64>, String> {
        Self::option(arguments, name)
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|error| format!("invalid {name}: {error}"))
            })
            .transpose()
    }

    fn required<'a>(arguments: &'a [String], name: &str) -> Result<&'a str, String> {
        Self::option(arguments, name).ok_or_else(|| format!("fylo serve requires {name}"))
    }

    fn option<'a>(arguments: &'a [String], name: &str) -> Option<&'a str> {
        arguments
            .iter()
            .position(|argument| argument == name)
            .and_then(|index| arguments.get(index + 1))
            .map(String::as_str)
    }

    fn exit_code(outcome: Result<(), String>) -> ExitCode {
        match outcome {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        }
    }
}
