use crate::{NativeStorageError, NativeStorageErrorCode};

/// Claim behavior for one consumer group poll.
#[derive(Clone, Copy, Debug)]
pub struct QueueClaimOptions {
    /// Maximum messages returned by one call.
    pub max_messages: usize,
    /// Time during which another worker in the group cannot claim the message.
    pub visibility_timeout_ms: u64,
    /// Delivery attempts before the message enters this group's dead letter set.
    pub max_attempts: u32,
    /// Maximum aggregate encoded delivery bytes returned by one claim.
    pub max_bytes: usize,
}

impl QueueClaimOptions {
    const DEFAULT_VISIBILITY_TIMEOUT_MILLISECONDS: u64 = 30_000;
    const DEFAULT_MAXIMUM_ATTEMPTS: u32 = 3;
    pub(super) const DEFAULT_READ_BUDGET_BYTES: usize = 8_388_608;
    pub(super) const MINIMUM_VISIBILITY_TIMEOUT_MILLISECONDS: u64 = 100;
    const MAXIMUM_VISIBILITY_TIMEOUT_MILLISECONDS: u64 = 86_400_000;
    pub(super) const MAXIMUM_ATTEMPTS: u32 = 100;
    const MAXIMUM_CLAIM_MESSAGES: usize = 1_000;
    const MINIMUM_CLAIM_MESSAGES: usize = 1;
    const MINIMUM_ATTEMPTS: u32 = 1;
    const MINIMUM_READ_BUDGET_BYTES: usize = 1;

    pub(super) fn validate(self) -> Result<(), NativeStorageError> {
        self.check_message_limit()?;
        Self::validate_visibility_timeout(self.visibility_timeout_ms)?;
        self.check_attempt_limit()?;
        Self::validate_response_budget(self.max_bytes)
    }

    pub(super) fn validate_visibility_timeout(value: u64) -> Result<(), NativeStorageError> {
        Self::check_visibility_timeout(value)
    }

    pub(super) fn validate_response_budget(max_bytes: usize) -> Result<(), NativeStorageError> {
        Self::check_response_budget(max_bytes)
    }

    fn check_message_limit(self) -> Result<(), NativeStorageError> {
        if !(Self::MINIMUM_CLAIM_MESSAGES..=Self::MAXIMUM_CLAIM_MESSAGES)
            .contains(&self.max_messages)
        {
            return Err(Self::limit_error(
                "queue claim limit must be between 1 and 1000",
            ));
        }
        Ok(())
    }

    fn check_visibility_timeout(value: u64) -> Result<(), NativeStorageError> {
        if !(Self::MINIMUM_VISIBILITY_TIMEOUT_MILLISECONDS
            ..=Self::MAXIMUM_VISIBILITY_TIMEOUT_MILLISECONDS)
            .contains(&value)
        {
            return Err(Self::limit_error(
                "queue visibility timeout must be between 100 and 86400000 milliseconds",
            ));
        }
        Ok(())
    }

    fn check_attempt_limit(self) -> Result<(), NativeStorageError> {
        if !(Self::MINIMUM_ATTEMPTS..=Self::MAXIMUM_ATTEMPTS).contains(&self.max_attempts) {
            return Err(Self::limit_error(
                "queue max attempts must be between 1 and 100",
            ));
        }
        Ok(())
    }

    fn check_response_budget(max_bytes: usize) -> Result<(), NativeStorageError> {
        if !(Self::MINIMUM_READ_BUDGET_BYTES..=Self::DEFAULT_READ_BUDGET_BYTES).contains(&max_bytes)
        {
            return Err(Self::limit_error(
                "queue response budget must be between 1 byte and 8 MiB",
            ));
        }
        Ok(())
    }

    fn limit_error(message: &str) -> NativeStorageError {
        NativeStorageError::new(NativeStorageErrorCode::QueueLimit, message)
    }
}

impl Default for QueueClaimOptions {
    fn default() -> Self {
        Self {
            max_messages: 1,
            visibility_timeout_ms: Self::DEFAULT_VISIBILITY_TIMEOUT_MILLISECONDS,
            max_attempts: Self::DEFAULT_MAXIMUM_ATTEMPTS,
            max_bytes: Self::DEFAULT_READ_BUDGET_BYTES,
        }
    }
}
