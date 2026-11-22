use crate::{QueryError, QueryErrorCode};

/// One Store predicate operator (`api/store/v1`, "Queries").
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreOperator {
    /// `eq`
    Equal,
    /// `ne`
    NotEqual,
    /// `lt`
    LessThan,
    /// `lte`
    LessThanOrEqual,
    /// `gt`
    GreaterThan,
    /// `gte`
    GreaterThanOrEqual,
    /// `in`
    In,
    /// `isNull`
    IsNull,
    /// `isNotNull`
    IsNotNull,
    /// `like`
    Like,
    /// `match`
    Match,
}

impl StoreOperator {
    /// The operator a wire name names.
    ///
    /// # Errors
    ///
    /// `EQUERY_INVALID` for a name that is not a Store operator.
    pub fn parse(name: &str) -> Result<Self, QueryError> {
        match name {
            "eq" => Ok(Self::Equal),
            "ne" => Ok(Self::NotEqual),
            "lt" => Ok(Self::LessThan),
            "lte" => Ok(Self::LessThanOrEqual),
            "gt" => Ok(Self::GreaterThan),
            "gte" => Ok(Self::GreaterThanOrEqual),
            "in" => Ok(Self::In),
            "isNull" => Ok(Self::IsNull),
            "isNotNull" => Ok(Self::IsNotNull),
            "like" => Ok(Self::Like),
            "match" => Ok(Self::Match),
            other => Err(QueryError::new(
                QueryErrorCode::InvalidQuery,
                format!("\"{other}\" is not a Store operator"),
            )),
        }
    }

    /// The operator's wire name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Equal => "eq",
            Self::NotEqual => "ne",
            Self::LessThan => "lt",
            Self::LessThanOrEqual => "lte",
            Self::GreaterThan => "gt",
            Self::GreaterThanOrEqual => "gte",
            Self::In => "in",
            Self::IsNull => "isNull",
            Self::IsNotNull => "isNotNull",
            Self::Like => "like",
            Self::Match => "match",
        }
    }

    /// The structured-query operator the prefix index answers for this
    /// operator, when there is one.
    #[must_use]
    pub const fn index_operator(self) -> Option<&'static str> {
        match self {
            Self::Equal => Some("$eq"),
            Self::LessThan => Some("$lt"),
            Self::LessThanOrEqual => Some("$lte"),
            Self::GreaterThan => Some("$gt"),
            Self::GreaterThanOrEqual => Some("$gte"),
            _ => None,
        }
    }
}
