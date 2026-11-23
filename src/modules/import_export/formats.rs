//! import export - formats.

use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;
use super::entities::*;
use super::validation::*;
use super::export::*;

/// Parse CSV text into rows of fields. Handles quoted fields,
/// doubled-quote escapes, embedded commas/newlines, and CRLF endings.
/// Returns an error naming the first malformed line.
pub(crate) fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let mut line: usize = 1;
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                if in_quotes {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                } else if field.is_empty() {
                    in_quotes = true;
                } else {
                    return Err(format!("Line {line}: unexpected quote inside an unquoted field"));
                }
            }
            ',' if !in_quotes => {
                row.push(std::mem::take(&mut field));
            }
            '\r' => {
                if in_quotes {
                    field.push('\r');
                } else if chars.peek() == Some(&'\n') {
                    chars.next();
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    line += 1;
                } else {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    line += 1;
                }
            }
            '\n' if !in_quotes => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                line += 1;
            }
            other => field.push(other),
        }
    }
    if in_quotes {
        return Err(format!("Line {line}: unterminated quoted field"));
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    // Drop a trailing empty row produced by a final newline.
    while let Some(last) = rows.last() {
        if last.len() == 1 && last[0].is_empty() {
            rows.pop();
        } else {
            break;
        }
    }
    Ok(rows)
}

pub(crate) fn escape_csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

pub(crate) fn value_to_csv_cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

pub(crate) fn escape_xml_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

pub(crate) fn unescape_xml_text(text: &str) -> String {
    let mut out = text
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    // Numeric character references (`&#65;` and `&#x41;`).
    while let Some(start) = out.find("&#") {
        let rest = &out[start + 2..];
        let (digits, hex) = if let Some(stripped) = rest.strip_prefix('x').or_else(|| rest.strip_prefix('X')) {
            (stripped, true)
        } else {
            (rest, false)
        };
        let end = digits.find(';');
        let Some(end) = end else { break };
        let number = &digits[..end];
        let parsed = if hex {
            u32::from_str_radix(number, 16).ok()
        } else {
            number.parse::<u32>().ok()
        };
        let Some(code) = parsed.and_then(char::from_u32) else { break };
        let prefix_len = if hex { 3 } else { 2 };
        out.replace_range(start..start + prefix_len + end + 1, &code.to_string());
    }
    out
}

#[derive(Debug, Clone)]
pub(crate) struct XmlElement {
    pub(crate) name: String,
    pub(crate) text: String,
    pub(crate) children: Vec<XmlElement>,
}

pub(crate) struct XmlParser {
    chars: Vec<char>,
    pos: usize,
}

impl XmlParser {
    fn new(source: &str) -> Self {
        Self { chars: source.chars().collect(), pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn starts_with(&self, text: &str) -> bool {
        let wanted: Vec<char> = text.chars().collect();
        self.chars[self.pos..].starts_with(&wanted)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(ch) if ch.is_whitespace()) {
            self.pos += 1;
        }
    }

    /// Skip an XML declaration, a comment, or a processing
    /// instruction. Returns false when there is nothing to skip.
    fn skip_misc(&mut self) -> bool {
        if self.starts_with("<?") {
            while let Some(ch) = self.peek() {
                self.pos += 1;
                if ch == '?' && self.peek() == Some('>') {
                    self.pos += 1;
                    break;
                }
            }
            true
        } else if self.starts_with("<!--") {
            while self.pos + 2 < self.chars.len() && !self.starts_with("-->") {
                self.pos += 1;
            }
            self.pos = (self.pos + 3).min(self.chars.len());
            true
        } else {
            false
        }
    }

    fn read_name(&mut self) -> Result<String, String> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':') {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err("Expected an element name".to_string());
        }
        Ok(self.chars[start..self.pos].iter().collect())
    }

    /// Skip attributes up to `>` or `/>`, honouring quoted values.
    fn skip_attributes(&mut self) -> Result<bool, String> {
        loop {
            match self.peek() {
                None => return Err("Unterminated element".to_string()),
                Some('>') => {
                    self.pos += 1;
                    return Ok(false);
                }
                Some('/') if self.peek_at(1) == Some('>') => {
                    self.pos += 2;
                    return Ok(true);
                }
                Some(quote @ ('"' | '\'')) => {
                    self.pos += 1;
                    while let Some(ch) = self.peek() {
                        self.pos += 1;
                        if ch == quote {
                            break;
                        }
                    }
                }
                Some(_) => {
                    self.pos += 1;
                }
            }
        }
    }

    fn parse_element(&mut self, depth: usize) -> Result<XmlElement, String> {
        if depth > 4 {
            return Err("XML nesting is too deep: rows must be flat".to_string());
        }
        if self.peek() != Some('<') {
            return Err("Expected '<' at the start of an element".to_string());
        }
        self.pos += 1;
        let name = self.read_name()?;
        let self_closing = self.skip_attributes()?;
        if self_closing {
            return Ok(XmlElement { name, text: String::new(), children: Vec::new() });
        }
        let mut text = String::new();
        let mut children = Vec::new();
        loop {
            match self.peek() {
                None => return Err(format!("Unterminated element <{name}>")),
                Some('<') if self.starts_with("</") => {
                    self.pos += 2;
                    let closing = self.read_name()?;
                    if closing != name {
                        return Err(format!(
                            "Mismatched closing tag </{closing}> for <{name}>"
                        ));
                    }
                    self.skip_whitespace();
                    if self.peek() != Some('>') {
                        return Err(format!("Expected '>' after </{closing}>"));
                    }
                    self.pos += 1;
                    break;
                }
                Some('<') if self.starts_with("<!--") => {
                    self.skip_misc();
                }
                Some('<') => {
                    children.push(self.parse_element(depth + 1)?);
                }
                Some(_) => {
                    let start = self.pos;
                    while let Some(ch) = self.peek() {
                        if ch == '<' {
                            break;
                        }
                        self.pos += 1;
                    }
                    text.push_str(&self.chars[start..self.pos].iter().collect::<String>());
                }
            }
        }
        Ok(XmlElement { name, text: unescape_xml_text(text.trim()), children })
    }
}

pub(crate) fn parse_xml_document(text: &str) -> Result<XmlElement, String> {
    let lowered = text.to_ascii_lowercase();
    if lowered.contains("<!doctype") || lowered.contains("<!entity") {
        return Err("DOCTYPE and ENTITY declarations are not allowed".to_string());
    }
    let mut parser = XmlParser::new(text);
    loop {
        parser.skip_whitespace();
        if !parser.skip_misc() {
            break;
        }
    }
    let root = parser.parse_element(0)?;
    loop {
        parser.skip_whitespace();
        if !parser.skip_misc() {
            break;
        }
    }
    if parser.pos != parser.chars.len() {
        return Err("Unexpected content after the root element".to_string());
    }
    Ok(root)
}

pub(crate) fn value_to_xml_cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

pub(crate) fn valid_schedule(schedule: &str) -> bool {
    let schedule = schedule.trim();
    if schedule.is_empty() {
        return false;
    }
    if schedule.parse::<u64>().is_ok() {
        return true;
    }
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    fields.iter().all(|field| {
        if *field == "*" {
            return true;
        }
        if let Some(step) = field.strip_prefix("*/") {
            return step.parse::<u32>().is_ok_and(|n| n > 0);
        }
        field.parse::<u32>().is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_validator_accepts_intervals_and_cron() {
        assert!(valid_schedule("3600"));
        assert!(valid_schedule("0 2 * * *"));
        assert!(valid_schedule("*/15 * * * *"));
        assert!(!valid_schedule(""));
        assert!(!valid_schedule("daily"));
        assert!(!valid_schedule("0 2 * *"));
        assert!(!valid_schedule("*/0 * * * *"));
    }

    #[test]
    fn xml_escaping_round_trips() {
        let escaped = escape_xml_text("a & b <c> \"d\" 'e'");
        assert_eq!(escaped, "a &amp; b &lt;c&gt; &quot;d&quot; &apos;e&apos;");
        assert_eq!(unescape_xml_text(&escaped), "a & b <c> \"d\" 'e'");
        assert_eq!(unescape_xml_text("&#65;&#x42;"), "AB");
    }

    #[test]
    fn xml_document_parses_flat_rows() {
        let root = parse_xml_document(
            "<rows><row><email>jane@example.com</email><name>Jane</name></row>\
             <row><email>bob@example.com</email><name>Bob</name></row></rows>",
        )
        .expect("flat rows parse");
        assert_eq!(root.name, "rows");
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].children[0].text, "jane@example.com");
    }

    #[test]
    fn xml_document_rejects_doctype() {
        let error = parse_xml_document(
            "<!DOCTYPE foo [<!ENTITY xxe SYSTEM \"file:///etc/passwd\">]><rows/>",
        )
        .expect_err("doctype must be rejected");
        assert!(error.contains("DOCTYPE"));
    }

    #[test]
    fn xml_rows_map_to_raw_rows() {
        let spec = entity_spec("users").expect("users spec exists");
        let rows = content_to_raw_rows(
            spec,
            "xml",
            "<export><row><email>jane@example.com</email><name>Jane</name></row></export>",
            None,
        )
        .expect("xml import maps");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("email").map(String::as_str), Some("jane@example.com"));
    }

    #[test]
    fn company_mismatch_fails_that_row_only() {
        let spec = entity_spec("users").expect("users spec exists");
        let company = Uuid::now_v7();
        let other = Uuid::now_v7();
        let mut row = HashMap::new();
        row.insert("email".to_string(), "jane@example.com".to_string());
        row.insert("name".to_string(), "Jane".to_string());
        row.insert("company_id".to_string(), other.to_string());
        let (valid, errors) = validate_rows(spec, vec![row], Some(company));
        assert!(valid.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("company_id"));
    }

    #[test]
    fn parse_csv_handles_quoted_commas_and_escaped_quotes() {
        let rows = parse_csv("a,\"b,c\",\"d\"\"e\"").expect("csv parses");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], vec!["a".to_string(), "b,c".to_string(), "d\"e".to_string()]);
    }

    #[test]
    fn parse_csv_handles_crlf_line_endings() {
        let rows = parse_csv("a,b\r\n1,2\r\n").expect("csv parses");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1], vec!["1".to_string(), "2".to_string()]);
    }

    #[test]
    fn parse_csv_rejects_unterminated_quotes() {
        assert!(parse_csv("\"abc").is_err());
    }

    #[test]
    fn parse_csv_empty_input_yields_no_rows() {
        let rows = parse_csv("").expect("empty csv parses");
        assert!(rows.is_empty());
    }

    #[test]
    fn escape_csv_field_quotes_fields_with_commas_and_doubles_quotes() {
        assert_eq!(escape_csv_field("a,b"), "\"a,b\"");
        assert_eq!(escape_csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape_csv_field("plain"), "plain");
    }

    #[test]
    fn value_to_csv_cell_renders_scalars() {
        assert_eq!(value_to_csv_cell(&Value::Null), "");
        assert_eq!(value_to_csv_cell(&serde_json::json!(true)), "true");
        assert_eq!(value_to_csv_cell(&serde_json::json!(5)), "5");
        assert_eq!(value_to_csv_cell(&serde_json::json!("hello")), "hello");
    }
}
