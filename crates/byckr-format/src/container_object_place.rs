/// One chunk's place: the extent's position in the object's runs, and the
/// frame's offset in that extent's payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerObjectPlace {
    /// The extent's position in the object, from zero.
    pub ordinal: u64,
    /// The frame's offset in the extent's payload.
    pub offset: u64,
}
