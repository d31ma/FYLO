use crate::{NativeStorageError, NativeStorageErrorCode};
use std::time::Duration;

/// How often an owner proves it is alive, and how long a process that finds
/// another owner's record waits for that proof before it takes over
/// (ADR 0033 section 4).
///
/// The wait is measured on the waiting process's own clock, so no clock is
/// ever compared across machines.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LeaseTiming {
    heartbeat: Duration,
    takeover: Duration,
}

impl LeaseTiming {
    const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(5);
    const DEFAULT_TAKEOVER: Duration = Duration::from_secs(20);
    const SHORTEST_HEARTBEAT: Duration = Duration::from_millis(10);
    const LONGEST_TAKEOVER: Duration = Duration::from_hours(24);
    const HEARTBEAT_VARIABLE: &str = "FYLO_LEASE_HEARTBEAT_SECONDS";
    const TAKEOVER_VARIABLE: &str = "FYLO_LEASE_TAKEOVER_SECONDS";

    /// # Errors
    ///
    /// Refuses a heartbeat under 10 ms, a takeover time over one day, and a
    /// takeover time that is not longer than the heartbeat: an owner must
    /// be able to beat at least once before anyone may take over.
    pub fn new(heartbeat: Duration, takeover: Duration) -> Result<Self, NativeStorageError> {
        Self::require_heartbeat_floor(heartbeat)?;
        Self::require_takeover_ceiling(takeover)?;
        Self::require_takeover_after_heartbeat(heartbeat, takeover)?;
        Ok(Self {
            heartbeat,
            takeover,
        })
    }

    /// The timing from `FYLO_LEASE_HEARTBEAT_SECONDS` and
    /// `FYLO_LEASE_TAKEOVER_SECONDS` (decimal seconds), each falling back to
    /// its default of 5 and 20 seconds.
    ///
    /// # Errors
    ///
    /// Refuses a value that is not a number of seconds, and the timings
    /// [`LeaseTiming::new`] refuses.
    pub fn from_environment() -> Result<Self, NativeStorageError> {
        let heartbeat = Self::read_variable(Self::HEARTBEAT_VARIABLE, Self::DEFAULT_HEARTBEAT)?;
        let takeover = Self::read_variable(Self::TAKEOVER_VARIABLE, Self::DEFAULT_TAKEOVER)?;
        Self::new(heartbeat, takeover)
    }

    #[must_use]
    pub fn heartbeat(&self) -> Duration {
        self.heartbeat
    }

    #[must_use]
    pub fn takeover(&self) -> Duration {
        self.takeover
    }

    /// The longer of this process's takeover time and the one the owner
    /// recorded, so a process with a short setting never takes over from an
    /// owner that beats more slowly than that setting expects.
    pub(super) fn takeover_against(&self, recorded_millis: u64) -> Duration {
        self.takeover.max(Duration::from_millis(recorded_millis))
    }

    fn read_variable(name: &str, default: Duration) -> Result<Duration, NativeStorageError> {
        let Ok(text) = std::env::var(name) else {
            return Ok(default);
        };
        Self::parse_seconds(name, &text)
    }

    fn parse_seconds(name: &str, text: &str) -> Result<Duration, NativeStorageError> {
        let seconds = text.trim().parse::<f64>().ok();
        let finite = seconds.filter(|value| value.is_finite() && *value > 0.0);
        let duration = finite.and_then(|value| Duration::try_from_secs_f64(value).ok());
        duration.ok_or_else(|| {
            Self::invalid(format!(
                "{name} must be a positive number of seconds, not {text:?}"
            ))
        })
    }

    fn require_heartbeat_floor(heartbeat: Duration) -> Result<(), NativeStorageError> {
        if heartbeat < Self::SHORTEST_HEARTBEAT {
            return Err(Self::invalid("the lease heartbeat must be at least 10 ms"));
        }
        Ok(())
    }

    fn require_takeover_ceiling(takeover: Duration) -> Result<(), NativeStorageError> {
        if takeover > Self::LONGEST_TAKEOVER {
            return Err(Self::invalid(
                "the lease takeover time must be at most one day",
            ));
        }
        Ok(())
    }

    fn require_takeover_after_heartbeat(
        heartbeat: Duration,
        takeover: Duration,
    ) -> Result<(), NativeStorageError> {
        if takeover <= heartbeat {
            return Err(Self::invalid(
                "the lease takeover time must be longer than the heartbeat",
            ));
        }
        Ok(())
    }

    fn invalid(message: impl Into<String>) -> NativeStorageError {
        NativeStorageError::new(NativeStorageErrorCode::Unsupported, message)
    }
}

impl Default for LeaseTiming {
    fn default() -> Self {
        Self {
            heartbeat: Self::DEFAULT_HEARTBEAT,
            takeover: Self::DEFAULT_TAKEOVER,
        }
    }
}
