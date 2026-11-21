use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// A stable name for the machine this process runs on, written into a lease
/// record so a later process can tell that a dead owner ran here.
///
/// Only when the record names this machine may a process ask whether the
/// owner's pid is still alive; a pid on another machine means nothing here.
/// The name is a hash, so the record does not carry the raw hardware id.
///
/// Every part makes the name stricter, never looser: a clone that shares a
/// machine id but has another host name or process namespace gets another
/// name, and then waits the takeover time instead of taking over at once.
/// A platform without a stable id (iOS, the browser) has no name, so its
/// processes always wait.
pub(super) struct MachineIdentity;

impl MachineIdentity {
    const HASH_PREFIX: &str = "fylo-machine-v1\n";
    const NAME_BYTES: usize = 16;
    #[cfg(feature = "test-support")]
    const TEST_VARIABLE: &str = "FYLO_TEST_LEASE_MACHINE";

    pub(super) fn current() -> Option<String> {
        static NAME: OnceLock<Option<String>> = OnceLock::new();
        NAME.get_or_init(Self::compute).clone()
    }

    fn compute() -> Option<String> {
        let parts = Self::parts()?;
        Some(Self::hash(&parts))
    }

    #[cfg(feature = "test-support")]
    fn parts() -> Option<String> {
        std::env::var(Self::TEST_VARIABLE)
            .ok()
            .or_else(Self::platform_parts)
    }

    #[cfg(not(feature = "test-support"))]
    fn parts() -> Option<String> {
        Self::platform_parts()
    }

    fn hash(parts: &str) -> String {
        let digest = Sha256::digest(format!("{}{parts}", Self::HASH_PREFIX).as_bytes());
        crate::hex_bytes(&digest[..Self::NAME_BYTES])
    }

    #[cfg(target_os = "macos")]
    fn platform_parts() -> Option<String> {
        let output = Self::command_output("ioreg", &["-rd1", "-c", "IOPlatformExpertDevice"])?;
        let line = output
            .lines()
            .find(|line| line.contains("\"IOPlatformUUID\""))?;
        let uuid = line.rsplit('"').nth(1)?;
        Some(format!("macos:{uuid}"))
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn platform_parts() -> Option<String> {
        let machine = Self::linux_machine_id()?;
        let namespace = std::fs::read_link("/proc/self/ns/pid").ok()?;
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname").ok()?;
        Some(format!(
            "linux:{machine}:{}:{}",
            namespace.display(),
            host.trim()
        ))
    }

    /// Android has no `/etc/machine-id`, so it uses the boot id: a process
    /// that crashed during this boot is still recognised, and after a
    /// reboot the next owner waits the takeover time once.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn linux_machine_id() -> Option<String> {
        [
            "/etc/machine-id",
            "/var/lib/dbus/machine-id",
            "/proc/sys/kernel/random/boot_id",
        ]
        .iter()
        .find_map(|path| Self::non_empty_file(path))
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn non_empty_file(path: &str) -> Option<String> {
        let text = std::fs::read_to_string(path).ok()?;
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }

    #[cfg(target_os = "windows")]
    fn platform_parts() -> Option<String> {
        let output = Self::command_output(
            "reg.exe",
            &[
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
            ],
        )?;
        let line = output.lines().find(|line| line.contains("MachineGuid"))?;
        let guid = line.split_whitespace().last()?;
        let computer = std::env::var("COMPUTERNAME").ok()?;
        Some(format!("windows:{guid}:{computer}"))
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "android",
        target_os = "windows"
    )))]
    fn platform_parts() -> Option<String> {
        None
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn command_output(program: &str, arguments: &[&str]) -> Option<String> {
        let output = std::process::Command::new(program)
            .args(arguments)
            .output()
            .ok()?;
        let text = String::from_utf8(output.stdout).ok()?;
        output.status.success().then_some(text)
    }
}
