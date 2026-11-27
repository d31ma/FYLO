use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::{QueryError, QueryErrorCode, QueryLimits};

/// A deterministic plan for BYCKR's currently supported SQL subset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SqlPlan {
    /// SQL with any `EXPLAIN` prefix removed.
    pub sql: String,
    /// Parsed operation.
    pub operation: SqlOperation,
    /// Primary collection routed by the statement.
    pub collection: String,
    /// Compatibility AST consumed by the JavaScript execution paths.
    pub ast: Value,
    /// Whether the source requested an execution plan.
    pub explain: bool,
    /// Whether the source requested execution statistics.
    pub analyze: bool,
    /// Deterministic access-path description.
    pub access: Vec<AccessPath>,
}

/// SQL operations recognized by BYCKR.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SqlOperation {
    /// Create a collection.
    #[serde(rename = "CREATE")]
    Create,
    /// Drop a collection.
    #[serde(rename = "DROP")]
    Drop,
    /// Read documents.
    #[serde(rename = "SELECT")]
    Select,
    /// Insert a document.
    #[serde(rename = "INSERT")]
    Insert,
    /// Update matching documents.
    #[serde(rename = "UPDATE")]
    Update,
    /// Delete matching documents.
    #[serde(rename = "DELETE")]
    Delete,
}

/// One query access path exposed by `EXPLAIN`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AccessPath {
    /// No indexable predicate was found.
    DocumentScan,
    /// A field/operator combination can use the prefix index.
    PrefixIndex {
        /// Field path in slash notation.
        field: String,
        /// Operators attached to the field.
        operators: Vec<String>,
    },
    /// A predicate must be checked against document content.
    DocumentFilter {
        /// Field path in slash notation.
        field: String,
        /// Operators attached to the field.
        operators: Vec<String>,
    },
}

/// Parse and plan a bounded SQL statement.
///
/// The AST intentionally matches `src/query/parser.js`; the JavaScript engine
/// remains the compatibility oracle during the strangler migration.
///
/// # Errors
///
/// Returns a stable query error for an empty, oversized, unsupported, or
/// malformed statement.
pub fn prepare_sql(input: &str, limits: QueryLimits) -> Result<SqlPlan, QueryError> {
    if input.len() > limits.max_input_bytes {
        return Err(QueryError::new(
            QueryErrorCode::InputTooLarge,
            format!(
                "SQL input contains {} bytes; limit is {}",
                input.len(),
                limits.max_input_bytes
            ),
        ));
    }
    let (sql, explain, analyze) = strip_explain(input)?;
    let tokens = Lexer::new(&sql).tokenize(limits)?;
    let operation = operation(tokens.first())?;
    let mut parser = SqlParser::new(tokens, limits);
    let ast = parser.parse(operation)?;
    if !parser.at_end() {
        return Err(unsupported_sql("more than one statement"));
    }
    let collection = ast
        .get("$collection")
        .or_else(|| ast.get("$leftCollection"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let query = if operation == SqlOperation::Update {
        ast.get("$where")
    } else {
        Some(&ast)
    };
    let access = match query {
        Some(value) => access_paths(value, limits)?,
        None => vec![AccessPath::DocumentScan],
    };
    Ok(SqlPlan {
        sql,
        operation,
        collection,
        ast,
        explain,
        analyze,
        access,
    })
}

/// The placeholders a SQL statement names, grouped by kind.
///
/// `?` and `?NNN` are positional (1-based); `:name` and `$name` are named.
/// A statement uses one kind only (ADR 0035, `api/sql/v1`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SqlParameters {
    /// 1-based positional placeholder indices (`?` and `?NNN`).
    pub positional: BTreeSet<usize>,
    /// Named placeholders (`:name` and `$name`), without the prefix.
    pub named: BTreeSet<String>,
}

impl SqlParameters {
    /// Whether the statement names no placeholders at all.
    pub fn is_empty(&self) -> bool {
        self.positional.is_empty() && self.named.is_empty()
    }

    /// Whether the statement mixes positional and named placeholders.
    pub fn mixes_kinds(&self) -> bool {
        !self.positional.is_empty() && !self.named.is_empty()
    }
}

/// Scan a SQL statement for its placeholders, skipping string literals and
/// quoted identifiers. The `EXPLAIN` prefix must already be stripped.
///
/// `?` is a sequential positional placeholder (the first is `1`, the next
/// `2`, and so on); `?NNN` names a specific 1-based position. `:name` and
/// `$name` are named placeholders.
pub fn scan_parameters(sql: &str) -> SqlParameters {
    let bytes = sql.as_bytes();
    let mut positional = BTreeSet::new();
    let mut named = BTreeSet::new();
    let mut next_bare = 1usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == quote {
                        if index + 1 < bytes.len() && bytes[index + 1] == quote {
                            index += 2;
                            continue;
                        }
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            b'?' => {
                let start = index + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end > start {
                    if let Some(number) = std::str::from_utf8(&bytes[start..end])
                        .unwrap_or("")
                        .parse::<usize>()
                        .ok()
                        .filter(|number| *number > 0)
                    {
                        positional.insert(number);
                    }
                } else {
                    positional.insert(next_bare);
                    next_bare += 1;
                }
                index = end;
            }
            b':' | b'$' => {
                let start = index + 1;
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
                if end > start {
                    named.insert(
                        std::str::from_utf8(&bytes[start..end])
                            .unwrap_or("")
                            .to_owned(),
                    );
                }
                index = end;
            }
            _ => index += 1,
        }
    }
    SqlParameters { positional, named }
}

fn strip_explain(input: &str) -> Result<(String, bool, bool), QueryError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(QueryError::new(
            QueryErrorCode::InvalidQuery,
            "SQL statement must be a non-empty string",
        ));
    }
    let mut words = trimmed.split_whitespace();
    if !words
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("EXPLAIN"))
    {
        return Ok((trimmed.to_owned(), false, false));
    }
    let remainder = trimmed["EXPLAIN".len()..].trim_start();
    let (remainder, analyze) = if remainder
        .get(.."ANALYZE".len())
        .is_some_and(|word| word.eq_ignore_ascii_case("ANALYZE"))
        && remainder["ANALYZE".len()..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
    {
        (remainder["ANALYZE".len()..].trim_start(), true)
    } else {
        (remainder, false)
    };
    if remainder.is_empty() {
        return Err(QueryError::new(
            QueryErrorCode::InvalidQuery,
            "EXPLAIN requires a SQL statement",
        ));
    }
    Ok((remainder.to_owned(), true, analyze))
}

fn operation(token: Option<&Token>) -> Result<SqlOperation, QueryError> {
    match token.map(|token| &token.kind) {
        Some(TokenKind::Create) => Ok(SqlOperation::Create),
        Some(TokenKind::Drop) => Ok(SqlOperation::Drop),
        Some(TokenKind::Select) => Ok(SqlOperation::Select),
        Some(TokenKind::Insert) => Ok(SqlOperation::Insert),
        Some(TokenKind::Update) => Ok(SqlOperation::Update),
        Some(TokenKind::Delete) => Ok(SqlOperation::Delete),
        Some(TokenKind::Begin) => Err(unsupported_sql("BEGIN")),
        Some(TokenKind::Commit) => Err(unsupported_sql("COMMIT")),
        Some(TokenKind::Alter) => Err(unsupported_sql("ALTER")),
        _ => Err(QueryError::new(
            QueryErrorCode::InvalidQuery,
            "unsupported or missing SQL operation",
        )),
    }
}

/// A statement or clause outside the `api/sql/v1` subset.
fn unsupported_sql(clause: &str) -> QueryError {
    QueryError::new(
        QueryErrorCode::UnsupportedSql,
        format!("SQL clause `{clause}` is outside the api/sql/v1 subset"),
    )
}

fn access_paths(value: &Value, limits: QueryLimits) -> Result<Vec<AccessPath>, QueryError> {
    let Some(operations) = value.get("$ops").and_then(Value::as_array) else {
        return Ok(vec![AccessPath::DocumentScan]);
    };
    let mut access = Vec::new();
    for operation in operations {
        let Some(fields) = operation.as_object() else {
            continue;
        };
        for (field, operand) in fields {
            let Some(operators) = operand.as_object() else {
                continue;
            };
            if access.len() >= limits.max_queries {
                return Err(QueryError::new(
                    QueryErrorCode::TooManyQueries,
                    "SQL plan contains too many access paths",
                ));
            }
            let operators: Vec<String> = operators.keys().cloned().collect();
            let indexable = operators.iter().any(|operator| {
                matches!(
                    operator.as_str(),
                    "$eq" | "$gt" | "$gte" | "$lt" | "$lte" | "$like" | "$contains"
                )
            });
            access.push(if indexable {
                AccessPath::PrefixIndex {
                    field: field.clone(),
                    operators,
                }
            } else {
                AccessPath::DocumentFilter {
                    field: field.clone(),
                    operators,
                }
            });
        }
    }
    if access.is_empty() {
        access.push(AccessPath::DocumentScan);
    }
    Ok(access)
}

#[derive(Clone, Debug, PartialEq)]
struct Token {
    kind: TokenKind,
}

#[derive(Clone, Debug, PartialEq)]
enum TokenKind {
    Create,
    Drop,
    Select,
    From,
    Where,
    Insert,
    Into,
    Values,
    Update,
    Set,
    Delete,
    Join,
    Inner,
    Left,
    Right,
    Outer,
    On,
    Group,
    By,
    Limit,
    And,
    Or,
    Equals,
    NotEquals,
    GreaterThan,
    LessThan,
    GreaterEqual,
    LessEqual,
    Like,
    Order,
    Offset,
    Count,
    In,
    Is,
    Not,
    Asc,
    Desc,
    Having,
    Begin,
    Commit,
    Alter,
    Index,
    View,
    Distinct,
    Union,
    Exists,
    As,
    Transaction,
    Parameter(Option<usize>),
    NamedParameter(String),
    Identifier(String),
    String(String),
    Number(String),
    Boolean(bool),
    Null,
    Comma,
    LeftParenthesis,
    RightParenthesis,
    Asterisk,
    End,
}

struct Lexer {
    input: Vec<char>,
    position: usize,
}

impl Lexer {
    fn new(input: &str) -> Self {
        Self {
            input: input.chars().collect(),
            position: 0,
        }
    }

    fn tokenize(mut self, limits: QueryLimits) -> Result<Vec<Token>, QueryError> {
        let mut tokens = Vec::new();
        while let Some(character) = self.current() {
            if character.is_whitespace() {
                self.position += 1;
                continue;
            }
            let kind = if matches!(character, '\'' | '"') {
                self.read_string(character)?
            } else if character.is_ascii_digit() {
                Some(self.read_number())
            } else if character.is_ascii_alphabetic() || character == '_' {
                self.read_word(limits)?
            } else {
                self.read_operator(character)
            };
            if let Some(kind) = kind {
                tokens.push(Token { kind });
                if tokens.len() > limits.max_queries.saturating_mul(32) {
                    return Err(QueryError::new(
                        QueryErrorCode::TooManyQueries,
                        "SQL statement contains too many tokens",
                    ));
                }
            }
        }
        tokens.push(Token {
            kind: TokenKind::End,
        });
        Ok(tokens)
    }

    fn current(&self) -> Option<char> {
        self.input.get(self.position).copied()
    }

    fn read_string(&mut self, quote: char) -> Result<Option<TokenKind>, QueryError> {
        self.position += 1;
        let mut value = String::new();
        loop {
            match self.current() {
                Some(character) if character != quote => {
                    value.push(character);
                    self.position += 1;
                }
                Some(_) if self.input.get(self.position + 1) == Some(&quote) => {
                    value.push(quote);
                    self.position += 2;
                }
                Some(_) => {
                    self.position += 1;
                    return Ok(Some(TokenKind::String(value)));
                }
                None => {
                    return Err(QueryError::new(
                        QueryErrorCode::InvalidQuery,
                        "unterminated SQL string",
                    ));
                }
            }
        }
    }

    fn read_number(&mut self) -> TokenKind {
        let start = self.position;
        while self
            .current()
            .is_some_and(|character| character.is_ascii_digit() || character == '.')
        {
            self.position += 1;
        }
        TokenKind::Number(self.input[start..self.position].iter().collect())
    }

    fn read_word(&mut self, limits: QueryLimits) -> Result<Option<TokenKind>, QueryError> {
        let mut value = self.read_identifier();
        while self.current() == Some('.')
            && self
                .input
                .get(self.position + 1)
                .is_some_and(|next| next.is_ascii_alphabetic() || *next == '_')
        {
            self.position += 1;
            value.push('/');
            value.push_str(&self.read_identifier());
        }
        if value.len() > limits.max_term_bytes {
            return Err(QueryError::new(
                QueryErrorCode::TermTooLarge,
                "SQL identifier exceeds the configured byte limit",
            ));
        }
        Ok(Some(keyword_or_identifier(value)))
    }

    fn read_identifier(&mut self) -> String {
        let start = self.position;
        while self.current().is_some_and(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
        }) {
            self.position += 1;
        }
        self.input[start..self.position].iter().collect()
    }

    fn read_operator(&mut self, character: char) -> Option<TokenKind> {
        self.position += 1;
        match character {
            '=' => Some(TokenKind::Equals),
            ',' => Some(TokenKind::Comma),
            '(' => Some(TokenKind::LeftParenthesis),
            ')' => Some(TokenKind::RightParenthesis),
            '*' => Some(TokenKind::Asterisk),
            '>' if self.current() == Some('=') => {
                self.position += 1;
                Some(TokenKind::GreaterEqual)
            }
            '>' => Some(TokenKind::GreaterThan),
            '<' if self.current() == Some('=') => {
                self.position += 1;
                Some(TokenKind::LessEqual)
            }
            '<' => Some(TokenKind::LessThan),
            '!' if self.current() == Some('=') => {
                self.position += 1;
                Some(TokenKind::NotEquals)
            }
            '?' => {
                let start = self.position;
                while self
                    .current()
                    .is_some_and(|character| character.is_ascii_digit())
                {
                    self.position += 1;
                }
                let index = if self.position > start {
                    self.input[start..self.position]
                        .iter()
                        .collect::<String>()
                        .parse::<usize>()
                        .ok()
                        .filter(|number| *number > 0)
                } else {
                    None
                };
                Some(TokenKind::Parameter(index))
            }
            ':' | '$' => {
                let start = self.position;
                while self
                    .current()
                    .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
                {
                    self.position += 1;
                }
                Some(TokenKind::NamedParameter(
                    self.input[start..self.position].iter().collect(),
                ))
            }
            _ => None,
        }
    }
}

fn keyword_or_identifier(value: String) -> TokenKind {
    match value.to_ascii_uppercase().as_str() {
        "CREATE" => TokenKind::Create,
        "DROP" => TokenKind::Drop,
        "SELECT" => TokenKind::Select,
        "FROM" => TokenKind::From,
        "WHERE" => TokenKind::Where,
        "INSERT" => TokenKind::Insert,
        "INTO" => TokenKind::Into,
        "VALUES" => TokenKind::Values,
        "UPDATE" => TokenKind::Update,
        "SET" => TokenKind::Set,
        "DELETE" => TokenKind::Delete,
        "JOIN" => TokenKind::Join,
        "INNER" => TokenKind::Inner,
        "LEFT" => TokenKind::Left,
        "RIGHT" => TokenKind::Right,
        "OUTER" => TokenKind::Outer,
        "ON" => TokenKind::On,
        "GROUP" => TokenKind::Group,
        "BY" => TokenKind::By,
        "LIMIT" => TokenKind::Limit,
        "AND" => TokenKind::And,
        "OR" => TokenKind::Or,
        "LIKE" => TokenKind::Like,
        "ORDER" => TokenKind::Order,
        "OFFSET" => TokenKind::Offset,
        "COUNT" => TokenKind::Count,
        "IN" => TokenKind::In,
        "IS" => TokenKind::Is,
        "NOT" => TokenKind::Not,
        "ASC" => TokenKind::Asc,
        "DESC" => TokenKind::Desc,
        "HAVING" => TokenKind::Having,
        "BEGIN" => TokenKind::Begin,
        "COMMIT" => TokenKind::Commit,
        "ALTER" => TokenKind::Alter,
        "INDEX" => TokenKind::Index,
        "VIEW" => TokenKind::View,
        "DISTINCT" => TokenKind::Distinct,
        "UNION" => TokenKind::Union,
        "EXISTS" => TokenKind::Exists,
        "AS" => TokenKind::As,
        "TRANSACTION" => TokenKind::Transaction,
        "TRUE" => TokenKind::Boolean(true),
        "FALSE" => TokenKind::Boolean(false),
        "NULL" => TokenKind::Null,
        _ => TokenKind::Identifier(value),
    }
}

struct SqlParser {
    tokens: Vec<Token>,
    position: usize,
    limits: QueryLimits,
    next_bare_parameter: usize,
}

impl SqlParser {
    fn new(tokens: Vec<Token>, limits: QueryLimits) -> Self {
        Self {
            tokens,
            position: 0,
            limits,
            next_bare_parameter: 1,
        }
    }

    fn parse(&mut self, operation: SqlOperation) -> Result<Value, QueryError> {
        match operation {
            SqlOperation::Create => self.parse_collection_ddl(&TokenKind::Create),
            SqlOperation::Drop => self.parse_collection_ddl(&TokenKind::Drop),
            SqlOperation::Select => self.parse_select(),
            SqlOperation::Insert => self.parse_insert(),
            SqlOperation::Update => self.parse_update(),
            SqlOperation::Delete => self.parse_delete(),
        }
    }

    fn current(&self) -> &TokenKind {
        &self.tokens[self.position].kind
    }

    fn advance(&mut self) {
        self.position = (self.position + 1).min(self.tokens.len() - 1);
    }

    /// Whether the parser has consumed every token of the statement, so only
    /// the terminating `End` token remains.
    fn at_end(&self) -> bool {
        matches!(self.current(), TokenKind::End)
    }

    fn expect(&mut self, expected: &TokenKind) -> Result<(), QueryError> {
        if std::mem::discriminant(self.current()) != std::mem::discriminant(expected) {
            return Err(QueryError::new(
                QueryErrorCode::InvalidQuery,
                "invalid SQL syntax",
            ));
        }
        self.advance();
        Ok(())
    }

    fn identifier(&mut self) -> Result<String, QueryError> {
        let TokenKind::Identifier(value) = self.current() else {
            return Err(QueryError::new(
                QueryErrorCode::InvalidQuery,
                "expected SQL identifier",
            ));
        };
        let value = value.clone();
        self.advance();
        Ok(value)
    }

    fn parse_collection_ddl(&mut self, operation: &TokenKind) -> Result<Value, QueryError> {
        self.expect(operation)?;
        if matches!(self.current(), TokenKind::Index) {
            return Err(unsupported_sql("CREATE INDEX"));
        }
        if matches!(self.current(), TokenKind::View) {
            return Err(unsupported_sql("CREATE VIEW"));
        }
        if matches!(self.current(), TokenKind::Identifier(_)) {
            self.advance();
        }
        let collection = self.identifier()?;
        Ok(object([("$collection", Value::String(collection))]))
    }

    fn parse_select(&mut self) -> Result<Value, QueryError> {
        self.expect(&TokenKind::Select)?;
        if matches!(self.current(), TokenKind::Distinct) {
            return Err(unsupported_sql("SELECT DISTINCT"));
        }
        let count = if matches!(self.current(), TokenKind::Count) {
            self.advance();
            self.expect(&TokenKind::LeftParenthesis)?;
            self.expect(&TokenKind::Asterisk)?;
            self.expect(&TokenKind::RightParenthesis)?;
            true
        } else {
            false
        };
        let columns = if count {
            Vec::new()
        } else {
            self.parse_columns()?
        };
        self.expect(&TokenKind::From)?;
        if matches!(self.current(), TokenKind::LeftParenthesis) {
            return Err(unsupported_sql("sub-query"));
        }
        let collection = self.identifier()?;
        if matches!(
            self.current(),
            TokenKind::Join
                | TokenKind::Inner
                | TokenKind::Left
                | TokenKind::Right
                | TokenKind::Outer
        ) {
            return self.parse_join(columns, collection);
        }
        let mut query = Map::new();
        query.insert("$collection".into(), Value::String(collection));
        if count {
            query.insert("$count".into(), Value::Bool(true));
        } else {
            if !columns.iter().any(|column| column == "*") {
                query.insert(
                    "$select".into(),
                    Value::Array(columns.iter().cloned().map(Value::String).collect()),
                );
            }
            query.insert(
                "$onlyIds".into(),
                Value::Bool(columns.iter().any(|column| column == "_id")),
            );
        }
        if matches!(self.current(), TokenKind::Where) {
            query.insert("$ops".into(), Value::Array(self.parse_where()?));
        }
        if matches!(self.current(), TokenKind::Group) {
            self.advance();
            self.expect(&TokenKind::By)?;
            query.insert("$groupby".into(), Value::String(self.identifier()?));
        }
        if matches!(self.current(), TokenKind::Order) {
            self.advance();
            self.expect(&TokenKind::By)?;
            query.insert("$sort".into(), self.parse_order_by()?);
        }
        if matches!(self.current(), TokenKind::Having) {
            return Err(unsupported_sql("HAVING"));
        }
        if matches!(self.current(), TokenKind::Limit) {
            self.advance();
            query.insert("$limit".into(), Value::Number(self.limit_number()?));
        }
        if matches!(self.current(), TokenKind::Offset) {
            self.advance();
            query.insert("$offset".into(), Value::Number(self.limit_number()?));
        }
        Ok(Value::Object(query))
    }

    /// Parse `ORDER BY <f> [ASC|DESC]` into a single-field `$sort` object.
    /// The `id` tie-breaker is already carried by the index sort keys, so
    /// only the named field and its direction are recorded here.
    fn parse_order_by(&mut self) -> Result<Value, QueryError> {
        let field = self.identifier()?;
        let direction = if matches!(self.current(), TokenKind::Asc) {
            self.advance();
            "asc"
        } else if matches!(self.current(), TokenKind::Desc) {
            self.advance();
            "desc"
        } else {
            "asc"
        };
        if matches!(self.current(), TokenKind::Comma) {
            return Err(unsupported_sql("ORDER BY with more than one field"));
        }
        let mut sort = Map::new();
        sort.insert(field, Value::String(direction.into()));
        Ok(Value::Object(sort))
    }

    fn parse_columns(&mut self) -> Result<Vec<String>, QueryError> {
        if matches!(self.current(), TokenKind::Asterisk) {
            self.advance();
            return Ok(vec!["*".into()]);
        }
        let mut columns = Vec::new();
        loop {
            let column = self.identifier()?;
            if matches!(self.current(), TokenKind::LeftParenthesis) {
                return Err(unsupported_sql("function"));
            }
            columns.push(column);
            if columns.len() > self.limits.max_queries {
                return Err(QueryError::new(
                    QueryErrorCode::TooManyQueries,
                    "SQL statement selects too many columns",
                ));
            }
            if !matches!(self.current(), TokenKind::Comma) {
                break;
            }
            self.advance();
        }
        Ok(columns)
    }

    fn parse_join(
        &mut self,
        columns: Vec<String>,
        left_collection: String,
    ) -> Result<Value, QueryError> {
        let mode = match self.current() {
            TokenKind::Left => "left",
            TokenKind::Right => "right",
            TokenKind::Outer => "outer",
            _ => "inner",
        };
        if !matches!(self.current(), TokenKind::Join) {
            self.advance();
        }
        self.expect(&TokenKind::Join)?;
        let right_collection = self.identifier()?;
        self.expect(&TokenKind::On)?;
        let conditions = self.parse_join_conditions()?;
        let mut query = Map::new();
        query.insert("$leftCollection".into(), Value::String(left_collection));
        query.insert("$rightCollection".into(), Value::String(right_collection));
        query.insert("$mode".into(), Value::String(mode.into()));
        query.insert("$on".into(), Value::Object(conditions));
        if !columns.iter().any(|column| column == "*") {
            query.insert(
                "$select".into(),
                Value::Array(columns.into_iter().map(Value::String).collect()),
            );
        }
        if matches!(self.current(), TokenKind::Where) {
            self.parse_where()?;
        }
        if matches!(self.current(), TokenKind::Group) {
            self.advance();
            self.expect(&TokenKind::By)?;
            query.insert("$groupby".into(), Value::String(self.identifier()?));
        }
        if matches!(self.current(), TokenKind::Limit) {
            self.advance();
            query.insert("$limit".into(), Value::Number(self.limit_number()?));
        }
        Ok(Value::Object(query))
    }

    fn parse_join_conditions(&mut self) -> Result<Map<String, Value>, QueryError> {
        let mut conditions = Map::new();
        let mut count = 0;
        loop {
            let left = self.identifier()?;
            let operator = self.parse_operator(false)?;
            let right = self.identifier()?;
            let operand = conditions
                .entry(left)
                .or_insert_with(|| Value::Object(Map::new()));
            operand
                .as_object_mut()
                .expect("join operands are created as objects")
                .insert(operator.into(), Value::String(right));
            count += 1;
            if count > self.limits.max_queries {
                return Err(QueryError::new(
                    QueryErrorCode::TooManyQueries,
                    "SQL join contains too many conditions",
                ));
            }
            if !matches!(self.current(), TokenKind::And) {
                break;
            }
            self.advance();
        }
        Ok(conditions)
    }

    fn parse_insert(&mut self) -> Result<Value, QueryError> {
        self.expect(&TokenKind::Insert)?;
        self.expect(&TokenKind::Into)?;
        let collection = self.identifier()?;
        let columns = if matches!(self.current(), TokenKind::LeftParenthesis) {
            self.advance();
            let columns = self.parse_columns()?;
            self.expect(&TokenKind::RightParenthesis)?;
            columns
        } else {
            Vec::new()
        };
        self.expect(&TokenKind::Values)?;
        self.expect(&TokenKind::LeftParenthesis)?;
        let mut values = Map::new();
        let mut index = 0;
        loop {
            let value = self.parse_value()?;
            let column = columns
                .get(index)
                .cloned()
                .unwrap_or_else(|| format!("col{index}"));
            values.insert(column, value);
            index += 1;
            if index > self.limits.max_queries {
                return Err(QueryError::new(
                    QueryErrorCode::TooManyQueries,
                    "SQL insert contains too many values",
                ));
            }
            if !matches!(self.current(), TokenKind::Comma) {
                break;
            }
            self.advance();
        }
        self.expect(&TokenKind::RightParenthesis)?;
        Ok(object([
            ("$collection", Value::String(collection)),
            ("$values", Value::Object(values)),
        ]))
    }

    fn parse_update(&mut self) -> Result<Value, QueryError> {
        self.expect(&TokenKind::Update)?;
        let collection = self.identifier()?;
        self.expect(&TokenKind::Set)?;
        let mut set = Map::new();
        loop {
            let column = self.identifier()?;
            self.expect(&TokenKind::Equals)?;
            set.insert(column, self.parse_value()?);
            if set.len() > self.limits.max_queries {
                return Err(QueryError::new(
                    QueryErrorCode::TooManyQueries,
                    "SQL update contains too many assignments",
                ));
            }
            if !matches!(self.current(), TokenKind::Comma) {
                break;
            }
            self.advance();
        }
        let mut update = Map::new();
        update.insert("$collection".into(), Value::String(collection.clone()));
        update.insert("$set".into(), Value::Object(set));
        if matches!(self.current(), TokenKind::Where) {
            update.insert(
                "$where".into(),
                object([
                    ("$collection", Value::String(collection)),
                    ("$ops", Value::Array(self.parse_where()?)),
                ]),
            );
        }
        Ok(Value::Object(update))
    }

    fn parse_delete(&mut self) -> Result<Value, QueryError> {
        self.expect(&TokenKind::Delete)?;
        self.expect(&TokenKind::From)?;
        let collection = self.identifier()?;
        let mut delete = Map::new();
        delete.insert("$collection".into(), Value::String(collection));
        if matches!(self.current(), TokenKind::Where) {
            delete.insert("$ops".into(), Value::Array(self.parse_where()?));
        }
        Ok(Value::Object(delete))
    }

    fn parse_where(&mut self) -> Result<Vec<Value>, QueryError> {
        self.expect(&TokenKind::Where)?;
        let mut disjunction = Vec::new();
        let mut conjunction = Map::new();
        loop {
            let field = self.identifier()?;
            let (operator, value) = self.parse_predicate()?;
            let operand = conjunction
                .entry(field)
                .or_insert_with(|| Value::Object(Map::new()));
            operand
                .as_object_mut()
                .expect("predicate operands are created as objects")
                .insert(operator.into(), value);
            if matches!(self.current(), TokenKind::And) {
                self.advance();
                continue;
            }
            if matches!(self.current(), TokenKind::Or) {
                disjunction.push(Value::Object(conjunction));
                conjunction = Map::new();
                self.advance();
                if disjunction.len() >= self.limits.max_queries {
                    return Err(QueryError::new(
                        QueryErrorCode::TooManyQueries,
                        "SQL WHERE contains too many disjunctions",
                    ));
                }
                continue;
            }
            break;
        }
        disjunction.push(Value::Object(conjunction));
        Ok(disjunction)
    }

    /// Parse one predicate's operator and value, after its field is known.
    /// Handles `IN (...)`, `IS [NOT] NULL`, and the comparison operators.
    fn parse_predicate(&mut self) -> Result<(&'static str, Value), QueryError> {
        match self.current() {
            TokenKind::In => {
                self.advance();
                self.expect(&TokenKind::LeftParenthesis)?;
                if matches!(self.current(), TokenKind::Select) {
                    return Err(unsupported_sql("sub-query"));
                }
                let mut values = Vec::new();
                loop {
                    values.push(self.parse_value()?);
                    if values.len() > 100 {
                        return Err(QueryError::new(
                            QueryErrorCode::QueryLimit,
                            "SQL IN clause has more than 100 values",
                        ));
                    }
                    if !matches!(self.current(), TokenKind::Comma) {
                        break;
                    }
                    self.advance();
                }
                self.expect(&TokenKind::RightParenthesis)?;
                Ok(("$in", Value::Array(values)))
            }
            TokenKind::Is => {
                self.advance();
                let negated = if matches!(self.current(), TokenKind::Not) {
                    self.advance();
                    true
                } else {
                    false
                };
                self.expect(&TokenKind::Null)?;
                Ok((
                    if negated { "$isnotnull" } else { "$isnull" },
                    Value::Bool(true),
                ))
            }
            _ => {
                let operator = self.parse_operator(true)?;
                let value = self.parse_value()?;
                Ok((operator, value))
            }
        }
    }

    fn parse_operator(&mut self, allow_like: bool) -> Result<&'static str, QueryError> {
        let operator = match self.current() {
            TokenKind::Equals => "$eq",
            TokenKind::NotEquals => "$ne",
            TokenKind::GreaterThan => "$gt",
            TokenKind::LessThan => "$lt",
            TokenKind::GreaterEqual => "$gte",
            TokenKind::LessEqual => "$lte",
            TokenKind::Like if allow_like => "$like",
            _ => {
                return Err(QueryError::new(
                    QueryErrorCode::InvalidQuery,
                    "unknown SQL operator",
                ));
            }
        };
        self.advance();
        Ok(operator)
    }

    fn parse_value(&mut self) -> Result<Value, QueryError> {
        if let TokenKind::Parameter(index) = self.current() {
            let index = (*index).unwrap_or_else(|| {
                let next = self.next_bare_parameter;
                self.next_bare_parameter += 1;
                next
            });
            self.advance();
            return Ok(parameter_marker(index));
        }
        if let TokenKind::NamedParameter(name) = self.current() {
            let name = name.clone();
            self.advance();
            return Ok(named_parameter_marker(&name));
        }
        let value = match self.current() {
            TokenKind::String(value) => Value::String(value.clone()),
            TokenKind::Number(value) => Value::Number(parse_number(value)?),
            TokenKind::Boolean(value) => Value::Bool(*value),
            TokenKind::Null => Value::Null,
            _ => {
                return Err(QueryError::new(
                    QueryErrorCode::InvalidQuery,
                    "unexpected SQL value",
                ));
            }
        };
        self.advance();
        Ok(value)
    }

    fn limit_number(&mut self) -> Result<Number, QueryError> {
        let TokenKind::Number(value) = self.current() else {
            return Err(QueryError::new(
                QueryErrorCode::InvalidQuery,
                "SQL LIMIT requires a number",
            ));
        };
        let integer = value.split('.').next().unwrap_or_default();
        let limit = integer.parse::<u64>().map_err(|_| {
            QueryError::new(QueryErrorCode::InvalidQuery, "invalid SQL LIMIT value")
        })?;
        self.advance();
        Ok(Number::from(limit))
    }
}

fn parse_number(value: &str) -> Result<Number, QueryError> {
    if !value.contains('.') {
        return value.parse::<u64>().map(Number::from).map_err(|_| {
            QueryError::new(QueryErrorCode::InvalidQuery, "invalid SQL numeric literal")
        });
    }
    let value = value.parse::<f64>().map_err(|_| {
        QueryError::new(QueryErrorCode::InvalidQuery, "invalid SQL numeric literal")
    })?;
    Number::from_f64(value)
        .ok_or_else(|| QueryError::new(QueryErrorCode::InvalidQuery, "invalid SQL number"))
}

fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

/// The AST marker for a positional placeholder (`?` or `?NNN`), resolved to
/// its 1-based index. The machine binds the value before execution.
fn parameter_marker(index: usize) -> Value {
    let mut inner = Map::new();
    inner.insert("positional".into(), Value::from(index as u64));
    let mut marker = Map::new();
    marker.insert("$param".into(), Value::Object(inner));
    Value::Object(marker)
}

/// The AST marker for a named placeholder (`:name` or `$name`).
fn named_parameter_marker(name: &str) -> Value {
    let mut inner = Map::new();
    inner.insert("named".into(), Value::String(name.to_owned()));
    let mut marker = Map::new();
    marker.insert("$param".into(), Value::Object(inner));
    Value::Object(marker)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn scans_positional_and_named_placeholders() {
        let none = scan_parameters("SELECT * FROM users");
        assert!(none.is_empty());

        let numbered = scan_parameters("SELECT * FROM users WHERE a = ?1 AND b = ?2");
        assert_eq!(numbered.positional, BTreeSet::from([1, 2]));
        assert!(numbered.named.is_empty());

        let bare = scan_parameters("SELECT * FROM users WHERE a = ? AND b = ?");
        assert_eq!(bare.positional, BTreeSet::from([1, 2]));

        let named = scan_parameters("SELECT * FROM users WHERE a = :x AND b = $y");
        assert_eq!(
            named.named,
            BTreeSet::from(["x".to_owned(), "y".to_owned()])
        );
        assert!(named.positional.is_empty());
    }

    #[test]
    fn scan_parameters_ignores_literals_and_quoted_identifiers() {
        let literals = scan_parameters("SELECT * FROM users WHERE a = '?' AND b = \"$1\"");
        assert!(literals.is_empty());

        let mixed = scan_parameters("SELECT * FROM users WHERE a = ?1 AND b = '?'");
        assert_eq!(mixed.positional, BTreeSet::from([1]));
    }

    #[test]
    fn plans_select_predicates_and_explain() {
        let plan = prepare_sql(
            "EXPLAIN ANALYZE SELECT name FROM users WHERE role = 'admin' AND score >= 90 OR active = true LIMIT 5",
            QueryLimits::default(),
        )
        .unwrap();
        assert!(plan.explain);
        assert!(plan.analyze);
        assert_eq!(plan.operation, SqlOperation::Select);
        assert_eq!(plan.collection, "users");
        assert_eq!(
            plan.ast,
            json!({
                "$collection": "users",
                "$select": ["name"],
                "$onlyIds": false,
                "$ops": [
                    {"role": {"$eq": "admin"}, "score": {"$gte": 90}},
                    {"active": {"$eq": true}}
                ],
                "$limit": 5
            })
        );
        assert_eq!(plan.access.len(), 3);
    }

    #[test]
    fn parses_mutation_asts_without_executing_them() {
        let insert = prepare_sql(
            "INSERT INTO users (name, active) VALUES ('O''Brien', true)",
            QueryLimits::default(),
        )
        .unwrap();
        assert_eq!(
            insert.ast,
            json!({
                "$collection": "users",
                "$values": {"name": "O'Brien", "active": true}
            })
        );

        let update = prepare_sql(
            "UPDATE users SET score = 92, active = false WHERE id = 7",
            QueryLimits::default(),
        )
        .unwrap();
        assert_eq!(
            update.ast,
            json!({
                "$collection": "users",
                "$set": {"score": 92, "active": false},
                "$where": {"$collection": "users", "$ops": [{"id": {"$eq": 7}}]}
            })
        );

        let delete = prepare_sql(
            "DELETE FROM users WHERE retired = true",
            QueryLimits::default(),
        )
        .unwrap();
        assert_eq!(
            delete.ast,
            json!({
                "$collection": "users",
                "$ops": [{"retired": {"$eq": true}}]
            })
        );
    }

    #[test]
    fn parses_join_and_nested_paths() {
        let plan = prepare_sql(
            "SELECT users.name FROM users LEFT JOIN teams ON team.id = id AND status != state GROUP BY team.id LIMIT 2",
            QueryLimits::default(),
        )
        .unwrap();
        assert_eq!(
            plan.ast,
            json!({
                "$leftCollection": "users",
                "$rightCollection": "teams",
                "$mode": "left",
                "$on": {
                    "team/id": {"$eq": "id"},
                    "status": {"$ne": "state"}
                },
                "$select": ["users/name"],
                "$groupby": "team/id",
                "$limit": 2
            })
        );
    }

    #[test]
    fn rejects_malformed_and_oversized_sql() {
        let malformed = prepare_sql("SELECT FROM users", QueryLimits::default()).unwrap_err();
        assert_eq!(malformed.code(), QueryErrorCode::InvalidQuery);

        let limits = QueryLimits {
            max_input_bytes: 4,
            ..QueryLimits::default()
        };
        let oversized = prepare_sql("SELECT * FROM users", limits).unwrap_err();
        assert_eq!(oversized.code(), QueryErrorCode::InputTooLarge);
    }

    #[test]
    fn parses_order_by_offset_and_count() {
        let ordered = prepare_sql(
            "SELECT name FROM users ORDER BY score DESC LIMIT 5 OFFSET 2",
            QueryLimits::default(),
        )
        .unwrap();
        assert_eq!(
            ordered.ast,
            json!({
                "$collection": "users",
                "$select": ["name"],
                "$onlyIds": false,
                "$sort": {"score": "desc"},
                "$limit": 5,
                "$offset": 2
            })
        );

        let count = prepare_sql(
            "SELECT COUNT(*) FROM users WHERE active = true",
            QueryLimits::default(),
        )
        .unwrap();
        assert_eq!(
            count.ast,
            json!({
                "$collection": "users",
                "$count": true,
                "$ops": [{"active": {"$eq": true}}]
            })
        );
    }

    #[test]
    fn parses_in_and_is_null_predicates() {
        let plan = prepare_sql(
            "SELECT * FROM users WHERE status IN ('a', 'b') AND note IS NOT NULL",
            QueryLimits::default(),
        )
        .unwrap();
        assert_eq!(
            plan.ast,
            json!({
                "$collection": "users",
                "$onlyIds": false,
                "$ops": [
                    {
                        "status": {"$in": ["a", "b"]},
                        "note": {"$isnotnull": true}
                    }
                ]
            })
        );

        let isnull = prepare_sql(
            "SELECT * FROM users WHERE note IS NULL",
            QueryLimits::default(),
        )
        .unwrap();
        assert_eq!(isnull.ast["$ops"], json!([{"note": {"$isnull": true}}]));
    }

    #[test]
    fn parses_parameter_markers() {
        let positional = prepare_sql(
            "SELECT * FROM users WHERE age > ?1 AND name = ?2",
            QueryLimits::default(),
        )
        .unwrap();
        assert_eq!(
            positional.ast["$ops"],
            json!([
                {
                    "age": {"$gt": {"$param": {"positional": 1}}},
                    "name": {"$eq": {"$param": {"positional": 2}}}
                }
            ])
        );

        let named = prepare_sql(
            "SELECT * FROM users WHERE age > :min AND name = $name",
            QueryLimits::default(),
        )
        .unwrap();
        assert_eq!(
            named.ast["$ops"],
            json!([
                {
                    "age": {"$gt": {"$param": {"named": "min"}}},
                    "name": {"$eq": {"$param": {"named": "name"}}}
                }
            ])
        );
    }

    #[test]
    fn rejects_out_of_subset_statements_with_unsupported_sql() {
        for (sql, _clause) in [
            ("SELECT * FROM users HAVING count > 1", "HAVING"),
            ("BEGIN", "BEGIN"),
            ("COMMIT", "COMMIT"),
            ("ALTER TABLE users ADD x", "ALTER"),
            ("CREATE INDEX i ON users", "CREATE INDEX"),
            ("SELECT DISTINCT name FROM users", "SELECT DISTINCT"),
            ("SELECT * FROM (SELECT * FROM users)", "sub-query"),
            ("SELECT SUM(score) FROM users", "function"),
        ] {
            let error = prepare_sql(sql, QueryLimits::default()).unwrap_err();
            assert_eq!(
                error.code(),
                QueryErrorCode::UnsupportedSql,
                "expected EUNSUPPORTEDSQL for: {sql}"
            );
        }
    }
}
