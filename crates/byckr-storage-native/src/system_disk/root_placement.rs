use std::sync::{Mutex, OnceLock, PoisonError};

use super::app_id::AppId;
use super::opt_out_source::OptOutSource;
use super::placement_settings::PlacementSettings;

/// Where a root may live in this process (ADR 0032 section 3).
///
/// The CLI resolves it once from its flags, the environment and its
/// configuration file, and installs it. A library host that installs
/// nothing gets the environment alone: `YCKR_ALLOW_SYSTEM_DISK` and
/// `YCKR_APP_ID`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RootPlacement {
    /// The default: a local disk that is not the system disk.
    DataDisk,
    /// The opt-out: any local disk, the system disk included.
    AllowSystemDisk(OptOutSource),
    /// App mode: also the app's own data folder.
    AppOwned(AppId),
}

impl RootPlacement {
    /// The opt-out's environment variable.
    pub const ALLOW_VARIABLE: &'static str = "YCKR_ALLOW_SYSTEM_DISK";
    /// App mode's environment variable.
    pub const APP_ID_VARIABLE: &'static str = "YCKR_APP_ID";

    /// Resolve `settings` and the variables `environment` returns.
    ///
    /// The opt-out is on when the flag, the environment variable or the
    /// configuration key turns it on. The app id comes from the flag, else
    /// the environment variable, else the configuration key. The opt-out
    /// wins over app mode.
    ///
    /// # Errors
    ///
    /// Returns a message for a `YCKR_ALLOW_SYSTEM_DISK` value other than
    /// `1`, `true`, `0` or `false`, and for an invalid app id.
    pub fn resolve(
        settings: &PlacementSettings,
        environment: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let allow_environment = Self::allow_value(environment(Self::ALLOW_VARIABLE).as_deref())?;
        let source = Self::opt_out_source(settings, allow_environment);
        let app_id = Self::app_id(settings, environment(Self::APP_ID_VARIABLE))?;
        Ok(Self::choose(source, app_id))
    }

    /// Resolve `settings` with this process's environment.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve`].
    pub fn from_settings(settings: &PlacementSettings) -> Result<Self, String> {
        Self::resolve(settings, |name| std::env::var(name).ok())
    }

    /// Make this the placement of every root this process opens.
    pub fn install(self) {
        *Self::slot().lock().unwrap_or_else(PoisonError::into_inner) = Some(self);
    }

    /// The installed placement, or the environment's when none is.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve`], for the environment.
    pub fn current() -> Result<Self, String> {
        let installed = Self::slot()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        installed.map_or_else(|| Self::from_settings(&PlacementSettings::default()), Ok)
    }

    fn slot() -> &'static Mutex<Option<Self>> {
        static SLOT: OnceLock<Mutex<Option<RootPlacement>>> = OnceLock::new();
        SLOT.get_or_init(|| Mutex::new(None))
    }

    fn allow_value(value: Option<&str>) -> Result<bool, String> {
        match value {
            None | Some("" | "0" | "false") => Ok(false),
            Some("1" | "true") => Ok(true),
            Some(other) => Err(format!(
                "{} must be 1, true, 0 or false, not {other:?}",
                Self::ALLOW_VARIABLE
            )),
        }
    }

    fn opt_out_source(settings: &PlacementSettings, environment: bool) -> Option<OptOutSource> {
        [
            (settings.allow_flag, OptOutSource::Flag),
            (environment, OptOutSource::Environment),
            (settings.allow_config, OptOutSource::Config),
        ]
        .into_iter()
        .find_map(|(on, source)| on.then_some(source))
    }

    fn app_id(
        settings: &PlacementSettings,
        environment: Option<String>,
    ) -> Result<Option<AppId>, String> {
        let environment = environment.filter(|text| !text.is_empty());
        let text = settings
            .app_id_flag
            .clone()
            .or(environment)
            .or_else(|| settings.app_id_config.clone());
        text.map(|text| AppId::parse(&text)).transpose()
    }

    fn choose(source: Option<OptOutSource>, app_id: Option<AppId>) -> Self {
        if let Some(source) = source {
            return Self::AllowSystemDisk(source);
        }
        app_id.map_or(Self::DataDisk, Self::AppOwned)
    }
}

#[cfg(test)]
mod tests {
    use super::{AppId, OptOutSource, PlacementSettings, RootPlacement};

    fn with(variables: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let variables: Vec<(String, String)> = variables
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |name| {
            variables
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    fn resolve(settings: &PlacementSettings, variables: &[(&str, &str)]) -> RootPlacement {
        RootPlacement::resolve(settings, with(variables)).unwrap()
    }

    #[test]
    fn nothing_set_means_a_data_disk() {
        assert_eq!(
            resolve(&PlacementSettings::default(), &[]),
            RootPlacement::DataDisk
        );
    }

    #[test]
    fn each_form_of_the_opt_out_turns_it_on() {
        let flag = PlacementSettings {
            allow_flag: true,
            ..PlacementSettings::default()
        };
        let config = PlacementSettings {
            allow_config: true,
            ..PlacementSettings::default()
        };
        assert_eq!(
            resolve(&flag, &[]),
            RootPlacement::AllowSystemDisk(OptOutSource::Flag)
        );
        assert_eq!(
            resolve(&config, &[]),
            RootPlacement::AllowSystemDisk(OptOutSource::Config)
        );
        assert_eq!(
            resolve(
                &PlacementSettings::default(),
                &[("YCKR_ALLOW_SYSTEM_DISK", "1")]
            ),
            RootPlacement::AllowSystemDisk(OptOutSource::Environment)
        );
        assert_eq!(
            resolve(
                &PlacementSettings::default(),
                &[("YCKR_ALLOW_SYSTEM_DISK", "true")]
            ),
            RootPlacement::AllowSystemDisk(OptOutSource::Environment)
        );
    }

    #[test]
    fn the_flag_is_named_before_the_environment_and_the_config() {
        let all = PlacementSettings {
            allow_flag: true,
            allow_config: true,
            ..PlacementSettings::default()
        };
        assert_eq!(
            resolve(&all, &[("YCKR_ALLOW_SYSTEM_DISK", "1")]),
            RootPlacement::AllowSystemDisk(OptOutSource::Flag)
        );
    }

    #[test]
    fn off_values_leave_the_opt_out_off() {
        let none = PlacementSettings::default();
        assert_eq!(
            resolve(&none, &[("YCKR_ALLOW_SYSTEM_DISK", "")]),
            RootPlacement::DataDisk
        );
        assert_eq!(
            resolve(&none, &[("YCKR_ALLOW_SYSTEM_DISK", "0")]),
            RootPlacement::DataDisk
        );
        assert_eq!(
            resolve(&none, &[("YCKR_ALLOW_SYSTEM_DISK", "false")]),
            RootPlacement::DataDisk
        );
    }

    #[test]
    fn another_environment_value_is_an_error() {
        let error = RootPlacement::resolve(
            &PlacementSettings::default(),
            with(&[("YCKR_ALLOW_SYSTEM_DISK", "yes")]),
        )
        .unwrap_err();
        assert!(error.contains("YCKR_ALLOW_SYSTEM_DISK must be 1, true, 0 or false"));
    }

    #[test]
    fn the_app_id_flag_wins_over_the_environment_and_the_config() {
        let settings = PlacementSettings {
            app_id_flag: Some("com.example.flag".to_owned()),
            app_id_config: Some("com.example.config".to_owned()),
            ..PlacementSettings::default()
        };
        assert_eq!(
            resolve(&settings, &[("YCKR_APP_ID", "com.example.environment")]),
            RootPlacement::AppOwned(AppId::parse("com.example.flag").unwrap())
        );
        let config = PlacementSettings {
            app_id_config: Some("com.example.config".to_owned()),
            ..PlacementSettings::default()
        };
        assert_eq!(
            resolve(&config, &[("YCKR_APP_ID", "com.example.environment")]),
            RootPlacement::AppOwned(AppId::parse("com.example.environment").unwrap())
        );
        assert_eq!(
            resolve(&config, &[]),
            RootPlacement::AppOwned(AppId::parse("com.example.config").unwrap())
        );
    }

    #[test]
    fn the_opt_out_wins_over_app_mode() {
        let settings = PlacementSettings {
            allow_flag: true,
            app_id_flag: Some("com.example.app".to_owned()),
            ..PlacementSettings::default()
        };
        assert_eq!(
            resolve(&settings, &[]),
            RootPlacement::AllowSystemDisk(OptOutSource::Flag)
        );
    }

    #[test]
    fn an_invalid_app_id_is_an_error() {
        let error = RootPlacement::resolve(
            &PlacementSettings::default(),
            with(&[("YCKR_APP_ID", "../escape")]),
        )
        .unwrap_err();
        assert!(error.contains("app id"));
    }
}
