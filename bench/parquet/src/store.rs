use serde_json::Value;

use crate::data::Doc;

pub type Outcome<T> = Result<T, String>;

pub struct Filter {
    pub field: &'static str,
    pub minimum: i64,
}

/// What every engine under test offers the workload: FYLO's document API,
/// answered by FYLO itself or by the Parquet model.
pub trait Store {
    fn put(&mut self, doc: Doc) -> Outcome<String>;
    fn put_batch(&mut self, docs: Vec<Doc>) -> Outcome<Vec<String>>;
    fn patch(&mut self, id: &str, changes: &Doc) -> Outcome<()>;
    fn delete(&mut self, id: &str) -> Outcome<()>;
    fn get(&mut self, id: &str) -> Outcome<Option<Doc>>;
    fn latest(&mut self, id: &str) -> Outcome<Option<Doc>>;
    fn find_eq(&mut self, field: &str, value: i64) -> Outcome<Vec<(String, Doc)>>;
    fn find_range(&mut self, field: &str, low: f64, high: f64) -> Outcome<Vec<(String, Doc)>>;
    fn scan_project(&mut self, filter: Option<&Filter>, fields: &[&str]) -> Outcome<Vec<Doc>>;
    fn count(&mut self) -> Outcome<usize>;
    fn compact(&mut self) -> Outcome<Value>;
    fn reopen(&mut self) -> Outcome<()>;
    fn file_count(&self) -> usize;
}
