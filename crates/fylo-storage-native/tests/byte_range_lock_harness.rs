//! Runs the multi-process byte-range lock harness (ADR 0016 section 7) in a
//! short configuration against the local temporary directory.

#![cfg(not(target_family = "wasm"))]

use std::process::Command;

#[test]
fn processes_contending_for_vdbx_lock_bytes_exclude_each_other() {
    let dir = std::env::temp_dir().join(format!("fylo-lock-harness-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create directory");
    let output = Command::new(env!("CARGO_BIN_EXE_fylo-lock-harness"))
        .arg("qualify")
        .arg("--dir")
        .arg(&dir)
        .args(["--procs", "4", "--threads", "2", "--seconds", "2"])
        .output()
        .expect("run harness");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "harness failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("PASSED"), "{stdout}");
}
