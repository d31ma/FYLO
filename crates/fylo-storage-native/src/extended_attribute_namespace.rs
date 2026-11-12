use std::ffi::OsStr;

/// Tells FYLO's extended attributes apart from the ones the kernel keeps.
///
/// Linux and Android list the kernel's own attributes, such as
/// `security.selinux`, next to a file's `user.` ones. An app cannot remove
/// or set them, and they are not part of a record, so a before-image and a
/// copy leave them alone. Other systems have no such namespaces.
pub(crate) struct ExtendedAttributeNamespace;

impl ExtendedAttributeNamespace {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    const KERNEL_PREFIXES: [&'static str; 3] = ["security.", "system.", "trusted."];

    /// Whether `name` belongs to the kernel rather than to the file's owner.
    pub(crate) fn belongs_to_the_kernel(name: &OsStr) -> bool {
        Self::has_kernel_prefix(&name.to_string_lossy())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn has_kernel_prefix(name: &str) -> bool {
        Self::KERNEL_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn has_kernel_prefix(_name: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::ExtendedAttributeNamespace;

    #[test]
    fn fylo_attributes_belong_to_the_owner() {
        assert!(!ExtendedAttributeNamespace::belongs_to_the_kernel(
            OsStr::new("user.fylo.key")
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn security_system_and_trusted_names_belong_to_the_kernel() {
        assert!(ExtendedAttributeNamespace::belongs_to_the_kernel(
            OsStr::new("security.selinux")
        ));
        assert!(ExtendedAttributeNamespace::belongs_to_the_kernel(
            OsStr::new("system.posix_acl_access")
        ));
        assert!(ExtendedAttributeNamespace::belongs_to_the_kernel(
            OsStr::new("trusted.overlay.opaque")
        ));
    }
}
