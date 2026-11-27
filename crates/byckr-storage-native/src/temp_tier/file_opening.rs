/// How [`PrivateFolder`] opens a file below it.
#[derive(Clone, Copy)]
pub(crate) enum FileOpening {
    Read,
    CreateNew,
    Replace,
}
