//! FYLO's extended-attribute calls (ADR 0026): [`ExtendedAttributes`] by
//! path, without following symlinks, and `FileExt` on an open file. On Unix
//! the `xattr` crate provides the calls; iOS, which that crate does not
//! support, uses rustix's functions with the same meaning; Windows has no
//! extended-attribute backend yet, so every call is a no-op.

#[cfg(target_os = "ios")]
mod attribute_file;
#[cfg(target_os = "ios")]
mod attribute_names;
#[cfg(all(unix, not(target_os = "ios")))]
mod extended_attributes;
#[cfg(target_os = "ios")]
#[path = "extended_attributes_ios.rs"]
mod extended_attributes;
#[cfg(windows)]
#[path = "extended_attributes_windows.rs"]
mod extended_attributes;
#[cfg(target_os = "ios")]
mod sized_read;

#[cfg(target_os = "ios")]
pub use attribute_file::FileExt;
#[cfg(target_os = "ios")]
pub use attribute_names::AttributeNames;
pub use extended_attributes::ExtendedAttributes;
#[cfg(all(unix, not(target_os = "ios")))]
pub use xattr::FileExt;
