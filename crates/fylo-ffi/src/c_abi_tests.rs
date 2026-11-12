//! A panic inside a call (ADR 0026, D6): it returns `FYLO_PANIC`, the handle
//! refuses every later call, and closing still frees it.

use std::path::PathBuf;

use super::CAbi;
use crate::fylo_bytes::FyloBytes;
use crate::fylo_status::FyloStatus;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let directory = std::env::temp_dir().join(format!(
            "fylo-ffi-panic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        Scratch(directory)
    }

    fn database(&self) -> String {
        self.0.join("panic.vdbx").to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open_plain(path: &str) -> u64 {
    let options = r#"{"create": {"mode": "plain"}}"#;
    let mut root = 0;
    // SAFETY: every pointer addresses its length for the call.
    let status = unsafe {
        CAbi::fylo_open(
            path.as_ptr(),
            path.len(),
            options.as_ptr(),
            options.len(),
            std::ptr::null(),
            0,
            &raw mut root,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(status, FyloStatus::Ok);
    root
}

fn create_collection(root: u64) -> FyloStatus {
    let name = "notes";
    let mut out = FyloBytes::empty();
    // SAFETY: `name` addresses its length; `out` is writable.
    let status =
        unsafe { CAbi::fylo_create_collection(root, name.as_ptr(), name.len(), &raw mut out) };
    // SAFETY: `out` came from the call above and is freed once.
    unsafe { CAbi::fylo_bytes_free(out) };
    status
}

fn close(root: u64) -> FyloStatus {
    // SAFETY: a null error slot is ignored.
    unsafe { CAbi::fylo_close(root, std::ptr::null_mut()) }
}

#[test]
fn a_panic_returns_fylo_panic_and_kills_only_its_handle() {
    let scratch = Scratch::new();
    let other = Scratch::new();
    let root = open_plain(&scratch.database());
    let bystander = open_plain(&other.database());

    let outcome = CAbi::root_call(root, || panic!("a test panic inside a call"));

    assert_eq!(outcome.status(), FyloStatus::Panic);
    assert_eq!(create_collection(root), FyloStatus::Panic);
    assert_eq!(create_collection(bystander), FyloStatus::Ok);
    assert_eq!(close(root), FyloStatus::Ok);
    assert_eq!(create_collection(root), FyloStatus::Closed);
    assert_eq!(close(bystander), FyloStatus::Ok);
}
