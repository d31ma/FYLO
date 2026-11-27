use std::collections::BTreeSet;

use super::IndexGrowth;

fn keys(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn a_write_that_only_removes_keys_needs_no_room() {
    let growth = IndexGrowth::of(&keys(&["name/a/4VRNF52J000"]), &keys(&[]));
    assert_eq!(growth.needed_bytes(), 0);
}

#[test]
fn a_write_that_swaps_keys_of_the_same_size_needs_no_room() {
    let growth = IndexGrowth::of(
        &keys(&["name/a/4VRNF52J000"]),
        &keys(&["name/b/4VRNF52J000"]),
    );
    assert_eq!(growth.needed_bytes(), 0);
}

#[test]
fn a_write_that_adds_keys_needs_their_log_lines() {
    let growth = IndexGrowth::of(&keys(&[]), &keys(&["ab", "cde"]));
    assert_eq!(growth.needed_bytes(), "+\tab\n+\tcde\n".len() as u64);
}

#[test]
fn a_write_that_grows_the_index_needs_every_added_line() {
    let growth = IndexGrowth::of(&keys(&["name/a"]), &keys(&["name/bb", "tag/c"]));
    assert_eq!(growth.needed_bytes(), "+\tname/bb\n+\ttag/c\n".len() as u64);
}

#[test]
fn nothing_written_needs_no_room() {
    assert_eq!(IndexGrowth::default().needed_bytes(), 0);
}
