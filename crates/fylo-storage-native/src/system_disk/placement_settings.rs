/// What a host was told on its command line and in its configuration file
/// about where a root may live (ADR 0032 section 3). The environment is read
/// by [`crate::RootPlacement`] itself, so a library host that sets nothing
/// still gets `FYLO_ALLOW_SYSTEM_DISK` and `FYLO_APP_ID`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlacementSettings {
    /// `--allow-system-disk` was given.
    pub allow_flag: bool,
    /// `fylo.allowSystemDisk` is `true`.
    pub allow_config: bool,
    /// The value of `--app-id`.
    pub app_id_flag: Option<String>,
    /// The value of `fylo.appId`.
    pub app_id_config: Option<String>,
}
