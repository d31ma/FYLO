use serde_json::{Map, Value};

/// A slash-separated field path into a document, as `$encrypted` uses
/// (`payload/verifier`). Paths reach through objects only, never into arrays.
pub(super) struct DocumentPath {
    segments: Vec<String>,
}

impl DocumentPath {
    pub(super) fn parse(path: &str) -> Result<Self, String> {
        let segments: Vec<String> = path.split('/').map(str::to_owned).collect();
        if segments.iter().any(String::is_empty) {
            return Err(format!("migration path {path:?} has an empty segment"));
        }
        Ok(Self { segments })
    }

    pub(super) fn get<'a>(&self, document: &'a Map<String, Value>) -> Option<&'a Value> {
        let (last, parents) = self.segments.split_last()?;
        let mut object = document;
        for segment in parents {
            object = object.get(segment)?.as_object()?;
        }
        object.get(last)
    }

    /// The value at this path, treating JSON `null` as missing.
    pub(super) fn present<'a>(&self, document: &'a Map<String, Value>) -> Option<&'a Value> {
        self.get(document).filter(|value| !value.is_null())
    }

    /// Set the value, creating missing parent objects. A parent that exists
    /// but is not an object is an error rather than being replaced.
    pub(super) fn set(
        &self,
        document: &mut Map<String, Value>,
        value: Value,
    ) -> Result<(), String> {
        let Some((last, parents)) = self.segments.split_last() else {
            return Ok(());
        };
        let mut object = document;
        for segment in parents {
            object = object
                .entry(segment.clone())
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or_else(|| format!("migration path {} crosses a non-object", self.display()))?;
        }
        object.insert(last.clone(), value);
        Ok(())
    }

    pub(super) fn remove(&self, document: &mut Map<String, Value>) -> Option<Value> {
        let (last, parents) = self.segments.split_last()?;
        let mut object = document;
        for segment in parents {
            object = object.get_mut(segment)?.as_object_mut()?;
        }
        object.remove(last)
    }

    pub(super) fn display(&self) -> String {
        self.segments.join("/")
    }
}
