use serde_json::Value;

use super::store_change::StoreChange;
use super::store_entity::StoreEntity;
use super::store_request::StoreRequest;
use super::store_view::StoreView;
use crate::MachineError;

/// What one Store operation answers, and the change it makes, worked out
/// before anything is written.
pub(crate) struct StoreOutcome {
    answer: StoreAnswer,
    change: Option<StoreChange>,
}

/// An answer known at once, or the entity of a document to read after the
/// change is written.
enum StoreAnswer {
    Value(Value),
    Entity(String),
}

impl StoreOutcome {
    /// A read: an answer and no change.
    pub(crate) const fn value(answer: Value) -> Self {
        Self {
            answer: StoreAnswer::Value(answer),
            change: None,
        }
    }

    /// A write that answers the document `id` as it is after the write.
    pub(crate) fn written(id: &str, change: StoreChange) -> Self {
        Self {
            answer: StoreAnswer::Entity(id.to_owned()),
            change: Some(change),
        }
    }

    /// A write that answers a value known before it, such as a count.
    pub(crate) const fn counted(answer: Value, change: StoreChange) -> Self {
        Self {
            answer: StoreAnswer::Value(answer),
            change: Some(change),
        }
    }

    pub(crate) const fn change(&self) -> Option<&StoreChange> {
        self.change.as_ref()
    }

    /// The answer, reading a written document through `view`, which must
    /// already show the change.
    pub(crate) fn answer(
        self,
        view: &StoreView<'_>,
        store: &StoreRequest<'_>,
    ) -> Result<Value, MachineError> {
        match self.answer {
            StoreAnswer::Value(answer) => Ok(answer),
            StoreAnswer::Entity(id) => {
                let fields = view.record(store.collection(), &id)?;
                Ok(StoreEntity::or_null(&id, fields, store.id_field()))
            }
        }
    }
}
