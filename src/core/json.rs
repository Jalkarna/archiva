use std::error::Error;
use std::fmt::{self, Write};

pub const DEFAULT_MAX_DEPTH: usize = 512;
pub const DEFAULT_MAX_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(JsonObject),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct JsonObject {
    entries: Vec<(String, JsonValue)>,
}

impl JsonObject {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn from_entries(entries: Vec<(String, JsonValue)>) -> Self {
        let mut object = Self::new();
        for (key, value) in entries {
            object.insert(key, value);
        }
        object
    }

    pub fn insert(&mut self, key: String, value: JsonValue) {
        if let Some((_, existing)) = self
            .entries
            .iter_mut()
            .find(|(existing, _)| existing == &key)
        {
            *existing = value;
            return;
        }
        self.entries.push((key, value));
    }

    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        self.entries
            .iter()
            .find(|(existing, _)| existing == key)
            .map(|(_, value)| value)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut JsonValue> {
        self.entries
            .iter_mut()
            .find(|(existing, _)| existing == key)
            .map(|(_, value)| value)
    }

    pub fn entries(&self) -> &[(String, JsonValue)] {
        &self.entries
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseOptions {
    pub max_depth: usize,
    pub max_bytes: usize,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonError {
    message: String,
    offset: usize,
}

impl JsonError {
    fn new(message: impl Into<String>, offset: usize) -> Self {
        Self {
            message: message.into(),
            offset,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn offset(&self) -> usize {
        self.offset
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at byte {}", self.message, self.offset)
    }
}

impl Error for JsonError {}

pub fn parse(input: &str) -> Result<JsonValue, JsonError> {
    parse_with_options(input, ParseOptions::default())
}

pub fn parse_with_options(input: &str, options: ParseOptions) -> Result<JsonValue, JsonError> {
    if input.len() > options.max_bytes {
        return Err(JsonError::new(
            "JSON input exceeds configured byte limit",
            options.max_bytes,
        ));
    }

    let mut parser = Parser {
        input,
        bytes: input.as_bytes(),
        index: 0,
        options,
    };
    let value = parser.parse_value(0)?;
    parser.skip_whitespace();
    if parser.index != parser.bytes.len() {
        return Err(parser.error("Unexpected trailing characters"));
    }
    Ok(value)
}

pub fn stringify_compact(value: &JsonValue) -> String {
    let mut output = String::new();
    write_compact(value, &mut output);
    output
}

pub fn stringify_pretty(value: &JsonValue, indent_spaces: usize) -> String {
    let mut output = String::new();
    write_pretty(value, &mut output, 0, indent_spaces);
    output
}

struct Parser<'a> {
    input: &'a str,
    bytes: &'a [u8],
    index: usize,
    options: ParseOptions,
}

impl<'a> Parser<'a> {
    fn parse_value(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        self.skip_whitespace();
        match self.peek() {
            Some(b'n') => {
                self.consume_literal(b"null")?;
                Ok(JsonValue::Null)
            }
            Some(b't') => {
                self.consume_literal(b"true")?;
                Ok(JsonValue::Bool(true))
            }
            Some(b'f') => {
                self.consume_literal(b"false")?;
                Ok(JsonValue::Bool(false))
            }
            Some(b'"') => Ok(JsonValue::String(self.parse_string()?)),
            Some(b'[') => {
                self.check_depth(depth)?;
                self.parse_array(depth + 1)
            }
            Some(b'{') => {
                self.check_depth(depth)?;
                self.parse_object(depth + 1)
            }
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            Some(_) => Err(self.error("Expected JSON value")),
            None => Err(self.error("Unexpected end of input")),
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        self.expect_byte(b'[')?;
        self.skip_whitespace();
        let mut values = Vec::new();
        if self.consume_if(b']') {
            return Ok(JsonValue::Array(values));
        }

        loop {
            values.push(self.parse_value(depth)?);
            self.skip_whitespace();
            if self.consume_if(b']') {
                break;
            }
            self.expect_byte(b',')?;
        }

        Ok(JsonValue::Array(values))
    }

    fn parse_object(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        self.expect_byte(b'{')?;
        self.skip_whitespace();
        let mut object = JsonObject::new();
        if self.consume_if(b'}') {
            return Ok(JsonValue::Object(object));
        }

        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(self.error("Expected object key string"));
            }
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect_byte(b':')?;
            let value = self.parse_value(depth)?;
            object.insert(key, value);
            self.skip_whitespace();
            if self.consume_if(b'}') {
                break;
            }
            self.expect_byte(b',')?;
        }

        Ok(JsonValue::Object(object))
    }

    fn parse_string(&mut self) -> Result<String, JsonError> {
        self.expect_byte(b'"')?;
        let mut output = String::new();
        loop {
            let byte = self
                .peek()
                .ok_or_else(|| self.error("Unterminated string"))?;
            match byte {
                b'"' => {
                    self.index += 1;
                    return Ok(output);
                }
                b'\\' => {
                    self.index += 1;
                    self.parse_escape(&mut output)?;
                }
                0x00..=0x1f => return Err(self.error("Unescaped control character in string")),
                _ => {
                    let character = self.input[self.index..]
                        .chars()
                        .next()
                        .ok_or_else(|| self.error("Unterminated string"))?;
                    output.push(character);
                    self.index += character.len_utf8();
                }
            }
        }
    }

    fn parse_escape(&mut self, output: &mut String) -> Result<(), JsonError> {
        let escape = self
            .peek()
            .ok_or_else(|| self.error("Unterminated escape sequence"))?;
        self.index += 1;
        match escape {
            b'"' => output.push('"'),
            b'\\' => output.push('\\'),
            b'/' => output.push('/'),
            b'b' => output.push('\u{0008}'),
            b'f' => output.push('\u{000c}'),
            b'n' => output.push('\n'),
            b'r' => output.push('\r'),
            b't' => output.push('\t'),
            b'u' => self.parse_unicode_escape(output)?,
            _ => return Err(self.error_at("Invalid string escape", self.index.saturating_sub(1))),
        }
        Ok(())
    }

    fn parse_unicode_escape(&mut self, output: &mut String) -> Result<(), JsonError> {
        let first = self.parse_hex_unit()?;
        if is_high_surrogate(first) {
            let escape_offset = self.index;
            if self.peek() != Some(b'\\') || self.bytes.get(self.index + 1) != Some(&b'u') {
                return Err(self.error_at(
                    "High surrogate must be followed by a low surrogate",
                    escape_offset,
                ));
            }
            self.index += 2;
            let second = self.parse_hex_unit()?;
            if !is_low_surrogate(second) {
                return Err(self.error_at(
                    "High surrogate must be followed by a low surrogate",
                    escape_offset,
                ));
            }
            let codepoint = 0x10000 + (((first as u32 - 0xd800) << 10) | (second as u32 - 0xdc00));
            let character = char::from_u32(codepoint)
                .ok_or_else(|| self.error_at("Invalid unicode scalar", escape_offset))?;
            output.push(character);
            return Ok(());
        }
        if is_low_surrogate(first) {
            return Err(self.error("Low surrogate without preceding high surrogate"));
        }
        let character =
            char::from_u32(first as u32).ok_or_else(|| self.error("Invalid unicode scalar"))?;
        output.push(character);
        Ok(())
    }

    fn parse_hex_unit(&mut self) -> Result<u16, JsonError> {
        let start = self.index;
        if self.index + 4 > self.bytes.len() {
            return Err(self.error_at("Incomplete unicode escape", start));
        }
        let mut value = 0_u16;
        for _ in 0..4 {
            let byte = self.bytes[self.index];
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err(self.error_at("Invalid unicode escape", self.index)),
            };
            value = (value << 4) | digit as u16;
            self.index += 1;
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<JsonValue, JsonError> {
        let start = self.index;
        if self.consume_if(b'-') && self.peek().is_none() {
            return Err(self.error_at("Invalid number", start));
        }

        match self.peek() {
            Some(b'0') => {
                self.index += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.error("Leading zeroes are not valid JSON numbers"));
                }
            }
            Some(b'1'..=b'9') => self.consume_digits(),
            _ => return Err(self.error_at("Invalid number", start)),
        }

        if self.consume_if(b'.') {
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("Expected digit after decimal point"));
            }
            self.consume_digits();
        }

        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.index += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.index += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("Expected digit in exponent"));
            }
            self.consume_digits();
        }

        let raw = &self.input[start..self.index];
        let value = raw
            .parse::<f64>()
            .map_err(|_| self.error_at("Invalid number", start))?;
        Ok(JsonValue::Number(value))
    }

    fn consume_digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.index += 1;
        }
    }

    fn consume_literal(&mut self, literal: &[u8]) -> Result<(), JsonError> {
        if self.bytes.get(self.index..self.index + literal.len()) == Some(literal) {
            self.index += literal.len();
            Ok(())
        } else {
            Err(self.error("Expected JSON literal"))
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.index += 1;
        }
    }

    fn check_depth(&self, depth: usize) -> Result<(), JsonError> {
        if depth >= self.options.max_depth {
            return Err(self.error("JSON nesting exceeds configured depth limit"));
        }
        Ok(())
    }

    fn expect_byte(&mut self, expected: u8) -> Result<(), JsonError> {
        if self.consume_if(expected) {
            Ok(())
        } else {
            Err(self.error(format!("Expected '{}'", expected as char)))
        }
    }

    fn consume_if(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    fn error(&self, message: impl Into<String>) -> JsonError {
        JsonError::new(message, self.index)
    }

    fn error_at(&self, message: impl Into<String>, offset: usize) -> JsonError {
        JsonError::new(message, offset)
    }
}

fn write_compact(value: &JsonValue, output: &mut String) {
    match value {
        JsonValue::Null => output.push_str("null"),
        JsonValue::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        JsonValue::Number(value) => output.push_str(&format_number(*value)),
        JsonValue::String(value) => write_json_string(value, output),
        JsonValue::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_compact(value, output);
            }
            output.push(']');
        }
        JsonValue::Object(object) => {
            output.push('{');
            for (index, (key, value)) in object.entries().iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_json_string(key, output);
                output.push(':');
                write_compact(value, output);
            }
            output.push('}');
        }
    }
}

fn write_pretty(value: &JsonValue, output: &mut String, depth: usize, indent_spaces: usize) {
    match value {
        JsonValue::Array(values) if values.is_empty() => output.push_str("[]"),
        JsonValue::Array(values) => {
            output.push('[');
            output.push('\n');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push_str(",\n");
                }
                write_indent(output, depth + 1, indent_spaces);
                write_pretty(value, output, depth + 1, indent_spaces);
            }
            output.push('\n');
            write_indent(output, depth, indent_spaces);
            output.push(']');
        }
        JsonValue::Object(object) if object.entries().is_empty() => output.push_str("{}"),
        JsonValue::Object(object) => {
            output.push('{');
            output.push('\n');
            for (index, (key, value)) in object.entries().iter().enumerate() {
                if index > 0 {
                    output.push_str(",\n");
                }
                write_indent(output, depth + 1, indent_spaces);
                write_json_string(key, output);
                output.push_str(": ");
                write_pretty(value, output, depth + 1, indent_spaces);
            }
            output.push('\n');
            write_indent(output, depth, indent_spaces);
            output.push('}');
        }
        _ => write_compact(value, output),
    }
}

fn write_indent(output: &mut String, depth: usize, indent_spaces: usize) {
    for _ in 0..depth * indent_spaces {
        output.push(' ');
    }
}

fn write_json_string(value: &str, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{0008}' => output.push_str("\\b"),
            '\u{000c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{0000}'..='\u{001f}' => {
                let _ = write!(output, "\\u{:04x}", character as u32);
            }
            _ => output.push(character),
        }
    }
    output.push('"');
}

fn format_number(value: f64) -> String {
    if !value.is_finite() {
        return "null".to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }

    let basic = value.to_string();
    let absolute = value.abs();
    if basic.contains('e') || basic.contains('E') {
        return normalize_exponent_notation(&basic);
    }
    if !(1e-6..1e21).contains(&absolute) {
        return decimal_to_exponent(&basic);
    }
    basic
}

fn normalize_exponent_notation(value: &str) -> String {
    let Some((mantissa, exponent)) = value.split_once(['e', 'E']) else {
        return value.to_string();
    };
    let exponent = exponent.parse::<i32>().unwrap_or(0);
    format!(
        "{}e{}{}",
        trim_mantissa(mantissa),
        if exponent >= 0 { "+" } else { "-" },
        exponent.abs()
    )
}

fn decimal_to_exponent(value: &str) -> String {
    let (sign, unsigned) = value
        .strip_prefix('-')
        .map_or(("", value), |rest| ("-", rest));
    if let Some((integer, fraction)) = unsigned.split_once('.') {
        if integer != "0" {
            let digits = format!("{integer}{fraction}");
            return compose_exponent(sign, &digits, integer.len() as i32 - 1);
        }
        let leading_zeroes = fraction.bytes().take_while(|byte| *byte == b'0').count();
        let digits = &fraction[leading_zeroes..];
        return compose_exponent(sign, digits, -((leading_zeroes as i32) + 1));
    }
    compose_exponent(sign, unsigned, unsigned.len() as i32 - 1)
}

fn compose_exponent(sign: &str, digits: &str, exponent: i32) -> String {
    let mut significant = digits.trim_end_matches('0').to_string();
    if significant.is_empty() {
        significant.push('0');
    }
    if significant == "0" {
        return "0".to_string();
    }

    let mut mantissa = String::new();
    mantissa.push_str(sign);
    mantissa.push(significant.as_bytes()[0] as char);
    if significant.len() > 1 {
        mantissa.push('.');
        mantissa.push_str(&significant[1..]);
    }

    format!(
        "{}e{}{}",
        mantissa,
        if exponent >= 0 { "+" } else { "-" },
        exponent.abs()
    )
}

fn trim_mantissa(value: &str) -> String {
    if !value.contains('.') {
        return value.to_string();
    }
    value
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn is_high_surrogate(value: u16) -> bool {
    (0xd800..=0xdbff).contains(&value)
}

fn is_low_surrogate(value: u16) -> bool {
    (0xdc00..=0xdfff).contains(&value)
}

#[cfg(test)]
mod tests {
    use super::{
        parse, parse_with_options, stringify_compact, stringify_pretty, JsonObject, JsonValue,
        ParseOptions,
    };

    #[test]
    fn matches_javascript_json_parse_and_stringify_contract() {
        let value = parse(
            "{\"z\":0,\"dup\":\"first\",\"a\":{\"escape\":\"line\\n\\uD83D\\uDE00\",\"quote\":\"\\\"\"},\"dup\":\"last\",\"arr\":[true,false,null,-0,1.25,1e2]}",
        )
        .expect("contract JSON parses");

        let JsonValue::Object(object) = &value else {
            panic!("expected object");
        };
        assert_eq!(
            object
                .entries()
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            vec!["z", "dup", "a", "arr"]
        );
        assert_eq!(
            stringify_compact(&value),
            "{\"z\":0,\"dup\":\"last\",\"a\":{\"escape\":\"line\\n\u{1F600}\",\"quote\":\"\\\"\"},\"arr\":[true,false,null,0,1.25,100]}"
        );
        assert_eq!(stringify_compact(&parse("1e21").unwrap()), "1e+21");
        assert_eq!(stringify_compact(&parse("1e-7").unwrap()), "1e-7");
        assert_eq!(
            stringify_compact(&JsonValue::String("\u{0001}".to_string())),
            "\"\\u0001\""
        );
    }

    #[test]
    fn writes_settings_style_pretty_json() {
        let value = JsonValue::Object(JsonObject::from_entries(vec![
            ("b".to_string(), JsonValue::Number(1.0)),
            (
                "a".to_string(),
                JsonValue::Object(JsonObject::from_entries(vec![(
                    "c".to_string(),
                    JsonValue::Number(2.0),
                )])),
            ),
        ]));

        assert_eq!(
            format!("{}\n", stringify_pretty(&value, 2)),
            "{\n  \"b\": 1,\n  \"a\": {\n    \"c\": 2\n  }\n}\n"
        );
    }

    #[test]
    fn rejects_malformed_json_and_enforces_limits() {
        assert!(parse("{bad")
            .unwrap_err()
            .message()
            .contains("Expected object key string"));
        assert!(parse("\"\\uD800\"")
            .unwrap_err()
            .message()
            .contains("High surrogate"));
        assert!(parse("\"\\uDE00\"")
            .unwrap_err()
            .message()
            .contains("Low surrogate"));
        assert!(parse("01")
            .unwrap_err()
            .message()
            .contains("Leading zeroes"));
        assert!(parse("[1,]").is_err());
        assert!(parse_with_options(
            "[[0]]",
            ParseOptions {
                max_depth: 1,
                max_bytes: 1024,
            },
        )
        .unwrap_err()
        .message()
        .contains("depth"));
        assert!(parse_with_options(
            "null",
            ParseOptions {
                max_depth: 8,
                max_bytes: 3,
            },
        )
        .unwrap_err()
        .message()
        .contains("byte limit"));
    }
}
