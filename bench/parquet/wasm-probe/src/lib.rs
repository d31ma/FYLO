use std::sync::Arc;

use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use bytes_free::round_trip;

mod bytes_free {
    use super::*;
    use parquet::arrow::ArrowWriter;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::basic::Compression;
    use parquet::file::properties::WriterProperties;

    pub fn round_trip(rows: i64) -> usize {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("value", DataType::Int64, false),
        ]));
        let ids: ArrayRef = Arc::new(StringArray::from_iter_values(
            (0..rows).map(|row| row.to_string()),
        ));
        let values: ArrayRef = Arc::new(Int64Array::from_iter_values(0..rows));
        let Ok(batch) = RecordBatch::try_new(schema.clone(), vec![ids, values]) else {
            return 0;
        };
        let properties = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        let mut buffer = Vec::new();
        let Ok(mut writer) = ArrowWriter::try_new(&mut buffer, schema, Some(properties)) else {
            return 0;
        };
        if writer.write(&batch).is_err() || writer.close().is_err() {
            return 0;
        }
        let Ok(builder) = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(buffer))
        else {
            return 0;
        };
        let Ok(reader) = builder.build() else {
            return 0;
        };
        reader.flatten().map(|batch| batch.num_rows()).sum()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn parquet_round_trip(rows: i64) -> usize {
    round_trip(rows)
}
