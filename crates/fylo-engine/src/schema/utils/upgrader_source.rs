pub(in crate::schema) struct UpgraderSource;

impl UpgraderSource {
    const DEFAULT_EXPORT: &str = "export default ";
    const ADDED_CAPACITY: usize = 64;

    pub(in crate::schema) fn normalize(source: &str) -> String {
        Self::normalize_export(source, source.find(Self::DEFAULT_EXPORT))
    }

    fn normalize_export(source: &str, offset: Option<usize>) -> String {
        let Some(offset) = offset else {
            return source.to_owned();
        };
        Self::normalize_expression(source, offset)
    }

    fn normalize_expression(source: &str, offset: usize) -> String {
        let expression = &source[offset + Self::DEFAULT_EXPORT.len()..];
        let declaration = Self::is_declaration(expression);
        Self::replace_expression(source, offset, declaration)
    }

    fn is_declaration(expression: &str) -> bool {
        expression.starts_with("function") || expression.starts_with("class")
    }

    fn replace_expression(source: &str, offset: usize, declaration: bool) -> String {
        if declaration {
            return source.to_owned();
        }
        Self::normalized(source, offset)
    }

    fn normalized(source: &str, offset: usize) -> String {
        let mut normalized = String::with_capacity(source.len() + Self::ADDED_CAPACITY);
        normalized.push_str(&source[..offset]);
        normalized.push_str("const __fylo_default_upgrader__ = ");
        normalized.push_str(&source[offset + Self::DEFAULT_EXPORT.len()..]);
        normalized.push_str("\nexport { __fylo_default_upgrader__ as default };\n");
        normalized
    }
}
