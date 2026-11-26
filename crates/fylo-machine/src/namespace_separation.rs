use fylo_storage_native::NativeStorageError;
use fylo_storage_native::layout::NamespaceName;

use crate::MachineError;

/// Whether namespaces keep data apart yet (ADR 0033 section 3).
///
/// Off until slice L1-5 moves the engine onto the layout of ADR 0033. Until
/// then every collection lives in the root's older layout, so a second
/// namespace would see and change the default namespace's data. While it is
/// off, any namespace other than `public` (in a request, as the session's
/// `--namespace`, or in `createNamespace`, `listNamespaces` and
/// `dropNamespace`) is refused with `ENAMESPACEUNSUPPORTED` before anything
/// is written. L1-5 turns it on together with the layout.
pub struct NamespaceSeparation;

impl NamespaceSeparation {
    /// The switch.
    pub const ENABLED: bool = false;

    /// Refuse `namespace` unless it is the default or separation is on.
    ///
    /// # Errors
    ///
    /// `ENAMESPACEUNSUPPORTED` for any other namespace while the switch is
    /// off.
    pub fn require(namespace: &NamespaceName) -> Result<(), NativeStorageError> {
        let default = namespace == &NamespaceName::public();
        if default || Self::enabled() {
            return Ok(());
        }
        Err(NativeStorageError::namespace_unsupported())
    }

    /// Refuse the namespace operations while the switch is off.
    pub(crate) fn require_enabled() -> Result<(), MachineError> {
        if Self::enabled() {
            return Ok(());
        }
        Err(Self::unsupported())
    }

    /// Turn separation on in this process, for the tests of the namespace
    /// code. Only builds with the `test-support` feature have it.
    #[cfg(feature = "test-support")]
    pub fn enable_for_tests() {
        Self::test_override().store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Turn separation off again in this process, for the tests of the
    /// refusal path. Only builds with the `test-support` feature have it.
    #[cfg(feature = "test-support")]
    pub fn disable_for_tests() {
        Self::test_override().store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn enabled() -> bool {
        Self::ENABLED || Self::enabled_by_tests()
    }

    #[cfg(feature = "test-support")]
    fn enabled_by_tests() -> bool {
        Self::test_override().load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(not(feature = "test-support"))]
    fn enabled_by_tests() -> bool {
        false
    }

    #[cfg(feature = "test-support")]
    fn test_override() -> &'static std::sync::atomic::AtomicBool {
        static ENABLED_BY_TESTS: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        &ENABLED_BY_TESTS
    }

    fn unsupported() -> MachineError {
        crate::storage_error(&NativeStorageError::namespace_unsupported())
    }
}
