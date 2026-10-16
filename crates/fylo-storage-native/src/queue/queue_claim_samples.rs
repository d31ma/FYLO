use crate::NativeStorageErrorCode;

pub(super) struct QueueClaimSamples;

impl QueueClaimSamples {
    pub(super) const TOPIC: &str = "jobs";
    pub(super) const GROUP: &str = "workers";
    pub(super) const DIRECTORY: &str = ".fylo-queue";
    pub(super) const MAXIMUM_MESSAGES: usize = 1000;
    pub(super) const MINIMUM_VISIBILITY_TIMEOUT_MILLISECONDS: u64 = 100;
    pub(super) const MAXIMUM_VISIBILITY_TIMEOUT_MILLISECONDS: u64 = 86_400_000;
    pub(super) const MAXIMUM_ATTEMPTS: u32 = 100;
    pub(super) const MAXIMUM_BYTES: usize = 8_388_608;
    pub(super) const MESSAGE_LIMIT_ERROR: &str =
        "EQUEUE_LIMIT: queue claim limit must be between 1 and 1000";
    pub(super) const VISIBILITY_LIMIT_ERROR: &str =
        "EQUEUE_LIMIT: queue visibility timeout must be between 100 and 86400000 milliseconds";
    pub(super) const ATTEMPT_LIMIT_ERROR: &str =
        "EQUEUE_LIMIT: queue max attempts must be between 1 and 100";
    pub(super) const BUDGET_LIMIT_ERROR: &str =
        "EQUEUE_LIMIT: queue response budget must be between 1 byte and 8 MiB";

    pub(super) fn rejected(message: &str) -> (NativeStorageErrorCode, String, bool) {
        (
            NativeStorageErrorCode::QueueLimit,
            message.to_owned(),
            false,
        )
    }
}
