use serde_json::{Map, Value as Json};
use turso_core::Value;
use turso_ext::{ConstraintInfo, ConstraintOp, ConstraintUsage, IndexInfo};

use super::value;

/// How one scan of a collection table reads FYLO, chosen by `best_index` and
/// carried to `filter` in the plan's `idx_str`.
///
/// Turso 0.8.3 passes only `=`, `<`, `<=`, `>`, `>=` and `!=` constraints to a
/// virtual table. `=` on `id` or a field column, and the four ranges with a
/// numeric value on a field column, are pushed down: `id =` reads one
/// document, and field terms become a FYLO query answered from its
/// zero-payload indexes. Every pushed constraint is
/// still checked again by the engine (`omit` stays false), so a pushed plan
/// can narrow the rows FYLO reads but never decides the answer alone.
#[derive(Debug, Default)]
pub(crate) struct ScanPlan {
    terms: Vec<Term>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Term {
    column: u32,
    operator: Operator,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operator {
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

/// What a planned scan asks FYLO for.
pub(crate) enum Lookup {
    /// Every document.
    All,
    /// One document by its TTID.
    Id(String),
    /// No document can match (two different `id =` values).
    Nothing,
    /// The documents matching a FYLO query (`api/query/v1`).
    Query(Json),
}

impl Operator {
    fn of(operator: ConstraintOp) -> Option<Self> {
        match operator {
            ConstraintOp::Eq => Some(Self::Eq),
            ConstraintOp::Lt => Some(Self::Lt),
            ConstraintOp::Le => Some(Self::Le),
            ConstraintOp::Gt => Some(Self::Gt),
            ConstraintOp::Ge => Some(Self::Ge),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [Self::Eq, Self::Lt, Self::Le, Self::Gt, Self::Ge]
            .into_iter()
            .find(|operator| operator.name() == name)
    }

    const fn fylo(self) -> &'static str {
        match self {
            Self::Eq => "$eq",
            Self::Lt => "$lt",
            Self::Le => "$lte",
            Self::Gt => "$gt",
            Self::Ge => "$gte",
        }
    }
}

impl ScanPlan {
    const ID_COLUMN: u32 = 0;
    const FIRST_FIELD_COLUMN: u32 = 2;
    const COST_ID: f64 = 1.0;
    const COST_EQUALITY: f64 = 10.0;
    const COST_RANGE: f64 = 1_000.0;
    const COST_SCAN: f64 = 1_000_000.0;

    /// Choose the pushed terms among `constraints`, for a table whose field
    /// columns are `2..2 + fields`. With `pushdown` off every scan reads the
    /// whole collection.
    pub(crate) fn choose(
        constraints: &[ConstraintInfo],
        fields: usize,
        pushdown: bool,
    ) -> IndexInfo {
        let last_field = Self::FIRST_FIELD_COLUMN + u32::try_from(fields).unwrap_or(u32::MAX);
        let mut terms = Vec::new();
        let mut usages = Vec::with_capacity(constraints.len());
        for constraint in constraints {
            let pushable = pushdown
                && constraint.usable
                && (constraint.column_index == Self::ID_COLUMN
                    || (Self::FIRST_FIELD_COLUMN..last_field).contains(&constraint.column_index));
            let operator = Operator::of(constraint.op).filter(|_| pushable);
            let operator = operator.filter(|operator| {
                constraint.column_index != Self::ID_COLUMN || *operator == Operator::Eq
            });
            let argv_index = operator.map(|operator| {
                terms.push(Term {
                    column: constraint.column_index,
                    operator,
                });
                u32::try_from(terms.len()).unwrap_or(u32::MAX)
            });
            usages.push(ConstraintUsage {
                argv_index,
                omit: false,
            });
        }
        let plan = Self { terms };
        let (cost, rows) = plan.estimate();
        IndexInfo {
            idx_num: i32::try_from(plan.terms.len()).unwrap_or(i32::MAX),
            idx_str: Some(plan.encode()),
            order_by_consumed: false,
            estimated_cost: cost,
            estimated_rows: rows,
            constraint_usages: usages,
        }
    }

    fn estimate(&self) -> (f64, u32) {
        let has = |test: fn(&Term) -> bool| self.terms.iter().any(test);
        if has(|term| term.column == Self::ID_COLUMN) {
            (Self::COST_ID, 1)
        } else if has(|term| term.operator == Operator::Eq) {
            (Self::COST_EQUALITY, 10)
        } else if !self.terms.is_empty() {
            (Self::COST_RANGE, 1_000)
        } else {
            (Self::COST_SCAN, 1_000_000)
        }
    }

    fn encode(&self) -> String {
        self.terms
            .iter()
            .map(|term| format!("{}:{}", term.column, term.operator.name()))
            .collect::<Vec<_>>()
            .join(",")
    }

    pub(crate) fn decode(text: Option<&str>) -> Self {
        let terms = text
            .unwrap_or_default()
            .split(',')
            .filter_map(|term| {
                let (column, operator) = term.split_once(':')?;
                Some(Term {
                    column: column.parse().ok()?,
                    operator: Operator::parse(operator)?,
                })
            })
            .collect();
        Self { terms }
    }

    /// What to read, given the values `filter` received for the pushed terms
    /// (in `argv` order) and the table's field names. A value with no JSON
    /// form, or a `NULL`, is not pushed: the engine's recheck decides it.
    pub(crate) fn lookup(&self, arguments: &[Value], fields: &[String]) -> Lookup {
        let mut identifier: Option<String> = None;
        let mut conditions: Map<String, Json> = Map::new();
        for (term, argument) in self.terms.iter().zip(arguments) {
            if term.column == Self::ID_COLUMN {
                let Value::Text(text) = argument else {
                    return Lookup::Nothing;
                };
                match &identifier {
                    Some(known) if known != text.as_str() => return Lookup::Nothing,
                    _ => identifier = Some(text.as_str().to_owned()),
                }
                continue;
            }
            let Some(field) = term
                .column
                .checked_sub(Self::FIRST_FIELD_COLUMN)
                .and_then(|index| fields.get(usize::try_from(index).ok()?))
            else {
                continue;
            };
            let Some(scalar) = value::to_json(argument).filter(|scalar| !scalar.is_null()) else {
                continue;
            };
            // FYLO's range operators compare numbers; a text range is left to
            // the engine's recheck over a wider read.
            if term.operator != Operator::Eq && !scalar.is_number() {
                continue;
            }
            let condition = conditions
                .entry(field.clone())
                .or_insert_with(|| Json::Object(Map::new()));
            if let Json::Object(operators) = condition {
                operators.insert(term.operator.fylo().to_owned(), scalar);
            }
        }
        match (identifier, conditions.is_empty()) {
            (Some(identifier), _) => Lookup::Id(identifier),
            (None, true) => Lookup::All,
            (None, false) => Lookup::Query(serde_json::json!({ "$ops": [conditions] })),
        }
    }
}
