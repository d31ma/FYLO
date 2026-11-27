use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Runs a system tool FYLO asks about disks, with a time limit and one
/// retry (ADR 0032 section 2.2). The program is an absolute path, never
/// looked up in `PATH`, and no argument comes from input.
pub(crate) struct ToolRunner {
    name: &'static str,
    program: PathBuf,
    arguments: &'static [&'static str],
    limits: [Duration; 2],
}

impl ToolRunner {
    /// The path FYLO runs `hdiutil` from; only this one.
    pub(crate) const HDIUTIL: &'static str = "/usr/bin/hdiutil";
    const HDIUTIL_ARGUMENTS: &'static [&'static str] = &["info", "-plist"];
    const FIRST_LIMIT: Duration = Duration::from_secs(5);
    const SECOND_LIMIT: Duration = Duration::from_secs(15);
    const POLL: Duration = Duration::from_millis(10);

    /// `hdiutil info -plist`, 5 seconds, then once more with 15.
    pub(crate) fn hdiutil() -> Self {
        Self::new(
            "hdiutil",
            PathBuf::from(Self::HDIUTIL),
            Self::HDIUTIL_ARGUMENTS,
            [Self::FIRST_LIMIT, Self::SECOND_LIMIT],
        )
    }

    pub(crate) fn new(
        name: &'static str,
        program: PathBuf,
        arguments: &'static [&'static str],
        limits: [Duration; 2],
    ) -> Self {
        Self {
            name,
            program,
            arguments,
            limits,
        }
    }

    /// Run the tool and `parse` its output; on any failure, warn on stderr
    /// and try once more with the longer limit.
    ///
    /// # Errors
    ///
    /// Returns the second failure: a timeout, a failed exit, or output
    /// `parse` refuses.
    pub(crate) fn run<T>(&self, parse: impl Fn(&str) -> Result<T, String>) -> Result<T, String> {
        let first = self.attempt(self.limits[0], &parse);
        first.or_else(|reason| {
            eprintln!(
                "FYLO warning: {} {reason}; trying again (ADR 0032).",
                self.name
            );
            self.attempt(self.limits[1], &parse)
                .map_err(|reason| format!("{} {reason}", self.name))
        })
    }

    fn attempt<T>(
        &self,
        limit: Duration,
        parse: &impl Fn(&str) -> Result<T, String>,
    ) -> Result<T, String> {
        let output = self.output(limit)?;
        parse(&output)
    }

    fn output(&self, limit: Duration) -> Result<String, String> {
        let mut child = Command::new(&self.program)
            .args(self.arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start: {error}"))?;
        let stdout = child.stdout.take();
        let reader = thread::spawn(move || Self::read_all(stdout));
        let status = Self::wait(&mut child, limit);
        let text = reader
            .join()
            .map_err(|_| "output could not be read".to_owned())?;
        Self::checked(status?, text)
    }

    fn read_all(stdout: Option<ChildStdout>) -> String {
        let mut text = String::new();
        if let Some(mut stdout) = stdout {
            let _ = stdout.read_to_string(&mut text);
        }
        text
    }

    /// Wait for `child` up to `limit`; past it, kill the child and reap it.
    fn wait(child: &mut Child, limit: Duration) -> Result<ExitStatus, String> {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {} s", limit.as_secs()));
            }
            thread::sleep(Self::POLL);
        }
    }

    fn checked(status: ExitStatus, text: String) -> Result<String, String> {
        if status.success() {
            return Ok(text);
        }
        Err(format!("exited with {status}"))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::ToolRunner;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    const SHORT: [Duration; 2] = [Duration::from_millis(300), Duration::from_millis(300)];

    fn stand_in(folder: &Path, name: &str, body: &str) -> PathBuf {
        let path = folder.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn runner(program: PathBuf) -> ToolRunner {
        ToolRunner::new("stand-in", program, &[], SHORT)
    }

    fn accept(text: &str) -> Result<String, String> {
        if text.starts_with("good") {
            return Ok(text.trim().to_owned());
        }
        Err("output could not be read".to_owned())
    }

    #[test]
    fn the_production_tool_is_named_by_its_absolute_path() {
        let runner = ToolRunner::hdiutil();
        assert_eq!(runner.program, PathBuf::from("/usr/bin/hdiutil"));
        assert!(runner.program.is_absolute());
        assert_eq!(
            runner.limits,
            [Duration::from_secs(5), Duration::from_secs(15)]
        );
    }

    #[test]
    fn a_good_answer_is_parsed() {
        let folder = crate::system_disk::scratch::Scratch::new("tool-runner");
        let program = stand_in(folder.path(), "tool", "echo good answer");
        assert_eq!(runner(program).run(accept).unwrap(), "good answer");
    }

    #[test]
    fn two_timeouts_fail_and_leave_no_child_running() {
        let folder = crate::system_disk::scratch::Scratch::new("tool-runner");
        let program = stand_in(folder.path(), "tool", "exec sleep 30");
        let started = Instant::now();
        let error = runner(program).run(accept).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_first_failure_is_tried_again() {
        let folder = crate::system_disk::scratch::Scratch::new("tool-runner");
        let marker = folder.path().join("ran-once");
        let body = format!(
            "if [ -e '{0}' ]; then echo good second; else touch '{0}'; exec sleep 30; fi",
            marker.display()
        );
        let program = stand_in(folder.path(), "tool", &body);
        assert_eq!(runner(program).run(accept).unwrap(), "good second");
    }

    #[test]
    fn a_failed_exit_and_unreadable_output_fail_twice() {
        let folder = crate::system_disk::scratch::Scratch::new("tool-runner");
        let failing = stand_in(folder.path(), "failing", "exit 3");
        let error = runner(failing).run(accept).unwrap_err();
        assert!(error.contains("exited with"), "{error}");
        let garbage = stand_in(folder.path(), "garbage", "echo nonsense");
        let error = runner(garbage).run(accept).unwrap_err();
        assert!(error.contains("could not be read"), "{error}");
    }

    #[test]
    fn a_missing_tool_cannot_start() {
        let error = runner(PathBuf::from("/nonexistent/fylo-tool"))
            .run(accept)
            .unwrap_err();
        assert!(error.contains("could not start"), "{error}");
    }

    #[test]
    fn the_parser_runs_once_per_good_answer() {
        let folder = crate::system_disk::scratch::Scratch::new("tool-runner");
        let program = stand_in(folder.path(), "tool", "echo good");
        let calls = AtomicUsize::new(0);
        runner(program)
            .run(|text| {
                calls.fetch_add(1, Ordering::SeqCst);
                accept(text)
            })
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
