use serde_json::{Map, Value};

use crate::index::{encode_field_path, sortable_float64};
use crate::{FieldValue, IndexKeyRange, matches_operand, numeric_value};

/// The numeric bounds a field's comparison operators put on its sortable
/// `n` index keys.
///
/// Both bounds are inclusive, so a range can hold extra documents at an
/// exclusive bound. The predicate still decides every candidate document.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumericRange {
    lower: Option<f64>,
    upper: Option<f64>,
}

impl NumericRange {
    const LOWER_OPERATORS: [&str; 2] = ["$gt", "$gte"];
    const UPPER_OPERATORS: [&str; 2] = ["$lt", "$lte"];
    const NUMERIC_KEY_KIND: &str = "n";

    /// The range of one field's `$gt`, `$gte`, `$lt` and `$lte` operators.
    ///
    /// Returns `None` when the index cannot answer them: there is no
    /// comparison operator, a bound is not a number, or an empty array
    /// satisfies the operators. An empty array compares as zero but has no
    /// index key, so only reading the documents can find it.
    #[must_use]
    pub fn from_operators(operators: &Map<String, Value>) -> Option<Self> {
        Self::require_no_empty_array_match(operators)?;
        let comparisons = Self::comparisons(operators)?;
        let range = Self {
            lower: Self::tightest(&comparisons, &Self::LOWER_OPERATORS, f64::max),
            upper: Self::tightest(&comparisons, &Self::UPPER_OPERATORS, f64::min),
        };
        range.require_a_bound()
    }

    /// The index keys of `field_path` whose number lies in the range.
    ///
    /// `field_path` uses `/` between segments, as index keys do.
    #[must_use]
    pub fn key_range(&self, field_path: &str) -> IndexKeyRange {
        let prefix = format!(
            "{}/{}/",
            encode_field_path(field_path),
            Self::NUMERIC_KEY_KIND
        );
        let start = self.start_key(&prefix);
        let end = self.end_key(&prefix);
        IndexKeyRange::new(start, end)
    }

    fn require_no_empty_array_match(operators: &Map<String, Value>) -> Option<()> {
        let empty_array = Value::Array(Vec::new());
        let matches = matches_operand(FieldValue::Present(&empty_array), operators);
        (!matches).then_some(())
    }

    /// Each comparison operator with its numeric bound, or `None` when a
    /// bound is not a number.
    fn comparisons(operators: &Map<String, Value>) -> Option<Vec<(&str, f64)>> {
        let mut comparisons = Vec::new();
        for (operator, expected) in Self::comparison_operators(operators) {
            comparisons.push((operator.as_str(), Self::bound(expected)?));
        }
        Some(comparisons)
    }

    fn comparison_operators(
        operators: &Map<String, Value>,
    ) -> impl Iterator<Item = (&String, &Value)> {
        operators
            .iter()
            .filter(|(operator, _)| Self::is_comparison(operator))
    }

    fn bound(expected: &Value) -> Option<f64> {
        numeric_value(FieldValue::Present(expected))
    }

    fn is_comparison(operator: &str) -> bool {
        let lower = Self::LOWER_OPERATORS.contains(&operator);
        let upper = Self::UPPER_OPERATORS.contains(&operator);
        lower | upper
    }

    fn tightest(
        comparisons: &[(&str, f64)],
        operators: &[&str],
        choose: fn(f64, f64) -> f64,
    ) -> Option<f64> {
        let mut tightest: Option<f64> = None;
        for (operator, bound) in comparisons {
            if operators.contains(operator) {
                tightest = Some(tightest.map_or(*bound, |current| choose(current, *bound)));
            }
        }
        tightest
    }

    fn require_a_bound(self) -> Option<Self> {
        let bounded = self.lower.is_some() | self.upper.is_some();
        bounded.then_some(self)
    }

    fn start_key(&self, prefix: &str) -> Vec<u8> {
        match self.lower {
            Some(lower) => format!("{prefix}{}", sortable_float64(Self::lowest_zero(lower))),
            None => prefix.to_owned(),
        }
        .into_bytes()
    }

    fn end_key(&self, prefix: &str) -> Vec<u8> {
        let last_prefix = match self.upper {
            Some(upper) => format!("{prefix}{}/", sortable_float64(Self::highest_zero(upper))),
            None => prefix.to_owned(),
        };
        IndexKeyRange::prefix(&last_prefix).end().to_vec()
    }

    /// Negative zero sorts just below positive zero in the index, and both
    /// compare equal to zero.
    fn lowest_zero(value: f64) -> f64 {
        if value == 0.0 {
            return -0.0;
        }
        value
    }

    fn highest_zero(value: f64) -> f64 {
        if value == 0.0 {
            return 0.0;
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::NumericRange;
    use crate::index_entries_for_document;
    use crate::{IndexKeyRange, IndexLookupValue};

    fn range(operators: &Value) -> Option<NumericRange> {
        NumericRange::from_operators(operators.as_object().unwrap())
    }

    fn numeric_keys(value: &Value) -> Vec<String> {
        let fields = json!({ "score": value });
        index_entries_for_document("4V0001", fields.as_object().unwrap(), |_, value| {
            Ok::<_, std::convert::Infallible>(IndexLookupValue::plain(value))
        })
        .unwrap()
        .into_iter()
        .filter(|key| key.starts_with("score/n/"))
        .collect()
    }

    fn holds(key_range: &IndexKeyRange, value: &Value) -> bool {
        numeric_keys(value)
            .iter()
            .any(|key| key_range.contains(key.as_bytes()))
    }

    #[test]
    fn a_range_holds_every_value_between_its_bounds() {
        let key_range = range(&json!({ "$gte": 5, "$lt": 7 }))
            .unwrap()
            .key_range("score");
        assert!(holds(&key_range, &json!(5)));
        assert!(holds(&key_range, &json!(6.5)));
        assert!(holds(&key_range, &json!("6")));
        assert!(holds(&key_range, &json!([6])));
        assert!(holds(&key_range, &json!(7)));
        assert!(!holds(&key_range, &json!(4.99)));
        assert!(!holds(&key_range, &json!(7.01)));
        assert!(!holds(&key_range, &json!(-6)));
    }

    #[test]
    fn negative_and_open_ended_ranges_follow_numeric_order() {
        let below = range(&json!({ "$lt": -2 })).unwrap().key_range("score");
        assert!(holds(&below, &json!(-1000)));
        assert!(holds(&below, &json!(-2)));
        assert!(!holds(&below, &json!(-1)));
        let above = range(&json!({ "$gt": 0 })).unwrap().key_range("score");
        assert!(holds(&above, &json!(0.5)));
        assert!(holds(&above, &json!(true)));
        assert!(!holds(&above, &json!(-0.5)));
    }

    #[test]
    fn the_tightest_bound_on_each_side_wins() {
        let key_range = range(&json!({ "$gte": 1, "$gt": 3, "$lte": 9, "$lt": 8 }))
            .unwrap()
            .key_range("score");
        assert!(holds(&key_range, &json!(3)));
        assert!(holds(&key_range, &json!(8)));
        assert!(!holds(&key_range, &json!(2)));
        assert!(!holds(&key_range, &json!(8.5)));
    }

    #[test]
    fn nested_paths_use_encoded_index_segments() {
        let key_range = range(&json!({ "$gte": 1 })).unwrap().key_range("a b/c");
        assert!(key_range.start().starts_with(b"a%20b/c/n/"));
    }

    #[test]
    fn ranges_the_index_cannot_answer_are_refused() {
        for operators in [
            json!({ "$eq": 1 }),
            json!({ "$ne": 1 }),
            json!({ "$gte": "abc" }),
            json!({ "$gte": {} }),
            json!({ "$gte": 0 }),
            json!({ "$gt": -1, "$lt": 1 }),
            json!({ "$lte": 0 }),
            json!({ "$gt": -1, "$ne": 5 }),
        ] {
            assert_eq!(range(&operators), None, "{operators}");
        }
    }

    #[test]
    fn coerced_bounds_follow_the_predicate() {
        for (operators, expected) in [
            (json!({ "$gte": "5" }), json!(5)),
            (json!({ "$gte": true }), json!(1)),
            (json!({ "$gte": [5] }), json!(5)),
        ] {
            let key_range = range(&operators).unwrap().key_range("score");
            assert!(holds(&key_range, &expected), "{operators}");
        }
    }
}
