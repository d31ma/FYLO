//! The warning a read prints, once per process, when it finds a record that
//! still carries a POSTIX access descriptor (ADR 0021).

use std::collections::BTreeMap;
use std::sync::Once;

/// Tells the user, once per process, that a root holds records with POSTIX
/// access descriptors, which FYLO reads but no longer enforces.
pub(crate) struct LegacyAccessWarning;

impl LegacyAccessWarning {
    const MESSAGE: &'static str = "fylo: warning: this root holds records with POSTIX access \
        descriptors (uid, gid, mode), which FYLO no longer enforces; list them with \
        `fylo access report <root>` and move the root into a protected database with \
        `fylo pack --protect` (docs/migrating-to-protected-databases.md)";

    /// Print the warning if `attributes` hold an access descriptor and no
    /// read of this process has printed it yet.
    pub(crate) fn notice<V>(attributes: &BTreeMap<String, V>) {
        Self::print_once(attributes.contains_key(crate::ACCESS_XATTR));
    }

    fn print_once(found: bool) {
        static PRINTED: Once = Once::new();
        if found {
            PRINTED.call_once(|| eprintln!("{}", Self::MESSAGE));
        }
    }
}
