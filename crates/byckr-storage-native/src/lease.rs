mod heartbeat_target;
mod kernel_file_lock;
mod lease_backoff;
mod lease_claims;
mod lease_failpoint;
mod lease_heartbeat;
mod lease_observation;
mod lease_record;
mod lease_record_file;
mod lease_state;
mod lease_takeover;
mod lease_target;
mod lease_timing;
mod machine_identity;
mod root_lease;
mod root_lease_inner;
mod root_turn;
mod weak_root_lease;

pub(crate) use kernel_file_lock::KernelFileLock;
pub use lease_timing::LeaseTiming;
pub use root_lease::RootLease;
pub(crate) use weak_root_lease::WeakRootLease;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod takeover_tests;
