//! Test-only qualification helpers shared by FYLO's Rust crates.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde_json::json;

/// One evaluated row in a unique-cause MC/DC decision table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McdcCase<const N: usize> {
    /// Stable, human-readable row name.
    pub name: &'static str,
    /// Atomic condition outcomes in the same order as the decision definition.
    pub conditions: [bool; N],
    /// Outcome returned by the production decision.
    pub outcome: bool,
}

impl<const N: usize> McdcCase<N> {
    /// Construct one evaluated decision row.
    #[must_use]
    pub const fn new(name: &'static str, conditions: [bool; N], outcome: bool) -> Self {
        Self {
            name,
            conditions,
            outcome,
        }
    }
}

/// `chars` characters of fixed text that does not compress: each is one of
/// 64, picked by a pseudo-random sequence, so a record padded with it keeps
/// six bits of every character through zstd or LZ4. Tests that fill `.fxdb`
/// extents by size pad documents with it, since their records are compressed.
#[must_use]
pub fn incompressible_text(chars: usize) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..chars)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            char::from(ALPHABET[usize::try_from(state >> 58).unwrap_or(0)])
        })
        .collect()
}

/// Prove unique-cause modified condition/decision coverage for one decision.
///
/// Every atomic condition must have a pair of cases where only that condition
/// changes and the overall decision changes with it. Duplicate row names or
/// condition vectors fail closed. When `FYLO_MCDC_REPORT_DIR` is set, the
/// verified vectors and selected independence pairs are emitted as JSON.
///
/// # Panics
///
/// Panics when the decision declaration is invalid, any condition lacks a
/// unique-cause independence pair, or the optional JSON evidence cannot be
/// written.
pub fn assert_unique_cause_mcdc<const N: usize>(
    decision_id: &str,
    condition_names: [&str; N],
    cases: &[McdcCase<N>],
) {
    assert!(N > 0, "{decision_id}: an MC/DC decision needs conditions");
    assert!(
        decision_id.bytes().all(|byte| byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || b"._-".contains(&byte)),
        "{decision_id}: decision ID is not report-path safe"
    );
    assert!(
        condition_names.iter().all(|name| !name.trim().is_empty()),
        "{decision_id}: condition names must not be empty"
    );
    assert_eq!(
        condition_names
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len(),
        N,
        "{decision_id}: condition names must be unique"
    );
    assert!(
        cases.len() > N,
        "{decision_id}: unique-cause MC/DC needs at least {} cases",
        N + 1
    );

    let mut names = BTreeSet::new();
    let mut vectors = BTreeSet::new();
    for case in cases {
        assert!(
            names.insert(case.name),
            "{decision_id}: duplicate case name {:?}",
            case.name
        );
        assert!(
            vectors.insert(case.conditions),
            "{decision_id}: duplicate condition vector in case {:?}",
            case.name
        );
    }

    let pairs = (0..N)
        .map(|condition| {
            independence_pair(cases, condition).unwrap_or_else(|| {
                panic!(
                    "{decision_id}: condition {:?} has no unique-cause independence pair",
                    condition_names[condition]
                )
            })
        })
        .collect::<Vec<_>>();

    if let Some(directory) = std::env::var_os("FYLO_MCDC_REPORT_DIR") {
        write_report(
            Path::new(&directory),
            decision_id,
            condition_names,
            cases,
            &pairs,
        );
    }
}

fn independence_pair<const N: usize>(
    cases: &[McdcCase<N>],
    condition: usize,
) -> Option<(usize, usize)> {
    for (left_index, left) in cases.iter().enumerate() {
        for (right_index, right) in cases.iter().enumerate().skip(left_index + 1) {
            if left.outcome == right.outcome
                || left.conditions[condition] == right.conditions[condition]
            {
                continue;
            }
            if (0..N).all(|index| {
                index == condition || left.conditions[index] == right.conditions[index]
            }) {
                return Some((left_index, right_index));
            }
        }
    }
    None
}

fn write_report<const N: usize>(
    directory: &Path,
    decision_id: &str,
    condition_names: [&str; N],
    cases: &[McdcCase<N>],
    pairs: &[(usize, usize)],
) {
    fs::create_dir_all(directory).unwrap_or_else(|error| {
        panic!("{decision_id}: cannot create MC/DC report directory: {error}")
    });
    let conditions = condition_names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let (left, right) = pairs[index];
            json!({
                "name": name,
                "independencePair": [cases[left].name, cases[right].name]
            })
        })
        .collect::<Vec<_>>();
    let rows = cases
        .iter()
        .map(|case| {
            json!({
                "name": case.name,
                "conditions": case.conditions.iter().copied().collect::<Vec<_>>(),
                "outcome": case.outcome
            })
        })
        .collect::<Vec<_>>();
    let report = json!({
        "schemaVersion": 1,
        "criterion": "unique-cause-mcdc",
        "decisionId": decision_id,
        "conditionCount": N,
        "caseCount": cases.len(),
        "conditions": conditions,
        "cases": rows
    });
    let bytes = serde_json::to_vec_pretty(&report)
        .unwrap_or_else(|error| panic!("{decision_id}: cannot encode MC/DC report: {error}"));
    fs::write(directory.join(format!("{decision_id}.json")), bytes)
        .unwrap_or_else(|error| panic!("{decision_id}: cannot write MC/DC report: {error}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_unique_cause_proof() {
        let cases = [
            McdcCase::new("baseline", [false, false], false),
            McdcCase::new("left", [true, false], true),
            McdcCase::new("right", [false, true], true),
        ];
        assert_unique_cause_mcdc("harness.accepts", ["left", "right"], &cases);
    }

    #[test]
    #[should_panic(expected = "has no unique-cause independence pair")]
    fn rejects_condition_toggling_that_cannot_change_the_decision() {
        let cases = [
            McdcCase::new("baseline", [false, false], false),
            McdcCase::new("left", [true, false], true),
            McdcCase::new("masked", [true, true], true),
        ];
        assert_unique_cause_mcdc("harness.rejects", ["left", "right"], &cases);
    }
}
