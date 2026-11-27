use crate::NativeCollection;
use crate::write::DocumentWrite;

/// One collection of a namespace transaction: its position in the request,
/// its writes, and their canonical bodies (`bodies[i]` for `writes[i]`,
/// `None` for a delete).
pub(crate) struct NamespaceMember<'a> {
    pub(crate) position: usize,
    pub(crate) collection: NativeCollection,
    pub(crate) writes: &'a [DocumentWrite],
    pub(crate) bodies: Vec<Option<Vec<u8>>>,
}
