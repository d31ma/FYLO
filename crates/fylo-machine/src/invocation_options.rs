use std::io;
use std::time::{Duration, Instant};

use fylo_storage_native::RootOpenOptions;

use crate::{ApplicationConfig, FrameLimits};

/// Host configuration for one-frame, cooperative native invocations.
///
/// The deadline is monotonic. Its cleanup reserve leaves time for checked
/// release; neither value can interrupt a blocking filesystem operation.
pub struct InvocationOptions {
    root: RootOpenOptions,
    application: ApplicationConfig,
    limits: FrameLimits,
    deadline: Instant,
    cleanup_reserve: Duration,
}

impl InvocationOptions {
    /// Select an existing physical root without borrowing an external owner.
    ///
    /// # Errors
    /// Rejects external leases, unrelated repositories, and unusable budgets.
    pub fn new(
        root: RootOpenOptions,
        application: ApplicationConfig,
        limits: FrameLimits,
        (deadline, cleanup_reserve): (Instant, Duration),
    ) -> io::Result<Self> {
        Self::require_unleased(&root)?;
        Self::check_work_budget((deadline, cleanup_reserve))?;
        Self::require_bounded_validation(&application)?;
        let root = Self::canonical_root(root)?;
        Ok(Self {
            root,
            application,
            limits: limits.clamped(),
            deadline,
            cleanup_reserve,
        })
    }

    pub(super) fn root(&self) -> &RootOpenOptions {
        &self.root
    }

    pub(super) fn application(&self) -> &ApplicationConfig {
        &self.application
    }

    pub(super) const fn limits(&self) -> FrameLimits {
        self.limits
    }

    pub(super) const fn budget(&self) -> (Instant, Duration) {
        (self.deadline, self.cleanup_reserve)
    }

    pub(super) fn check_work_budget((deadline, reserve): (Instant, Duration)) -> io::Result<()> {
        if reserve.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cleanup reserve must be nonzero",
            ));
        }
        let remaining = deadline.checked_duration_since(Instant::now());
        if remaining.is_none_or(|remaining| remaining <= reserve) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "invocation work budget is exhausted",
            ));
        }
        Ok(())
    }

    pub(super) fn check_completion_deadline(&self) -> io::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "invocation completion deadline is exhausted",
            ));
        }
        Ok(())
    }

    fn require_unleased(root: &RootOpenOptions) -> io::Result<()> {
        if root.owning_lease().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invocations cannot borrow an external root lease",
            ));
        }
        Ok(())
    }

    fn require_bounded_validation(application: &ApplicationConfig) -> io::Result<()> {
        if application.storage.strict_schema {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this invocation slice excludes strict schema validation until external helper deadlines and checked descendant cleanup are qualified",
            ));
        }
        Ok(())
    }

    fn canonical_root(root: RootOpenOptions) -> io::Result<RootOpenOptions> {
        let canonical = fylo_vfs::canonicalize(root.root())?;
        if !fylo_vfs::metadata(&canonical)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invocation root must be an existing directory",
            ));
        }
        if let Some(repository) = root.repository_root() {
            if fylo_vfs::canonicalize(repository)? != canonical {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invocation root and repository must be the same physical directory",
                ));
            }
        }
        Ok(RootOpenOptions::new(&canonical)
            .with_repository(&canonical)
            .with_profile(root.filesystem_profile()))
    }
}
