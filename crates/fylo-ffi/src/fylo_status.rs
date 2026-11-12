/// What a C call did. The JSON in its output buffer says more.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FyloStatus {
    /// The call worked; the output holds its result.
    Ok = 0,
    /// `fylo_get` found no such document; the output is empty.
    NotFound = 1,
    /// FYLO refused or failed the call; the output holds `{"code", "message"}`.
    Error = 2,
    /// A null pointer, text that is not UTF-8, or JSON of the wrong shape.
    InvalidArgument = 3,
    /// The root handle was closed, or never existed.
    Closed = 4,
    /// The engine panicked. The call did nothing more, and its root handle
    /// is dead: close it and open the root again.
    Panic = 5,
}
