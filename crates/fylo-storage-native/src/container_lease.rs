//! The single-writer lease for a `.fxdb` file on a filesystem whose
//! byte-range locks are unqualified or refused (ADR 0016 section 7).
//!
//! It needs nothing from the filesystem but exclusive create, unlink and a
//! directory listing, which every filesystem FYLO targets provides. Leases are
//! numbered files beside the database, `<name>.fxdb.lease.<generation>`, and
//! the highest generation is the owner. Taking over a crashed owner's lease
//! is creating the next generation, which exclusive create grants to exactly
//! one contender, so no two contenders can both win and no rename or delete
//! has to be raced.
//!
//! An owner proves it is alive by rewriting its lease, with a counter that
//! changes, more often than [`ContainerLeaseTiming::takeover_after`]. A
//! contender takes over only after seeing the lease bytes unchanged for that
//! long by its own monotonic clock, so hosts need not agree on the time. The
//! winner deletes the older generations; an owner that finds its file gone,
//! or a higher generation, has lost the lease and must stop writing.
//!
//! Releasing marks the lease free rather than deleting it, so generations
//! only grow: a contender that decided on generation `n + 1` long ago can
//! never create it after a later owner restarted the count.
//!
//! What it cannot do is stop an owner that is paused between checking the
//! lease and writing, then resumes after a takeover. The owner narrows that
//! window by checking before every commit and renewing well inside the
//! takeover time; `fylo serve` on a qualified filesystem closes it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SUFFIX: &str = ".lease.";
const FORMAT: &str = "fylo.fxdb-lease.v1";

/// How long a lease may go unrenewed before another process takes it over.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerLeaseTiming {
    /// A lease whose bytes stay unchanged this long is taken over. The owner
    /// renews at most a third of this apart.
    pub takeover_after: Duration,
    /// How often a waiting contender looks again.
    pub poll: Duration,
}

impl Default for ContainerLeaseTiming {
    fn default() -> Self {
        Self {
            takeover_after: Duration::from_secs(30),
            poll: Duration::from_millis(250),
        }
    }
}

/// Why the lease was not taken or kept.
#[derive(Debug)]
pub enum ContainerLeaseError {
    /// A live owner holds generation `generation`.
    Busy {
        /// The live owner's generation.
        generation: u64,
    },
    /// Another process took the lease over; stop writing.
    Lost,
    /// The filesystem refused an operation.
    Io(io::Error),
}

impl std::fmt::Display for ContainerLeaseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy { generation } => write!(
                formatter,
                "the database is held by another writer (lease generation {generation})"
            ),
            Self::Lost => formatter.write_str("the database lease was taken over"),
            Self::Io(error) => write!(formatter, "database lease I/O failed: {error}"),
        }
    }
}

impl std::error::Error for ContainerLeaseError {}

impl From<io::Error> for ContainerLeaseError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// A held single-writer lease. Dropping it deletes the lease file if this
/// process still owns it.
#[derive(Debug)]
pub struct ContainerLease {
    directory: PathBuf,
    prefix: String,
    generation: u64,
    owner: String,
    beat: u64,
    file: File,
    released: bool,
}

impl ContainerLease {
    /// Take the lease for the database at `database`, waiting up to `wait`.
    ///
    /// A live owner keeps it; a lease left unchanged for
    /// [`ContainerLeaseTiming::takeover_after`] is taken over, so taking over
    /// a crashed owner's lease needs a `wait` at least that long.
    ///
    /// # Errors
    ///
    /// Returns `Busy` when a live owner still holds it at the deadline, or a
    /// filesystem failure.
    pub fn acquire(
        database: &Path,
        timing: ContainerLeaseTiming,
        wait: Duration,
    ) -> Result<Self, ContainerLeaseError> {
        let (directory, prefix) = lease_location(database)?;
        let deadline = Instant::now() + wait;
        let mut observed: Option<(u64, Vec<u8>, Instant)> = None;
        loop {
            let newest = generations(&directory, &prefix)?.last().copied();
            let next = match newest {
                None => Some(1),
                Some(generation) => {
                    match fs::read(directory.join(format!("{prefix}{generation}"))) {
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.into()),
                        Ok(bytes) if is_free(&bytes) => Some(generation + 1),
                        Ok(bytes) => {
                            let now = Instant::now();
                            match &observed {
                                Some((seen, seen_bytes, since))
                                    if *seen == generation && *seen_bytes == bytes =>
                                {
                                    (now.duration_since(*since) >= timing.takeover_after)
                                        .then_some(generation + 1)
                                }
                                _ => {
                                    observed = Some((generation, bytes, now));
                                    None
                                }
                            }
                        }
                    }
                }
            };
            if let Some(generation) = next {
                if let Some(lease) = Self::create(&directory, &prefix, generation)? {
                    return Ok(lease);
                }
                observed = None;
                continue;
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(ContainerLeaseError::Busy {
                    generation: newest.unwrap_or(0),
                });
            }
            std::thread::sleep(timing.poll.min(deadline - now));
        }
    }

    fn create(
        directory: &Path,
        prefix: &str,
        generation: u64,
    ) -> Result<Option<Self>, ContainerLeaseError> {
        let path = directory.join(format!("{prefix}{generation}"));
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut owner = [0_u8; 16];
        getrandom::fill(&mut owner).map_err(|error| io::Error::other(error.to_string()))?;
        let mut lease = Self {
            directory: directory.to_path_buf(),
            prefix: prefix.to_owned(),
            generation,
            owner: format!("{:032x}", u128::from_le_bytes(owner)),
            beat: 0,
            file,
            released: false,
        };
        lease.write_state(HELD)?;
        for older in generations(directory, prefix)? {
            if older < generation {
                remove_if_present(&directory.join(format!("{prefix}{older}")))?;
            }
        }
        Ok(Some(lease))
    }

    /// This lease's generation; a takeover always raises it.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Check that this process still owns the lease. Call it before every
    /// commit.
    ///
    /// # Errors
    ///
    /// Returns `Lost` after a takeover, or a filesystem failure.
    pub fn assert_owned(&self) -> Result<(), ContainerLeaseError> {
        let generations = generations(&self.directory, &self.prefix)?;
        if generations.last() == Some(&self.generation) {
            Ok(())
        } else {
            Err(ContainerLeaseError::Lost)
        }
    }

    /// Prove this owner is alive. Call it at most a third of
    /// [`ContainerLeaseTiming::takeover_after`] apart.
    ///
    /// # Errors
    ///
    /// Returns `Lost` after a takeover, or a filesystem failure.
    pub fn renew(&mut self) -> Result<(), ContainerLeaseError> {
        self.assert_owned()?;
        self.beat += 1;
        self.write_state(HELD)
    }

    fn write_state(&mut self, state: &str) -> Result<(), ContainerLeaseError> {
        // Fixed width, so a rewrite never leaves a longer old tail behind.
        let body = format!(
            "{{\"format\":\"{FORMAT}\",\"state\":\"{state}\",\"generation\":{:20},\"owner\":\"{}\",\"pid\":{:10},\"beat\":{:20}}}\n",
            self.generation,
            self.owner,
            std::process::id(),
            self.beat,
        );
        let mut file = &self.file;
        io::Seek::seek(&mut file, io::SeekFrom::Start(0))?;
        file.write_all(body.as_bytes())?;
        // Other clients of a network filesystem see the beat only once the
        // server has it.
        self.file.sync_data()?;
        Ok(())
    }

    /// Give the lease up: mark it free, so the next writer takes it at once.
    ///
    /// # Errors
    ///
    /// Returns the write failure.
    pub fn release(mut self) -> Result<(), ContainerLeaseError> {
        self.released = true;
        if self.assert_owned().is_ok() {
            self.write_state(FREE)?;
        }
        Ok(())
    }
}

impl Drop for ContainerLease {
    fn drop(&mut self) {
        if !self.released && self.assert_owned().is_ok() {
            let _ = self.write_state(FREE);
        }
    }
}

const HELD: &str = "held";
const FREE: &str = "free";

fn is_free(bytes: &[u8]) -> bool {
    let marker = format!("\"state\":\"{FREE}\"");
    bytes
        .windows(marker.len())
        .any(|window| window == marker.as_bytes())
}

fn lease_location(database: &Path) -> io::Result<(PathBuf, String)> {
    let name = database
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "database path has no UTF-8 file name",
            )
        })?;
    let directory = match database.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    Ok((directory, format!("{name}{SUFFIX}")))
}

/// The lease generations present, lowest first.
fn generations(directory: &Path, prefix: &str) -> io::Result<Vec<u64>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if let Some(generation) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix(prefix))
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|digits| digits.parse::<u64>().ok())
        {
            found.push(generation);
        }
    }
    found.sort_unstable();
    Ok(found)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
