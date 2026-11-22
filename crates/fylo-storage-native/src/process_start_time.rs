use std::io;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, WAIT_EVENT, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    WaitForSingleObject,
};

/// When a running Windows process started, as the number lock records
/// carry: the .NET ticks of WMI's `Win32_Process.CreationDate` in UTC. Asking
/// WMI starts PowerShell, which took 1 to 10 seconds on a Windows VM and made
/// the first write of every FYLO process slow, so the time is read from the
/// process handle instead. WMI keeps whole microseconds, so the ticks are cut
/// to whole tens and stay equal to what older FYLO versions wrote. A process
/// that has exited reads as gone, as in WMI, even while another process still
/// holds a handle to it. Rust's standard library has no process times. The
/// unsafe function has `SAFETY` comments and an entry in
/// `docs/security/unsafe-inventory.md` (ADR 0013 amendment 1).
pub(crate) struct ProcessStartTime;

impl ProcessStartTime {
    /// .NET ticks (100 ns from 0001-01-01) at the `FILETIME` start, 1601-01-01.
    const TICKS_AT_FILETIME_START: u64 = 504_911_232_000_000_000;
    const TICKS_PER_MICROSECOND: u64 = 10;
    const BITS_IN_LOW_PART: u32 = 32;
    const NO_WAIT: u32 = 0;

    /// The start ticks of the running process `pid`, `Ok(None)` when no
    /// such process runs, or an error when Windows will not say (for example,
    /// access is denied), so the caller can ask another way and never reads a
    /// live process as gone.
    ///
    /// # Errors
    ///
    /// Returns the Windows error of `OpenProcess`, `WaitForSingleObject` or
    /// `GetProcessTimes`.
    pub(crate) fn ticks(pid: u32) -> io::Result<Option<u64>> {
        let created = Self::creation(pid)?;
        Ok(created.map(Self::ticks_from_filetime))
    }

    fn ticks_from_filetime(filetime: u64) -> u64 {
        let whole_microseconds = filetime / Self::TICKS_PER_MICROSECOND;
        whole_microseconds * Self::TICKS_PER_MICROSECOND + Self::TICKS_AT_FILETIME_START
    }

    /// The only unsafe function of this file (ADR 0013 amendment 1): open
    /// the process, ask whether it has exited, read its times, close it.
    #[allow(
        unsafe_code,
        reason = "ADR 0013 amendment 1: OpenProcess, WaitForSingleObject, GetProcessTimes and CloseHandle on a handle this function owns"
    )]
    fn creation(pid: u32) -> io::Result<Option<u64>> {
        let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
        // SAFETY: `OpenProcess` takes no pointers; a null result is an error
        // and is never used as a handle.
        let process: HANDLE = unsafe { OpenProcess(access, 0, pid) };
        if process.is_null() {
            return Self::not_opened(io::Error::last_os_error());
        }
        // SAFETY: `process` is the open handle from above, with synchronize
        // rights; a zero timeout returns at once and keeps no pointer.
        let state = unsafe { WaitForSingleObject(process, Self::NO_WAIT) };
        let mut created = Self::empty_time();
        let mut exited = Self::empty_time();
        let mut kernel = Self::empty_time();
        let mut user = Self::empty_time();
        // SAFETY: `process` is the open handle from above, with query rights,
        // and outlives the call; the four outputs are initialized, exclusively
        // borrowed `FILETIME` structs the call writes and does not keep.
        let done = unsafe {
            GetProcessTimes(
                process,
                &raw mut created,
                &raw mut exited,
                &raw mut kernel,
                &raw mut user,
            )
        };
        let error = io::Error::last_os_error();
        // SAFETY: `process` was opened above, is closed exactly once, and
        // nothing uses it afterwards.
        unsafe { CloseHandle(process) };
        Self::running_start(state, done, created, error)
    }

    fn not_opened(error: io::Error) -> io::Result<Option<u64>> {
        let gone = error.raw_os_error() == i32::try_from(ERROR_INVALID_PARAMETER).ok();
        if gone {
            return Ok(None);
        }
        Err(error)
    }

    fn running_start(
        state: WAIT_EVENT,
        done: i32,
        created: FILETIME,
        error: io::Error,
    ) -> io::Result<Option<u64>> {
        if state == WAIT_OBJECT_0 {
            return Ok(None);
        }
        if state != WAIT_TIMEOUT || done == 0 {
            return Err(error);
        }
        Ok(Some(Self::filetime_value(created)))
    }

    fn empty_time() -> FILETIME {
        FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        }
    }

    fn filetime_value(time: FILETIME) -> u64 {
        (u64::from(time.dwHighDateTime) << Self::BITS_IN_LOW_PART) | u64::from(time.dwLowDateTime)
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::ProcessStartTime;

    const QUICK: Duration = Duration::from_millis(200);
    const NO_SUCH_PROCESS: u32 = 0xFFFF_FFF0;
    const SYSTEM_PROCESS: u32 = 4;

    fn wmi_ticks(pid: u32) -> String {
        let command = format!(
            "(Get-CimInstance Win32_Process -Filter 'ProcessId = {pid}').CreationDate.ToUniversalTime().Ticks"
        );
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &command])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn short_child() -> Child {
        Command::new("cmd.exe")
            .args(["/C", "exit"])
            .spawn()
            .unwrap()
    }

    fn quick_answer(pid: u32) -> Option<u64> {
        let started = Instant::now();
        let answer = ProcessStartTime::ticks(pid).unwrap();
        assert!(started.elapsed() < QUICK, "took {:?}", started.elapsed());
        answer
    }

    #[test]
    fn this_process_start_equals_the_value_wmi_gives() {
        let pid = std::process::id();
        let ticks = ProcessStartTime::ticks(pid).unwrap().unwrap();
        assert_eq!(ticks.to_string(), wmi_ticks(pid));
    }

    #[test]
    fn another_running_process_start_equals_the_value_wmi_gives() {
        let mut child = Command::new("ping.exe")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let ticks = ProcessStartTime::ticks(child.id()).unwrap().unwrap();
        let wmi = wmi_ticks(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(ticks.to_string(), wmi);
    }

    #[test]
    fn the_system_process_never_reads_as_gone() {
        let answer = ProcessStartTime::ticks(SYSTEM_PROCESS);
        assert!(!matches!(answer, Ok(None)), "{answer:?}");
    }

    #[test]
    fn a_missing_process_reads_as_gone() {
        assert_eq!(quick_answer(NO_SUCH_PROCESS), None);
    }

    #[test]
    fn an_ended_process_reads_as_gone_while_its_handle_is_held() {
        let mut child = short_child();
        child.wait().unwrap();
        assert_eq!(quick_answer(child.id()), None);
    }

    #[test]
    fn an_ended_process_reads_as_gone_after_its_handle_is_closed() {
        let mut child = short_child();
        child.wait().unwrap();
        let pid = child.id();
        drop(child);
        assert_eq!(quick_answer(pid), None);
    }

    #[test]
    fn this_process_start_is_read_without_starting_a_process() {
        assert!(quick_answer(std::process::id()).is_some());
    }

    #[test]
    fn ticks_keep_whole_microseconds_from_the_filetime_start() {
        assert_eq!(
            ProcessStartTime::ticks_from_filetime(134_397_814_691_837_576),
            639_309_046_691_837_570
        );
    }
}
