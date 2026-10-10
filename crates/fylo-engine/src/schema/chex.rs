use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::Value;

use super::validation_request::ValidationRequest;

pub(super) struct Chex {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Chex {
    const MAXIMUM_RESPONSE_BYTES: u64 = 16_777_216;
    const EMPTY_RESPONSE_BYTES: usize = 0;

    pub(super) fn spawn(binary: &Path) -> Result<Self, String> {
        let mut child = Self::child(binary)?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| "CHEX stdin is unavailable".to_owned())?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| "CHEX stdout is unavailable".to_owned())?;
        Ok(Self {
            child,
            input,
            output: BufReader::new(output),
        })
    }

    pub(super) fn request(&mut self, request: &ValidationRequest<'_>) -> Result<Value, String> {
        let encoded = Self::encode(request)?;
        self.write(&encoded)?;
        let line = self.read()?;
        Self::decode(line)
    }

    fn child(binary: &Path) -> Result<Child, String> {
        // A just-written executable can briefly be busy when another thread
        // forks while its write handle is still open (ETXTBSY); that clears
        // as soon as the forked child execs or exits.
        let mut attempt = 0;
        loop {
            match Self::start(binary) {
                Err(error)
                    if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 20 =>
                {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                outcome => {
                    return outcome.map_err(|error| {
                        format!(
                            "cannot start the CHEX validator ({}): {error}",
                            binary.display()
                        )
                    });
                }
            }
        }
    }

    fn start(binary: &Path) -> std::io::Result<Child> {
        let mut command = Command::new(binary);
        // Its own process group, so teardown can reach any process it starts.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        command
            .args(["exec", "--loop"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
    }

    fn encode(request: &ValidationRequest<'_>) -> Result<String, String> {
        serde_json::to_string(request).map_err(|error| format!("invalid request: {error}"))
    }

    fn write(&mut self, encoded: &str) -> Result<(), String> {
        writeln!(self.input, "{encoded}")
            .and_then(|()| self.input.flush())
            .map_err(|error| format!("cannot write to the CHEX validator: {error}"))
    }

    fn read(&mut self) -> Result<Vec<u8>, String> {
        let mut line = Vec::new();
        let read = std::io::Read::take(&mut self.output, Self::MAXIMUM_RESPONSE_BYTES)
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("cannot read from the CHEX validator: {error}"))?;
        Self::require_response(read)?;
        Ok(line)
    }

    fn require_response(length: usize) -> Result<(), String> {
        if length == Self::EMPTY_RESPONSE_BYTES {
            return Err("the CHEX validator exited".into());
        }
        Ok(())
    }

    fn decode(line: Vec<u8>) -> Result<Value, String> {
        let line =
            String::from_utf8(line).map_err(|_| "CHEX response is not valid UTF-8".to_owned())?;
        serde_json::from_str(line.trim())
            .map_err(|error| format!("CHEX response is not valid JSON: {error}"))
    }
}

impl Drop for Chex {
    fn drop(&mut self) {
        // A validator's own children would otherwise outlive it and keep the
        // inherited pipes open. End the whole tree, then reap the validator.
        #[cfg(unix)]
        if let Some(group) = i32::try_from(self.child.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        // Windows has no process groups to signal; `taskkill /T` ends the tree.
        #[cfg(windows)]
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &self.child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
