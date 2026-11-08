use fylo_storage_native::{
    QueueAckResult, QueueClaimOptions, QueueDelivery, QueueNackResult, QueuePublishOptions,
    QueuePublishResult,
};
use serde_json::Value;

use crate::{Error, Root};

/// A queue topic in an open [`Root`] (ADR 0009).
#[derive(Clone, Copy, Debug)]
pub struct Queue<'root> {
    root: &'root Root,
    topic: &'root str,
}

/// How [`Queue::publish`] publishes one message.
#[derive(Clone, Debug, Default)]
pub struct PublishOptions {
    /// Keep the message unclaimable for this many milliseconds.
    pub delay_ms: u64,
    /// A producer key: publishing again with it returns the first message.
    pub idempotency_key: Option<String>,
}

/// How [`Queue::claim`] leases deliveries to one consumer group.
#[derive(Clone, Copy, Debug)]
pub struct ClaimOptions {
    /// Most deliveries returned by one claim.
    pub max_messages: usize,
    /// How long another worker in the group cannot claim a delivery.
    pub visibility_timeout_ms: u64,
    /// Attempts before a message moves to the group's dead letters.
    pub max_attempts: u32,
    /// Most encoded payload bytes returned by one claim.
    pub max_bytes: usize,
}

impl Default for ClaimOptions {
    fn default() -> Self {
        Self {
            max_messages: 1,
            visibility_timeout_ms: 30_000,
            max_attempts: 3,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

impl<'root> Queue<'root> {
    pub(crate) fn new(root: &'root Root, topic: &'root str) -> Self {
        Self { root, topic }
    }

    /// Durably publish one message.
    ///
    /// # Errors
    ///
    /// Fails for an invalid topic, an oversized payload, an idempotency key
    /// reused with another payload, or a failed durable write.
    pub fn publish(
        &self,
        payload: Value,
        options: &PublishOptions,
    ) -> Result<QueuePublishResult, Error> {
        let options = QueuePublishOptions {
            delay_ms: options.delay_ms,
            idempotency_key: options.idempotency_key.clone(),
        };
        self.root
            .native_queue()?
            .publish(self.topic, payload, &options)
            .map_err(Error::from)
    }

    /// Lease up to `options.max_messages` deliveries to `group`. Each claim
    /// is durable before it returns.
    ///
    /// # Errors
    ///
    /// Fails for invalid names or options, or a failed durable write.
    pub fn claim(&self, group: &str, options: ClaimOptions) -> Result<Vec<QueueDelivery>, Error> {
        let options = QueueClaimOptions {
            max_messages: options.max_messages,
            visibility_timeout_ms: options.visibility_timeout_ms,
            max_attempts: options.max_attempts,
            max_bytes: options.max_bytes,
        };
        self.root
            .native_queue()?
            .claim(self.topic, group, options)
            .map_err(Error::from)
    }

    /// Settle a delivery as done.
    ///
    /// # Errors
    ///
    /// `EQUEUE_RECEIPT` for a stale receipt, or a failed durable write.
    pub fn ack(&self, group: &str, id: &str, receipt: &str) -> Result<QueueAckResult, Error> {
        self.root
            .native_queue()?
            .ack(self.topic, group, id, receipt)
            .map_err(Error::from)
    }

    /// Return a delivery for another attempt after `delay_ms`.
    ///
    /// # Errors
    ///
    /// `EQUEUE_RECEIPT` for a stale receipt, or a failed durable write.
    pub fn nack(
        &self,
        group: &str,
        id: &str,
        receipt: &str,
        delay_ms: u64,
        reason: &str,
    ) -> Result<QueueNackResult, Error> {
        self.root
            .native_queue()?
            .nack(self.topic, group, id, receipt, delay_ms, reason)
            .map_err(Error::from)
    }
}
