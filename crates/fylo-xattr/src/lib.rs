//! FYLO's extended-attribute calls (ADR 0026): [`ExtendedAttributes`] by
//! path, without following symlinks, and [`FileExt`] on an open file. Every
//! system but iOS uses the `xattr` crate; iOS, which that crate does not
//! support, uses rustix's functions with the same meaning.

#[cfg(target_os = "ios")]
mod attribute_file;
#[cfg(target_os = "ios")]
mod attribute_names;
#[cfg(not(target_os = "ios"))]
mod extended_attributes;
#[cfg(target_os = "ios")]
#[path = "extended_attributes_ios.rs"]
mod extended_attributes;
#[cfg(target_os = "ios")]
mod sized_read;

#[cfg(target_os = "ios")]
pub use attribute_file::FileExt;
#[cfg(target_os = "ios")]
pub use attribute_names::AttributeNames;
pub use extended_attributes::ExtendedAttributes;
#[cfg(not(target_os = "ios"))]
pub use xattr::FileExt;
