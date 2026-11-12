use std::time::{Duration, Instant};

/// Growing pauses between tries of a busy lock, never past a deadline.
pub(super) struct LeaseBackoff {
    pause: Duration,
    wait_until: Instant,
}

impl LeaseBackoff {
    const FIRST_PAUSE: Duration = Duration::from_millis(10);
    const LONGEST_PAUSE: Duration = Duration::from_millis(50);

    pub(super) fn until(wait_until: Instant) -> Self {
        Self {
            pause: Self::FIRST_PAUSE,
            wait_until,
        }
    }

    /// Sleep before the next try. False, without sleeping, once the
    /// deadline has passed.
    pub(super) fn wait(&mut self) -> bool {
        let remaining = self.remaining();
        remaining.map(|remaining| self.sleep(remaining)).is_some()
    }

    fn remaining(&self) -> Option<Duration> {
        self.wait_until
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
    }

    fn sleep(&mut self, remaining: Duration) {
        std::thread::sleep(self.pause.min(remaining));
        self.pause = (self.pause * 2).min(Self::LONGEST_PAUSE);
    }
}
