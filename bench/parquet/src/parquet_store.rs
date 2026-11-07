use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use arrow_array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::ArrowWriter;
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReaderBuilder, RowSelection,
    RowSelector,
};
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::metadata::PageIndexPolicy;
use parquet::file::properties::{EnabledStatistics, WriterProperties};
use parquet::file::statistics::Statistics;
use serde_json::{Value, json};

use crate::data::{Doc, Shape};
use crate::store::{Filter, Outcome, Store};

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[derive(Clone, Copy, Debug)]
pub enum Codec {
    Uncompressed,
    Snappy,
    Zstd(i32),
}

impl Codec {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "none" => Ok(Self::Uncompressed),
            "snappy" => Ok(Self::Snappy),
            "zstd" => Ok(Self::Zstd(3)),
            other => Err(format!("unknown codec {other}")),
        }
    }

    pub fn name(self) -> String {
        match self {
            Self::Uncompressed => "none".into(),
            Self::Snappy => "snappy".into(),
            Self::Zstd(level) => format!("zstd({level})"),
        }
    }

    fn compression(self) -> Outcome<Compression> {
        Ok(match self {
            Self::Uncompressed => Compression::UNCOMPRESSED,
            Self::Snappy => Compression::SNAPPY,
            Self::Zstd(level) => Compression::ZSTD(ZstdLevel::try_new(level).map_err(text)?),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Location {
    file: u64,
    row: u64,
    deleted: bool,
}

struct DataFile {
    sequence: u64,
    path: PathBuf,
    handle: File,
    metadata: ArrowReaderMetadata,
    rows: u64,
}

/// FYLO's document API over immutable Parquet files, modelled the way table
/// formats such as Delta Lake and Iceberg do it: every commit writes one new
/// file (temporary name, fsync, rename, directory fsync), a later version of
/// a row shadows the earlier one, a delete writes a tombstone row, and
/// compaction rewrites the live rows into one sorted base file. On open, the
/// id and tombstone columns of every file are read into an in-memory map
/// from id to its newest row, which is the only index.
pub struct ParquetStore {
    directory: PathBuf,
    codec: Codec,
    doc_schema: SchemaRef,
    schema: SchemaRef,
    files: Vec<DataFile>,
    index: HashMap<String, Location>,
    next_sequence: u64,
    next_id: u64,
    directory_sync_failures: u64,
}

const ID: &str = "_id";
const SEQUENCE: &str = "_seq";
const DELETED: &str = "_deleted";
const META_COLUMNS: usize = 3;
const ROW_GROUP_ROWS: usize = 65_536;
const PAGE_ROWS: usize = 1024;

impl ParquetStore {
    pub fn create(directory: &Path, shape: Shape, codec: Codec) -> Outcome<Self> {
        std::fs::create_dir_all(directory).map_err(text)?;
        Self::open(directory, shape, codec)
    }

    pub fn open(directory: &Path, shape: Shape, codec: Codec) -> Outcome<Self> {
        let doc_schema = shape.arrow_schema();
        let mut fields = vec![
            Field::new(ID, DataType::Utf8, false),
            Field::new(SEQUENCE, DataType::UInt64, false),
            Field::new(DELETED, DataType::Boolean, false),
        ];
        fields.extend(
            doc_schema
                .fields()
                .iter()
                .map(|field| field.as_ref().clone()),
        );
        let mut store = Self {
            directory: directory.to_path_buf(),
            codec,
            doc_schema,
            schema: Arc::new(Schema::new(fields)),
            files: Vec::new(),
            index: HashMap::new(),
            next_sequence: 1,
            next_id: 0,
            directory_sync_failures: 0,
        };
        store.load()?;
        Ok(store)
    }

    pub fn directory_sync_failures(&self) -> u64 {
        self.directory_sync_failures
    }

    fn load(&mut self) -> Outcome<()> {
        self.files.clear();
        self.index.clear();
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&self.directory).map_err(text)? {
            let path = entry.map_err(text)?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_owned();
            if name.ends_with(".tmp") {
                std::fs::remove_file(&path).map_err(text)?;
                continue;
            }
            if let Some(stem) = name.strip_suffix(".parquet") {
                let sequence: u64 = stem.parse().map_err(text)?;
                names.push((sequence, path));
            }
        }
        names.sort();
        for (sequence, path) in names {
            let file = self.open_file(sequence, path)?;
            self.index_file(&file)?;
            self.next_sequence = sequence + 1;
            self.files.push(file);
        }
        let highest = self
            .index
            .keys()
            .filter_map(|id| id.parse::<u64>().ok())
            .max();
        self.next_id = self.next_id.max(highest.unwrap_or(0));
        Ok(())
    }

    fn open_file(&self, sequence: u64, path: PathBuf) -> Outcome<DataFile> {
        let handle = File::open(&path).map_err(text)?;
        let options = ArrowReaderOptions::new().with_page_index_policy(PageIndexPolicy::Required);
        let metadata = ArrowReaderMetadata::load(&handle, options).map_err(text)?;
        let rows = metadata.metadata().file_metadata().num_rows() as u64;
        Ok(DataFile {
            sequence,
            path,
            handle,
            metadata,
            rows,
        })
    }

    fn builder(&self, file: &DataFile) -> Outcome<ParquetRecordBatchReaderBuilder<File>> {
        let handle = file.handle.try_clone().map_err(text)?;
        Ok(ParquetRecordBatchReaderBuilder::new_with_metadata(
            handle,
            file.metadata.clone(),
        ))
    }

    fn mask(&self, file: &DataFile, names: &[&str]) -> ProjectionMask {
        let indices: Vec<usize> = names
            .iter()
            .filter_map(|name| self.schema.index_of(name).ok())
            .collect();
        ProjectionMask::roots(file.metadata.parquet_schema(), indices)
    }

    fn doc_mask(&self, file: &DataFile) -> ProjectionMask {
        let indices: Vec<usize> = (META_COLUMNS..self.schema.fields().len()).collect();
        ProjectionMask::roots(file.metadata.parquet_schema(), indices)
    }

    fn index_file(&mut self, file: &DataFile) -> Outcome<()> {
        let reader = self
            .builder(file)?
            .with_projection(self.mask(file, &[ID, DELETED]))
            .with_batch_size(65_536)
            .build()
            .map_err(text)?;
        let mut row = 0_u64;
        for batch in reader {
            let batch = batch.map_err(text)?;
            let ids = strings(batch.column(0))?;
            let deleted = booleans(batch.column(1))?;
            for position in 0..batch.num_rows() {
                let location = Location {
                    file: file.sequence,
                    row,
                    deleted: deleted.value(position),
                };
                self.index.insert(ids.value(position).to_owned(), location);
                row += 1;
            }
        }
        Ok(())
    }

    fn allocate_id(&mut self) -> String {
        self.next_id += 1;
        format!("{:016}", self.next_id)
    }

    fn file_by_sequence(&self, sequence: u64) -> Outcome<&DataFile> {
        self.files
            .iter()
            .find(|file| file.sequence == sequence)
            .ok_or_else(|| format!("no data file {sequence}"))
    }

    fn properties(&self, codec: Codec) -> Outcome<WriterProperties> {
        Ok(WriterProperties::builder()
            .set_compression(codec.compression()?)
            .set_max_row_group_row_count(Some(ROW_GROUP_ROWS))
            .set_data_page_row_count_limit(page_rows())
            .set_statistics_enabled(EnabledStatistics::Page)
            .build())
    }

    fn batch(&self, rows: &[(String, bool, Doc)], sequence: u64) -> Outcome<RecordBatch> {
        let mut decoder = arrow_json::ReaderBuilder::new(self.doc_schema.clone())
            .with_batch_size(rows.len().max(1))
            .build_decoder()
            .map_err(text)?;
        let docs: Vec<&Doc> = rows.iter().map(|(_, _, doc)| doc).collect();
        decoder.serialize(&docs).map_err(text)?;
        let decoded = decoder
            .flush()
            .map_err(text)?
            .ok_or_else(|| "nothing decoded".to_owned())?;
        let ids: ArrayRef = Arc::new(StringArray::from_iter_values(
            rows.iter().map(|(id, _, _)| id.as_str()),
        ));
        let sequences: ArrayRef = Arc::new(UInt64Array::from(vec![sequence; rows.len()]));
        let deleted: ArrayRef = Arc::new(BooleanArray::from(
            rows.iter().map(|(_, gone, _)| *gone).collect::<Vec<bool>>(),
        ));
        let mut columns = vec![ids, sequences, deleted];
        columns.extend(decoded.columns().iter().cloned());
        RecordBatch::try_new(self.schema.clone(), columns).map_err(text)
    }

    pub fn write_file(
        &mut self,
        path: &Path,
        batches: &[RecordBatch],
        codec: Codec,
    ) -> Outcome<()> {
        let temporary = path.with_extension("parquet.tmp");
        let file = File::create(&temporary).map_err(text)?;
        let mut writer =
            ArrowWriter::try_new(file, self.schema.clone(), Some(self.properties(codec)?))
                .map_err(text)?;
        for batch in batches {
            writer.write(batch).map_err(text)?;
        }
        let file = writer.into_inner().map_err(text)?;
        flush(&file)?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(text)?;
        self.sync_directory();
        Ok(())
    }

    fn sync_directory(&mut self) {
        let synced = File::open(&self.directory).map(|directory| flush(&directory));
        if !matches!(synced, Ok(Ok(()))) {
            self.directory_sync_failures += 1;
        }
    }

    fn data_path(&self, sequence: u64) -> PathBuf {
        self.directory.join(format!("{sequence:020}.parquet"))
    }

    fn commit(&mut self, rows: Vec<(String, bool, Doc)>) -> Outcome<()> {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let batch = self.batch(&rows, sequence)?;
        let path = self.data_path(sequence);
        self.write_file(&path, &[batch], self.codec)?;
        let file = self.open_file(sequence, path)?;
        for (row, (id, deleted, _)) in rows.iter().enumerate() {
            let location = Location {
                file: sequence,
                row: row as u64,
                deleted: *deleted,
            };
            self.index.insert(id.clone(), location);
        }
        self.files.push(file);
        Ok(())
    }

    fn read_rows(&self, file: &DataFile, rows: &[u64]) -> Outcome<Vec<Doc>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let mut selectors = Vec::new();
        let mut cursor = 0_u64;
        for &row in rows {
            if row > cursor {
                selectors.push(RowSelector::skip((row - cursor) as usize));
            }
            selectors.push(RowSelector::select(1));
            cursor = row + 1;
        }
        if file.rows > cursor {
            selectors.push(RowSelector::skip((file.rows - cursor) as usize));
        }
        let reader = self
            .builder(file)?
            .with_projection(self.doc_mask(file))
            .with_row_selection(RowSelection::from(selectors))
            .with_batch_size(8192)
            .build()
            .map_err(text)?;
        let mut docs = Vec::with_capacity(rows.len());
        for batch in reader {
            docs.extend(to_docs(&batch.map_err(text)?)?);
        }
        Ok(docs)
    }

    fn is_current(&self, id: &str, file: u64, row: u64) -> bool {
        self.index.get(id).is_some_and(|location| {
            location.file == file && location.row == row && !location.deleted
        })
    }

    fn row_groups_for(&self, file: &DataFile, field: &str, low: f64, high: f64) -> Vec<usize> {
        let metadata = file.metadata.metadata();
        let mut chosen = Vec::new();
        for (position, group) in metadata.row_groups().iter().enumerate() {
            let column = group
                .columns()
                .iter()
                .find(|column| column.column_path().string() == field);
            let overlaps = match column.and_then(|column| column.statistics()) {
                Some(Statistics::Int64(values)) => match (values.min_opt(), values.max_opt()) {
                    (Some(min), Some(max)) => (*max as f64) >= low && (*min as f64) < high,
                    _ => true,
                },
                Some(Statistics::Double(values)) => match (values.min_opt(), values.max_opt()) {
                    (Some(min), Some(max)) => *max >= low && *min < high,
                    _ => true,
                },
                _ => true,
            };
            if overlaps {
                chosen.push(position);
            }
        }
        chosen
    }

    fn matching_rows(
        &self,
        file: &DataFile,
        field: &str,
        low: f64,
        high: f64,
    ) -> Outcome<(Vec<u64>, Vec<String>)> {
        let groups = self.row_groups_for(file, field, low, high);
        let metadata = file.metadata.metadata();
        let mut starts = Vec::with_capacity(metadata.num_row_groups());
        let mut start = 0_u64;
        for group in metadata.row_groups() {
            starts.push(start);
            start += group.num_rows() as u64;
        }
        let mut rows = Vec::new();
        let mut ids = Vec::new();
        for group in groups {
            let reader = self
                .builder(file)?
                .with_projection(self.mask(file, &[ID, field]))
                .with_row_groups(vec![group])
                .with_batch_size(65_536)
                .build()
                .map_err(text)?;
            let mut row = starts[group];
            for batch in reader {
                let batch = batch.map_err(text)?;
                let batch_ids = strings(batch.column(0))?;
                let values = numbers(batch.column(1))?;
                for position in 0..batch.num_rows() {
                    let value = values[position];
                    let id = batch_ids.value(position);
                    if value >= low && value < high && self.is_current(id, file.sequence, row) {
                        rows.push(row);
                        ids.push(id.to_owned());
                    }
                    row += 1;
                }
            }
        }
        Ok((rows, ids))
    }

    fn find_between(&self, field: &str, low: f64, high: f64) -> Outcome<Vec<(String, Doc)>> {
        let mut found = Vec::new();
        for file in &self.files {
            let (rows, ids) = self.matching_rows(file, field, low, high)?;
            let docs = self.read_rows(file, &rows)?;
            found.extend(ids.into_iter().zip(docs));
        }
        Ok(found)
    }

    fn live_batches(&self, file: &DataFile) -> Outcome<Vec<RecordBatch>> {
        let reader = self
            .builder(file)?
            .with_batch_size(65_536)
            .build()
            .map_err(text)?;
        let mut batches = Vec::new();
        let mut row = 0_u64;
        for batch in reader {
            let batch = batch.map_err(text)?;
            let ids = strings(batch.column(0))?;
            let mut keep = Vec::with_capacity(batch.num_rows());
            for position in 0..batch.num_rows() {
                keep.push(self.is_current(ids.value(position), file.sequence, row));
                row += 1;
            }
            let mask = BooleanArray::from(keep);
            batches.push(arrow_select::filter::filter_record_batch(&batch, &mask).map_err(text)?);
        }
        Ok(batches)
    }

    pub fn live_sorted(&self) -> Outcome<RecordBatch> {
        let mut batches = Vec::new();
        for file in &self.files {
            batches.extend(self.live_batches(file)?);
        }
        let all = arrow_select::concat::concat_batches(&self.schema, &batches).map_err(text)?;
        let order = arrow_ord::sort::sort_to_indices(all.column(0), None, None).map_err(text)?;
        arrow_select::take::take_record_batch(&all, &order).map_err(text)
    }

    pub fn write_copy(&mut self, path: &Path, codec: Codec) -> Outcome<f64> {
        let live = self.live_sorted()?;
        let started = Instant::now();
        self.write_file(path, &[live], codec)?;
        Ok(started.elapsed().as_secs_f64() * 1e3)
    }
}

impl Store for ParquetStore {
    fn put(&mut self, doc: Doc) -> Outcome<String> {
        let id = self.allocate_id();
        self.commit(vec![(id.clone(), false, doc)])?;
        Ok(id)
    }

    fn put_batch(&mut self, docs: Vec<Doc>) -> Outcome<Vec<String>> {
        let mut rows = Vec::with_capacity(docs.len());
        let mut ids = Vec::with_capacity(docs.len());
        for doc in docs {
            let id = self.allocate_id();
            ids.push(id.clone());
            rows.push((id, false, doc));
        }
        self.commit(rows)?;
        Ok(ids)
    }

    fn patch(&mut self, id: &str, changes: &Doc) -> Outcome<()> {
        let mut doc = self.get(id)?.ok_or_else(|| format!("{id} not found"))?;
        for (key, value) in changes {
            doc.insert(key.clone(), value.clone());
        }
        self.commit(vec![(id.to_owned(), false, doc)])
    }

    fn delete(&mut self, id: &str) -> Outcome<()> {
        if self.get(id)?.is_none() {
            return Err(format!("{id} not found"));
        }
        self.commit(vec![(id.to_owned(), true, Doc::new())])
    }

    fn get(&mut self, id: &str) -> Outcome<Option<Doc>> {
        let Some(location) = self.index.get(id).copied() else {
            return Ok(None);
        };
        if location.deleted {
            return Ok(None);
        }
        let file = self.file_by_sequence(location.file)?;
        Ok(self.read_rows(file, &[location.row])?.into_iter().next())
    }

    fn latest(&mut self, id: &str) -> Outcome<Option<Doc>> {
        self.get(id)
    }

    fn find_eq(&mut self, field: &str, value: i64) -> Outcome<Vec<(String, Doc)>> {
        let exact = value as f64;
        self.find_between(field, exact, exact + 0.5)
    }

    fn find_range(&mut self, field: &str, low: f64, high: f64) -> Outcome<Vec<(String, Doc)>> {
        self.find_between(field, low, high)
    }

    fn scan_project(&mut self, filter: Option<&Filter>, fields: &[&str]) -> Outcome<Vec<Doc>> {
        let mut names = vec![ID];
        names.extend_from_slice(fields);
        if let Some(filter) = filter {
            names.push(filter.field);
        }
        let mut out = Vec::new();
        for file in &self.files {
            let reader = self
                .builder(file)?
                .with_projection(self.mask(file, &names))
                .with_batch_size(65_536)
                .build()
                .map_err(text)?;
            let mut row = 0_u64;
            for batch in reader {
                let batch = batch.map_err(text)?;
                let ids = strings(batch.column(0))?;
                let filter_values = match filter {
                    Some(filter) => Some(numbers(
                        batch
                            .column_by_name(filter.field)
                            .ok_or("no filter column")?,
                    )?),
                    None => None,
                };
                let mut keep = Vec::with_capacity(batch.num_rows());
                for position in 0..batch.num_rows() {
                    let current = self.is_current(ids.value(position), file.sequence, row);
                    let passes = match (&filter_values, filter) {
                        (Some(values), Some(filter)) => values[position] >= filter.minimum as f64,
                        _ => true,
                    };
                    keep.push(current && passes);
                    row += 1;
                }
                let kept =
                    arrow_select::filter::filter_record_batch(&batch, &BooleanArray::from(keep))
                        .map_err(text)?;
                let indices: Vec<usize> = fields
                    .iter()
                    .filter_map(|name| kept.schema().index_of(name).ok())
                    .collect();
                out.extend(to_docs(&kept.project(&indices).map_err(text)?)?);
            }
        }
        Ok(out)
    }

    fn count(&mut self) -> Outcome<usize> {
        Ok(self
            .index
            .values()
            .filter(|location| !location.deleted)
            .count())
    }

    fn compact(&mut self) -> Outcome<Value> {
        let files_before = self.files.len();
        let bytes_before: u64 = self
            .files
            .iter()
            .filter_map(|file| std::fs::metadata(&file.path).ok())
            .map(|metadata| metadata.len())
            .sum();
        let live = self.live_sorted()?;
        let rows = live.num_rows();
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let path = self.data_path(sequence);
        self.write_file(&path, &[live], self.codec)?;
        let old: Vec<PathBuf> = self.files.iter().map(|file| file.path.clone()).collect();
        self.files.clear();
        for path in old {
            std::fs::remove_file(path).map_err(text)?;
        }
        self.sync_directory();
        self.load()?;
        let bytes_after = std::fs::metadata(&path).map_err(text)?.len();
        Ok(json!({
            "files_before": files_before,
            "bytes_before": bytes_before,
            "rows_after": rows,
            "bytes_after": bytes_after,
        }))
    }

    fn reopen(&mut self) -> Outcome<()> {
        self.load()
    }

    fn file_count(&self) -> usize {
        self.files.len()
    }
}

fn page_rows() -> usize {
    std::env::var("BENCH_PAGE_ROWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(PAGE_ROWS)
}

fn flush(file: &File) -> Outcome<()> {
    let status = unsafe { libc::fsync(file.as_raw_fd()) };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

fn strings(column: &ArrayRef) -> Outcome<&StringArray> {
    column
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| "expected a string column".to_owned())
}

fn booleans(column: &ArrayRef) -> Outcome<&BooleanArray> {
    column
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| "expected a boolean column".to_owned())
}

fn numbers(column: &ArrayRef) -> Outcome<Vec<f64>> {
    if let Some(values) = column.as_any().downcast_ref::<Int64Array>() {
        return Ok((0..values.len())
            .map(|position| {
                if values.is_null(position) {
                    f64::NAN
                } else {
                    values.value(position) as f64
                }
            })
            .collect());
    }
    if let Some(values) = column.as_any().downcast_ref::<Float64Array>() {
        return Ok((0..values.len())
            .map(|position| {
                if values.is_null(position) {
                    f64::NAN
                } else {
                    values.value(position)
                }
            })
            .collect());
    }
    Err("expected a numeric column".to_owned())
}

fn to_docs(batch: &RecordBatch) -> Outcome<Vec<Doc>> {
    if batch.num_rows() == 0 {
        return Ok(Vec::new());
    }
    let mut writer = arrow_json::ArrayWriter::new(Vec::new());
    writer.write(batch).map_err(text)?;
    writer.finish().map_err(text)?;
    let bytes = writer.into_inner();
    let values: Vec<Doc> = serde_json::from_slice(&bytes).map_err(text)?;
    Ok(values)
}
