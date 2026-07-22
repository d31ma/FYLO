use serde::Deserialize;
use std::cell::RefCell;
use std::collections::HashSet;

const ERROR: i32 = -1;

#[derive(Deserialize)]
struct ScanQuery {
    prefix: String,
    range: Option<ScanRange>,
}

#[derive(Deserialize)]
struct ScanRange {
    op: String,
    value: String,
}

thread_local! {
    static SNAPSHOT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

#[no_mangle]
pub extern "C" fn allocate(length: usize) -> *mut u8 {
    let mut bytes = Vec::<u8>::with_capacity(length);
    let pointer = bytes.as_mut_ptr();
    std::mem::forget(bytes);
    pointer
}

/// # Safety
/// `pointer` must come from `allocate(capacity)` and must not have been freed.
#[no_mangle]
pub unsafe extern "C" fn deallocate(pointer: *mut u8, capacity: usize) {
    if pointer.is_null() || capacity == 0 {
        return;
    }
    unsafe { drop(Vec::from_raw_parts(pointer, 0, capacity)) };
}

/// Replace this instance's immutable, sorted newline-delimited snapshot.
///
/// # Safety
/// `pointer` must identify `length` readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn load_snapshot(pointer: *const u8, length: usize) -> i32 {
    if pointer.is_null() && length > 0 {
        return ERROR;
    }
    let source = if length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(pointer, length) }
    };
    SNAPSHOT.with(|snapshot| {
        let mut snapshot = snapshot.borrow_mut();
        snapshot.clear();
        snapshot.extend_from_slice(source);
    });
    0
}

/// Scan one or more prefix/range constraints, intersecting document IDs.
///
/// The return value is the required output length. If the supplied buffer is
/// too small no bytes are written, allowing the host to resize and retry.
///
/// # Safety
/// Input/output pointers must identify readable/writable regions of the stated
/// sizes and must not be concurrently mutated during this call.
#[no_mangle]
pub unsafe extern "C" fn scan_queries(
    query_pointer: *const u8,
    query_length: usize,
    output_pointer: *mut u8,
    output_capacity: usize,
) -> i32 {
    if query_pointer.is_null() {
        return ERROR;
    }
    let input = unsafe { std::slice::from_raw_parts(query_pointer, query_length) };
    let queries: Vec<ScanQuery> = match serde_json::from_slice::<Vec<ScanQuery>>(input) {
        Ok(queries) if !queries.is_empty() => queries,
        _ => return ERROR,
    };
    let encoded = SNAPSHOT.with(|snapshot| scan_snapshot(&snapshot.borrow(), &queries));
    let required = encoded.len();
    let Ok(required_i32) = i32::try_from(required) else {
        return ERROR;
    };
    if required > output_capacity || (required > 0 && output_pointer.is_null()) {
        return required_i32;
    }
    unsafe { std::ptr::copy_nonoverlapping(encoded.as_ptr(), output_pointer, required) };
    required_i32
}

fn scan_snapshot(snapshot: &[u8], queries: &[ScanQuery]) -> Vec<u8> {
    let mut candidates: Option<Vec<Vec<u8>>> = None;
    for query in queries {
        let prefix = query.prefix.as_bytes();
        let mut cursor = find_first_key_at_or_after(snapshot, prefix);
        let mut next = Vec::new();
        let mut seen = HashSet::new();
        while cursor < snapshot.len() {
            let relative_end = snapshot[cursor..]
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap_or(snapshot.len() - cursor);
            let end = cursor + relative_end;
            let key = &snapshot[cursor..end];
            if !key.starts_with(prefix) {
                break;
            }
            if include_key_in_range(key, query.range.as_ref()) {
                if let Some(separator) = key.iter().rposition(|byte| *byte == b'/') {
                    let id = key[separator + 1..].to_vec();
                    if !id.is_empty() && seen.insert(id.clone()) {
                        next.push(id);
                    }
                }
            }
            cursor = end.saturating_add(1);
        }
        candidates = Some(match candidates {
            None => next,
            Some(current) => {
                let allowed: HashSet<Vec<u8>> = next.into_iter().collect();
                current
                    .into_iter()
                    .filter(|id| allowed.contains(id))
                    .collect()
            }
        });
    }
    let mut output = Vec::new();
    for id in candidates.unwrap_or_default() {
        output.extend_from_slice(&id);
        output.push(b'\n');
    }
    output
}

fn find_first_key_at_or_after(snapshot: &[u8], prefix: &[u8]) -> usize {
    if snapshot.is_empty() {
        return 0;
    }
    let mut low = 0;
    let mut high = snapshot.len();
    while low < high {
        let middle = (low + high) / 2;
        let mut start = middle;
        while start > 0 && snapshot[start - 1] != b'\n' {
            start -= 1;
        }
        let mut end = middle;
        while end < snapshot.len() && snapshot[end] != b'\n' {
            end += 1;
        }
        if snapshot[start..end] < *prefix {
            low = end.saturating_add(1);
        } else {
            high = start;
        }
    }
    while low > 0 && low < snapshot.len() && snapshot[low - 1] != b'\n' {
        low -= 1;
    }
    low
}

fn include_key_in_range(key: &[u8], range: Option<&ScanRange>) -> bool {
    let Some(range) = range else {
        return true;
    };
    let Some(last) = key.iter().rposition(|byte| *byte == b'/') else {
        return false;
    };
    let Some(previous) = key[..last].iter().rposition(|byte| *byte == b'/') else {
        return false;
    };
    let value = &key[previous + 1..last];
    let threshold = range.value.as_bytes();
    match range.op.as_str() {
        "$gt" => value > threshold,
        "$gte" => value >= threshold,
        // Less-than indexes use reverse-sortable values, so their byte
        // comparisons intentionally point in the same direction.
        "$lt" => value > threshold,
        "$lte" => value >= threshold,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_prefix_ranges_and_intersections() {
        let snapshot = b"score/n/400fffffffffffff/doc-a\nscore/n/bff0000000000000/doc-b\ntitle/f/prefix%20a/doc-a\ntitle/f/prefix%20b/doc-b\n";
        let prefix = ScanQuery {
            prefix: "title/f/prefix%20".into(),
            range: None,
        };
        let range = ScanQuery {
            prefix: "score/n/".into(),
            range: Some(ScanRange {
                op: "$gte".into(),
                value: "8000000000000000".into(),
            }),
        };
        assert_eq!(scan_snapshot(snapshot, &[prefix]), b"doc-a\ndoc-b\n");
        assert_eq!(scan_snapshot(snapshot, &[range]), b"doc-b\n");
    }

    #[test]
    fn reverse_ranges_match_the_browser_index_contract() {
        let snapshot = b"score/nr/400fffffffffffff/doc-high\nscore/nr/bff0000000000000/doc-low\n";
        let range = ScanQuery {
            prefix: "score/nr/".into(),
            range: Some(ScanRange {
                op: "$lt".into(),
                value: "8000000000000000".into(),
            }),
        };
        assert_eq!(scan_snapshot(snapshot, &[range]), b"doc-low\n");
    }
}
