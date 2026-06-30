use std::error::Error;
use std::fmt;

pub const DEFAULT_MAX_DEPTH: usize = 512;

#[derive(Clone, Debug, PartialEq)]
pub enum YamlValue {
    Null,
    Bool(bool),
    Number(i64),
    String(String),
    Array(Vec<YamlValue>),
    Object(YamlObject),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct YamlObject {
    entries: Vec<(String, YamlValue)>,
}

impl YamlObject {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn from_entries(entries: Vec<(String, YamlValue)>) -> Self {
        let mut object = Self::new();
        for (key, value) in entries {
            object.insert(key, value);
        }
        object
    }

    pub fn insert(&mut self, key: String, value: YamlValue) {
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

    pub fn get(&self, key: &str) -> Option<&YamlValue> {
        self.entries
            .iter()
            .find(|(existing, _)| existing == key)
            .map(|(_, value)| value)
    }

    pub fn entries(&self) -> &[(String, YamlValue)] {
        &self.entries
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct YamlError {
    message: String,
    line: usize,
}

impl YamlError {
    fn new(message: impl Into<String>, line: usize) -> Self {
        Self {
            message: message.into(),
            line,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn line(&self) -> usize {
        self.line
    }
}

impl fmt::Display for YamlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at line {}", self.message, self.line)
    }
}

impl Error for YamlError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseOptions {
    pub max_depth: usize,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

pub fn parse_yaml(input: &str) -> Result<YamlValue, YamlError> {
    parse_yaml_with_options(input, ParseOptions::default())
}

pub fn parse_yaml_with_options(input: &str, options: ParseOptions) -> Result<YamlValue, YamlError> {
    let mut parser = Parser {
        lines: split_lines(input),
        index: 0,
        options,
    };
    parser.skip_ignored();
    if parser.index >= parser.lines.len() {
        return Ok(YamlValue::Null);
    }
    let indent = parser.current_indent().unwrap_or(0);
    let value = parser.parse_block(indent, 0)?;
    parser.skip_ignored();
    if parser.index < parser.lines.len() {
        return Err(parser.error("Unexpected trailing YAML content"));
    }
    Ok(value)
}

pub fn render_yaml(value: &YamlValue) -> String {
    let mut output = String::new();
    render_value(value, 0, &mut output);
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output
}

#[derive(Clone, Debug)]
struct SourceLine {
    number: usize,
    text: String,
}

struct Parser {
    lines: Vec<SourceLine>,
    index: usize,
    options: ParseOptions,
}

impl Parser {
    fn parse_block(&mut self, indent: usize, depth: usize) -> Result<YamlValue, YamlError> {
        self.skip_ignored();
        let Some(current_indent) = self.current_indent() else {
            return Ok(YamlValue::Null);
        };
        self.check_depth(depth)?;
        if current_indent < indent {
            return Ok(YamlValue::Null);
        }
        if current_indent > indent {
            return Err(self.error("Unexpected indentation"));
        }
        if self.current_content().starts_with("- ") {
            self.parse_sequence(indent, depth)
        } else {
            self.parse_mapping(indent, depth)
        }
    }

    fn parse_mapping(&mut self, indent: usize, depth: usize) -> Result<YamlValue, YamlError> {
        let mut object = YamlObject::new();
        loop {
            self.skip_ignored();
            let Some(current_indent) = self.current_indent() else {
                break;
            };
            if current_indent < indent {
                break;
            }
            if current_indent > indent {
                return Err(self.error("Unexpected indentation in mapping"));
            }
            if self.current_content().starts_with("- ") {
                break;
            }

            let line_number = self.current_line_number();
            let content = self.current_content().to_string();
            let (key, value_part) = split_mapping_entry(&content, line_number)?
                .ok_or_else(|| YamlError::new("Expected mapping entry", line_number))?;
            self.index += 1;
            let value = self.parse_mapping_value(value_part, indent, depth + 1)?;
            object.insert(key, value);
        }
        Ok(YamlValue::Object(object))
    }

    fn parse_sequence(&mut self, indent: usize, depth: usize) -> Result<YamlValue, YamlError> {
        let mut values = Vec::new();
        loop {
            self.skip_ignored();
            let Some(current_indent) = self.current_indent() else {
                break;
            };
            if current_indent < indent {
                break;
            }
            if current_indent > indent {
                return Err(self.error("Unexpected indentation in sequence"));
            }
            let content = self.current_content().to_string();
            let Some(rest) = content.strip_prefix("- ") else {
                break;
            };
            self.index += 1;
            values.push(self.parse_sequence_item(rest, indent, depth + 1)?);
        }
        Ok(YamlValue::Array(values))
    }

    fn parse_sequence_item(
        &mut self,
        rest: &str,
        indent: usize,
        depth: usize,
    ) -> Result<YamlValue, YamlError> {
        if rest.is_empty() {
            let child_indent = self.next_content_indent().unwrap_or(indent + 2);
            return self.parse_block(child_indent, depth);
        }

        let line_number = self.previous_line_number();
        if let Some((key, value_part)) = split_mapping_entry(rest, line_number)? {
            let mut object = YamlObject::new();
            self.check_depth(depth)?;
            let value = self.parse_mapping_value(value_part, indent, depth + 1)?;
            object.insert(key, value);
            self.parse_additional_item_mapping_entries(indent + 2, depth + 1, &mut object)?;
            return Ok(YamlValue::Object(object));
        }

        parse_scalar_with_options(rest, self.previous_line_number(), self.options, depth)
    }

    fn parse_additional_item_mapping_entries(
        &mut self,
        indent: usize,
        depth: usize,
        object: &mut YamlObject,
    ) -> Result<(), YamlError> {
        loop {
            self.skip_ignored();
            let Some(current_indent) = self.current_indent() else {
                break;
            };
            if current_indent < indent {
                break;
            }
            if current_indent > indent {
                return Err(self.error("Unexpected indentation in sequence item"));
            }
            if self.current_content().starts_with("- ") {
                break;
            }
            let line_number = self.current_line_number();
            let content = self.current_content().to_string();
            let (key, value_part) = split_mapping_entry(&content, line_number)?
                .ok_or_else(|| YamlError::new("Expected mapping entry", line_number))?;
            self.index += 1;
            let value = self.parse_mapping_value(value_part, indent, depth)?;
            object.insert(key, value);
        }
        Ok(())
    }

    fn parse_mapping_value(
        &mut self,
        value_part: &str,
        indent: usize,
        depth: usize,
    ) -> Result<YamlValue, YamlError> {
        let trimmed = value_part.trim_start();
        if trimmed.is_empty() {
            let child_indent = self.next_content_indent().unwrap_or(indent + 2);
            return self.parse_block(child_indent, depth);
        }
        if matches!(trimmed, ">-" | ">" | "|-" | "|") {
            return self.parse_block_scalar(trimmed, indent);
        }
        parse_scalar_with_options(trimmed, self.previous_line_number(), self.options, depth)
    }

    fn parse_block_scalar(
        &mut self,
        marker: &str,
        parent_indent: usize,
    ) -> Result<YamlValue, YamlError> {
        let mut collected: Vec<String> = Vec::new();
        let mut block_indent: Option<usize> = None;
        while self.index < self.lines.len() {
            let raw = &self.lines[self.index];
            if raw.text.trim().is_empty() {
                collected.push(String::new());
                self.index += 1;
                continue;
            }
            let indent = count_indent(&raw.text)?;
            if indent <= parent_indent {
                break;
            }
            let content_indent = *block_indent.get_or_insert(indent);
            let content = if raw.text.len() >= content_indent {
                raw.text[content_indent..].to_string()
            } else {
                String::new()
            };
            collected.push(content);
            self.index += 1;
        }

        let value = if marker.starts_with('>') {
            fold_block_scalar(&collected, marker.ends_with('-'))
        } else {
            literal_block_scalar(&collected, marker.ends_with('-'))
        };
        Ok(YamlValue::String(value))
    }

    fn skip_ignored(&mut self) {
        while self.index < self.lines.len() {
            let trimmed = self.lines[self.index].text.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                self.index += 1;
            } else {
                break;
            }
        }
    }

    fn current_indent(&self) -> Option<usize> {
        self.lines
            .get(self.index)
            .and_then(|line| count_indent(&line.text).ok())
    }

    fn current_content(&self) -> &str {
        let line = &self.lines[self.index].text;
        line.trim_start()
    }

    fn current_line_number(&self) -> usize {
        self.lines[self.index].number
    }

    fn previous_line_number(&self) -> usize {
        self.lines
            .get(self.index.saturating_sub(1))
            .map(|line| line.number)
            .unwrap_or(1)
    }

    fn next_content_indent(&mut self) -> Option<usize> {
        self.skip_ignored();
        self.current_indent()
    }

    fn error(&self, message: impl Into<String>) -> YamlError {
        let line = self
            .lines
            .get(self.index)
            .map(|line| line.number)
            .unwrap_or_else(|| self.lines.last().map(|line| line.number).unwrap_or(1));
        YamlError::new(message, line)
    }

    fn check_depth(&self, depth: usize) -> Result<(), YamlError> {
        if depth > self.options.max_depth {
            return Err(self.error("YAML nesting exceeds configured depth limit"));
        }
        Ok(())
    }
}

fn parse_scalar_with_options(
    input: &str,
    line: usize,
    options: ParseOptions,
    depth: usize,
) -> Result<YamlValue, YamlError> {
    let value = strip_plain_comment(input).trim_end();
    parse_scalar_value(value, line, options, depth)
}

fn parse_scalar_value(
    value: &str,
    line: usize,
    options: ParseOptions,
    depth: usize,
) -> Result<YamlValue, YamlError> {
    if value == "[]" {
        return Ok(YamlValue::Array(Vec::new()));
    }
    if value.starts_with('[') && value.ends_with(']') {
        return parse_flow_value_with_options(value, line, options, depth);
    }
    if value == "{}" {
        return Ok(YamlValue::Object(YamlObject::new()));
    }
    if value.starts_with('{') && value.ends_with('}') {
        return parse_flow_value_with_options(value, line, options, depth);
    }
    if value.starts_with('[') || value.starts_with('{') {
        return Err(YamlError::new("Unterminated flow collection", line));
    }
    if value == "null" || value == "~" {
        return Ok(YamlValue::Null);
    }
    if value == "true" {
        return Ok(YamlValue::Bool(true));
    }
    if value == "false" {
        return Ok(YamlValue::Bool(false));
    }
    if let Some(unquoted) = parse_single_quoted(value) {
        return Ok(YamlValue::String(unquoted));
    }
    if value.starts_with('"') {
        return parse_double_quoted(value, line).map(YamlValue::String);
    }
    if let Ok(number) = value.parse::<i64>() {
        return Ok(YamlValue::Number(number));
    }
    Ok(YamlValue::String(value.to_string()))
}

#[allow(dead_code)]
fn parse_flow_value(input: &str, line: usize) -> Result<YamlValue, YamlError> {
    parse_flow_value_with_options(input, line, ParseOptions::default(), 0)
}

fn parse_flow_value_with_options(
    input: &str,
    line: usize,
    options: ParseOptions,
    depth: usize,
) -> Result<YamlValue, YamlError> {
    let mut parser = FlowParser {
        input,
        index: 0,
        line,
        options,
    };
    let value = parser.parse_value(depth)?;
    parser.skip_ws();
    if parser.index != parser.input.len() {
        return Err(parser.error("Unexpected trailing flow content"));
    }
    Ok(value)
}

struct FlowParser<'a> {
    input: &'a str,
    index: usize,
    line: usize,
    options: ParseOptions,
}

impl<'a> FlowParser<'a> {
    fn parse_value(&mut self, depth: usize) -> Result<YamlValue, YamlError> {
        self.skip_ws();
        match self.peek_char() {
            Some('[') => self.parse_sequence(depth),
            Some('{') => self.parse_mapping(depth),
            Some('\'') => self.parse_single_quoted_value().map(YamlValue::String),
            Some('"') => self.parse_double_quoted_value().map(YamlValue::String),
            Some(',') | Some(']') | Some('}') | None => Err(self.error("Expected flow value")),
            _ => self.parse_plain_value(),
        }
    }

    fn parse_sequence(&mut self, depth: usize) -> Result<YamlValue, YamlError> {
        self.check_depth(depth)?;
        self.expect_char('[')?;
        let mut values = Vec::new();
        loop {
            self.skip_ws();
            if self.consume_char(']') {
                break;
            }
            values.push(self.parse_value(depth + 1)?);
            self.skip_ws();
            if self.consume_char(',') {
                continue;
            }
            if self.consume_char(']') {
                break;
            }
            return Err(self.error("Expected ',' or ']' in flow sequence"));
        }
        Ok(YamlValue::Array(values))
    }

    fn parse_mapping(&mut self, depth: usize) -> Result<YamlValue, YamlError> {
        self.check_depth(depth)?;
        self.expect_char('{')?;
        let mut object = YamlObject::new();
        loop {
            self.skip_ws();
            if self.consume_char('}') {
                break;
            }
            let key = self.parse_key()?;
            self.skip_ws();
            let value = if matches!(self.peek_char(), Some(',') | Some('}')) {
                YamlValue::Null
            } else {
                self.parse_value(depth + 1)?
            };
            object.insert(key, value);
            self.skip_ws();
            if self.consume_char(',') {
                continue;
            }
            if self.consume_char('}') {
                break;
            }
            return Err(self.error("Expected ',' or '}' in flow mapping"));
        }
        Ok(YamlValue::Object(object))
    }

    fn parse_key(&mut self) -> Result<String, YamlError> {
        self.skip_ws();
        match self.peek_char() {
            Some('\'') => {
                let key = self.parse_single_quoted_value()?;
                self.skip_ws();
                self.expect_char(':')?;
                Ok(key)
            }
            Some('"') => {
                let key = self.parse_double_quoted_value()?;
                self.skip_ws();
                self.expect_char(':')?;
                Ok(key)
            }
            Some(_) => self.parse_plain_key(),
            None => Err(self.error("Expected flow mapping key")),
        }
    }

    fn parse_plain_key(&mut self) -> Result<String, YamlError> {
        let start = self.index;
        let mut first_colon = None::<usize>;
        while let Some(character) = self.peek_char() {
            match character {
                ':' => {
                    let colon = self.index;
                    if first_colon.is_none() {
                        first_colon = Some(colon);
                    }
                    self.bump_char();
                    if self.peek_char().is_none_or(|next| {
                        next.is_whitespace() || matches!(next, ',' | '}' | ']' | '[' | '{')
                    }) {
                        return self.finish_plain_key(start, colon);
                    }
                }
                ',' | '}' | ']' => {
                    if let Some(colon) = first_colon {
                        self.index = colon + 1;
                        return self.finish_plain_key(start, colon);
                    }
                    return Err(self.error("Expected ':' in flow mapping"));
                }
                _ => {
                    self.bump_char();
                }
            }
        }
        if let Some(colon) = first_colon {
            self.index = colon + 1;
            return self.finish_plain_key(start, colon);
        }
        Err(self.error("Expected ':' in flow mapping"))
    }

    fn finish_plain_key(&mut self, start: usize, colon: usize) -> Result<String, YamlError> {
        let key = self.input[start..colon].trim();
        if key.is_empty() {
            return Err(self.error("Expected flow mapping key"));
        }
        Ok(key.to_string())
    }

    fn parse_plain_value(&mut self) -> Result<YamlValue, YamlError> {
        let start = self.index;
        while let Some(character) = self.peek_char() {
            if matches!(character, ',' | ']' | '}') {
                break;
            }
            self.bump_char();
        }
        let raw = self.input[start..self.index].trim();
        if raw.is_empty() {
            return Err(self.error("Expected flow value"));
        }
        parse_scalar_value(raw, self.line, self.options, 0)
    }

    fn parse_single_quoted_value(&mut self) -> Result<String, YamlError> {
        let start = self.index;
        self.expect_char('\'')?;
        while let Some(character) = self.peek_char() {
            if character == '\'' {
                self.bump_char();
                if self.peek_char() == Some('\'') {
                    self.bump_char();
                    continue;
                }
                return parse_single_quoted(&self.input[start..self.index])
                    .ok_or_else(|| self.error("Malformed single quoted scalar"));
            }
            self.bump_char();
        }
        Err(self.error("Unterminated single quoted scalar"))
    }

    fn parse_double_quoted_value(&mut self) -> Result<String, YamlError> {
        let start = self.index;
        self.expect_char('"')?;
        let mut escaped = false;
        while let Some(character) = self.peek_char() {
            if escaped {
                escaped = false;
                self.bump_char();
                continue;
            }
            if character == '\\' {
                escaped = true;
                self.bump_char();
                continue;
            }
            if character == '"' {
                self.bump_char();
                return parse_double_quoted(&self.input[start..self.index], self.line);
            }
            self.bump_char();
        }
        Err(self.error("Unterminated double quoted scalar"))
    }

    fn skip_ws(&mut self) {
        while self.peek_char().is_some_and(char::is_whitespace) {
            self.bump_char();
        }
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.bump_char();
            true
        } else {
            false
        }
    }

    fn expect_char(&mut self, expected: char) -> Result<(), YamlError> {
        if self.consume_char(expected) {
            Ok(())
        } else {
            Err(self.error(format!("Expected '{expected}'")))
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.input[self.index..].chars().next()
    }

    fn bump_char(&mut self) -> Option<char> {
        let character = self.peek_char()?;
        self.index += character.len_utf8();
        Some(character)
    }

    fn error(&self, message: impl Into<String>) -> YamlError {
        YamlError::new(message, self.line)
    }

    fn check_depth(&self, depth: usize) -> Result<(), YamlError> {
        if depth > self.options.max_depth {
            return Err(self.error("YAML nesting exceeds configured depth limit"));
        }
        Ok(())
    }
}

fn parse_single_quoted(input: &str) -> Option<String> {
    if !input.starts_with('\'') || !input.ends_with('\'') {
        return None;
    }
    Some(input[1..input.len() - 1].replace("''", "'"))
}

fn parse_double_quoted(input: &str, line: usize) -> Result<String, YamlError> {
    if !input.ends_with('"') || input.len() < 2 {
        return Err(YamlError::new("Unterminated double quoted scalar", line));
    }
    let mut output = String::new();
    let inner = &input[1..input.len() - 1];
    let mut index = 0;
    while let Some(character) = next_char(inner, &mut index) {
        if character != '\\' {
            output.push(character);
            continue;
        }
        let Some(escape) = next_char(inner, &mut index) else {
            return Err(YamlError::new("Unterminated escape sequence", line));
        };
        match escape {
            ' ' => output.push(' '),
            '"' => output.push('"'),
            '\\' => output.push('\\'),
            '/' => output.push('/'),
            '0' => output.push('\0'),
            'a' => output.push('\u{0007}'),
            'b' => output.push('\u{0008}'),
            't' | '\t' => output.push('\t'),
            'n' => output.push('\n'),
            'v' => output.push('\u{000b}'),
            'f' => output.push('\u{000c}'),
            'r' => output.push('\r'),
            'e' => output.push('\u{001b}'),
            'N' => output.push('\u{0085}'),
            '_' => output.push('\u{00a0}'),
            'L' => output.push('\u{2028}'),
            'P' => output.push('\u{2029}'),
            'x' => push_codepoint(
                &mut output,
                read_hex_escape(inner, &mut index, 2, line)?,
                line,
            )?,
            'u' => {
                let code = read_hex_escape(inner, &mut index, 4, line)?;
                push_yaml_unicode_escape(&mut output, code, inner, &mut index, line)?;
            }
            'U' => push_codepoint(
                &mut output,
                read_hex_escape(inner, &mut index, 8, line)?,
                line,
            )?,
            _ => return Err(YamlError::new("Unsupported double quoted escape", line)),
        }
    }
    Ok(output)
}

fn next_char(input: &str, index: &mut usize) -> Option<char> {
    let character = input.get(*index..)?.chars().next()?;
    *index += character.len_utf8();
    Some(character)
}

fn read_hex_escape(
    input: &str,
    index: &mut usize,
    digits: usize,
    line: usize,
) -> Result<u32, YamlError> {
    let mut value = 0_u32;
    for _ in 0..digits {
        let Some(character) = next_char(input, index) else {
            return Err(YamlError::new("Incomplete unicode escape", line));
        };
        let Some(digit) = character.to_digit(16) else {
            return Err(YamlError::new("Invalid unicode escape", line));
        };
        value = value * 16 + digit;
    }
    Ok(value)
}

fn push_yaml_unicode_escape(
    output: &mut String,
    code: u32,
    input: &str,
    index: &mut usize,
    line: usize,
) -> Result<(), YamlError> {
    if (0xd800..=0xdbff).contains(&code) {
        let Some(rest) = input.get(*index..) else {
            return Err(YamlError::new("Invalid unicode surrogate pair", line));
        };
        if !rest.starts_with("\\u") {
            return Err(YamlError::new("Invalid unicode surrogate pair", line));
        }
        *index += 2;
        let low = read_hex_escape(input, index, 4, line)?;
        if !(0xdc00..=0xdfff).contains(&low) {
            return Err(YamlError::new("Invalid unicode surrogate pair", line));
        }
        let combined = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
        return push_codepoint(output, combined, line);
    }
    if (0xdc00..=0xdfff).contains(&code) {
        return Err(YamlError::new("Invalid unicode surrogate pair", line));
    }
    push_codepoint(output, code, line)
}

fn push_codepoint(output: &mut String, code: u32, line: usize) -> Result<(), YamlError> {
    let Some(character) = char::from_u32(code) else {
        return Err(YamlError::new("Invalid unicode scalar", line));
    };
    output.push(character);
    Ok(())
}

fn render_value(value: &YamlValue, indent: usize, output: &mut String) {
    match value {
        YamlValue::Object(object) => render_object(object, indent, output),
        YamlValue::Array(values) => render_array(values, indent, output),
        _ => {
            output.push_str(&render_scalar(value, indent));
            output.push('\n');
        }
    }
}

fn render_object(object: &YamlObject, indent: usize, output: &mut String) {
    for (key, value) in object.entries() {
        write_indent(indent, output);
        output.push_str(&render_key(key));
        match value {
            YamlValue::Object(object) if object.entries().is_empty() => {
                output.push_str(": {}\n");
            }
            YamlValue::Object(_) => {
                output.push_str(":\n");
                render_value(value, indent + 2, output);
            }
            YamlValue::Array(values) if values.is_empty() => {
                output.push_str(": []\n");
            }
            YamlValue::Array(values) => {
                output.push_str(":\n");
                render_array(values, indent + 2, output);
            }
            _ => {
                output.push_str(": ");
                output.push_str(&render_scalar(value, indent + 2));
                output.push('\n');
            }
        }
    }
}

fn render_array(values: &[YamlValue], indent: usize, output: &mut String) {
    for value in values {
        write_indent(indent, output);
        match value {
            YamlValue::Object(object) if !object.entries().is_empty() => {
                let (first_key, first_value) = &object.entries()[0];
                output.push_str("- ");
                output.push_str(&render_key(first_key));
                match first_value {
                    YamlValue::Object(_) | YamlValue::Array(_) => {
                        output.push_str(":\n");
                        render_value(first_value, indent + 4, output);
                    }
                    _ => {
                        output.push_str(": ");
                        output.push_str(&render_scalar(first_value, indent + 4));
                        output.push('\n');
                    }
                }
                for (key, value) in object.entries().iter().skip(1) {
                    write_indent(indent + 2, output);
                    output.push_str(&render_key(key));
                    match value {
                        YamlValue::Object(_) | YamlValue::Array(_) => {
                            output.push_str(":\n");
                            render_value(value, indent + 4, output);
                        }
                        _ => {
                            output.push_str(": ");
                            output.push_str(&render_scalar(value, indent + 4));
                            output.push('\n');
                        }
                    }
                }
            }
            _ => {
                output.push_str("- ");
                output.push_str(&render_scalar(value, indent + 2));
                output.push('\n');
            }
        }
    }
}

fn render_key(key: &str) -> String {
    if needs_single_quotes(key) || key.ends_with(':') {
        format!("'{}'", key.replace('\'', "''"))
    } else {
        key.to_string()
    }
}

fn render_scalar(value: &YamlValue, indent: usize) -> String {
    match value {
        YamlValue::Null => "null".to_string(),
        YamlValue::Bool(value) => value.to_string(),
        YamlValue::Number(value) => value.to_string(),
        YamlValue::String(value) if value.contains('\n') => render_literal_block(value, indent),
        YamlValue::String(value) if value.len() > 100 => render_folded_block(value, indent),
        YamlValue::String(value) if needs_single_quotes(value) => {
            format!("'{}'", value.replace('\'', "''"))
        }
        YamlValue::String(value) => value.clone(),
        YamlValue::Array(values) if values.is_empty() => "[]".to_string(),
        _ => "null".to_string(),
    }
}

fn render_literal_block(value: &str, indent: usize) -> String {
    let mut output = String::from("|-\n");
    for line in value.split('\n') {
        write_indent(indent, &mut output);
        output.push_str(line);
        output.push('\n');
    }
    output.trim_end_matches('\n').to_string()
}

fn render_folded_block(value: &str, indent: usize) -> String {
    let mut output = String::from(">-\n");
    for line in wrap_words(value, 100 - indent) {
        write_indent(indent, &mut output);
        output.push_str(&line);
        output.push('\n');
    }
    output.trim_end_matches('\n').to_string()
}

fn wrap_words(value: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in value.split_whitespace() {
        if !current.is_empty() && current.len() + 1 + word.len() > width {
            lines.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn needs_single_quotes(value: &str) -> bool {
    value.is_empty()
        || value.starts_with([
            '-', '?', ':', '@', '`', '&', '*', '!', '|', '>', '\'', '"', '{', '}', '[', ']', ',',
        ])
        || value.contains(": ")
        || value.contains(" #")
        || value == "true"
        || value == "false"
        || value == "null"
        || value == "~"
        || value.parse::<i64>().is_ok()
        || looks_like_yaml_number(value)
        || looks_like_timestamp(value)
}

fn looks_like_yaml_number(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut index = 0;
    if matches!(bytes[index], b'+' | b'-') {
        index += 1;
    }
    if index >= bytes.len() {
        return false;
    }
    if bytes.get(index) == Some(&b'0') && matches!(bytes.get(index + 1), Some(b'x') | Some(b'X')) {
        index += 2;
        let digit_start = index;
        while index < bytes.len() && (bytes[index].is_ascii_hexdigit() || bytes[index] == b'_') {
            index += 1;
        }
        return index > digit_start && index == bytes.len();
    }

    let digit_start = index;
    while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'_') {
        index += 1;
    }
    if index == digit_start {
        return false;
    }
    let mut has_float_marker = false;
    if bytes.get(index) == Some(&b'.') {
        has_float_marker = true;
        index += 1;
        let fractional_start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'_') {
            index += 1;
        }
        if index == fractional_start {
            return false;
        }
    }
    if matches!(bytes.get(index), Some(b'e') | Some(b'E')) {
        has_float_marker = true;
        index += 1;
        if matches!(bytes.get(index), Some(b'+') | Some(b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'_') {
            index += 1;
        }
        if index == exponent_start {
            return false;
        }
    }
    has_float_marker && index == bytes.len()
}

fn looks_like_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 20
        && matches!(bytes.get(4), Some(b'-'))
        && matches!(bytes.get(7), Some(b'-'))
        && matches!(bytes.get(10), Some(b'T' | b' '))
        && matches!(bytes.get(13), Some(b':'))
        && matches!(bytes.get(16), Some(b':'))
}

fn fold_block_scalar(lines: &[String], strip_final_newline: bool) -> String {
    let mut output = String::new();
    let mut previous_blank = false;
    for line in lines {
        if line.is_empty() {
            if !output.ends_with('\n') && !output.is_empty() {
                output.push('\n');
            }
            previous_blank = true;
            continue;
        }
        if !output.is_empty() && !output.ends_with('\n') {
            output.push(' ');
        } else if previous_blank && !output.is_empty() {
            output.push('\n');
        }
        output.push_str(line.trim_end());
        previous_blank = false;
    }
    if !strip_final_newline {
        output.push('\n');
    }
    output
}

fn literal_block_scalar(lines: &[String], strip_final_newline: bool) -> String {
    let mut output = lines.join("\n");
    if !strip_final_newline {
        output.push('\n');
    }
    output
}

fn split_mapping_entry(content: &str, line: usize) -> Result<Option<(String, &str)>, YamlError> {
    let bytes = content.as_bytes();
    let mut index = 0;
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    while index < bytes.len() {
        if let Some(active_quote) = quote {
            if active_quote == b'"' && !escaped && bytes[index] == b'\\' {
                escaped = true;
                index += 1;
                continue;
            }
            if bytes[index] == active_quote {
                if active_quote == b'\'' && bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                    continue;
                }
                if !escaped {
                    quote = None;
                }
            }
            escaped = false;
            index += 1;
            continue;
        }
        if index == 0 && matches!(bytes[index], b'\'' | b'"') {
            quote = Some(bytes[index]);
            index += 1;
            continue;
        }
        if bytes[index] == b':' && (index + 1 == bytes.len() || bytes[index + 1] == b' ') {
            let raw_key = content[..index].trim_end();
            return Ok(Some((
                parse_mapping_key(raw_key, line)?,
                content[index + 1..].trim_start(),
            )));
        }
        index += 1;
    }
    Ok(None)
}

fn parse_mapping_key(raw_key: &str, line: usize) -> Result<String, YamlError> {
    if let Some(key) = parse_single_quoted(raw_key) {
        return Ok(key);
    }
    if raw_key.starts_with('"') {
        return parse_double_quoted(raw_key, line);
    }
    Ok(raw_key.to_string())
}

fn strip_plain_comment(input: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut flow_depth = 0_usize;
    let mut previous_was_space = true;
    for (index, character) in input.char_indices() {
        if let Some(active_quote) = quote {
            if active_quote == '"' && !escaped && character == '\\' {
                escaped = true;
                previous_was_space = false;
                continue;
            }
            if !escaped && character == active_quote {
                quote = None;
            }
            escaped = false;
            previous_was_space = character.is_whitespace();
            continue;
        }
        if character == '\'' || character == '"' {
            quote = Some(character);
            previous_was_space = false;
            continue;
        }
        match character {
            '[' | '{' => flow_depth += 1,
            ']' | '}' => flow_depth = flow_depth.saturating_sub(1),
            _ => {}
        }
        if character == '#' && flow_depth == 0 && previous_was_space {
            return &input[..index];
        }
        previous_was_space = character.is_whitespace();
    }
    input
}

fn count_indent(line: &str) -> Result<usize, YamlError> {
    let mut count = 0;
    for character in line.chars() {
        match character {
            ' ' => count += 1,
            '\t' => return Err(YamlError::new("Tabs are not supported for indentation", 1)),
            _ => break,
        }
    }
    Ok(count)
}

fn split_lines(input: &str) -> Vec<SourceLine> {
    input
        .split('\n')
        .enumerate()
        .map(|(index, line)| SourceLine {
            number: index + 1,
            text: line.strip_suffix('\r').unwrap_or(line).to_string(),
        })
        .collect()
}

fn write_indent(indent: usize, output: &mut String) {
    for _ in 0..indent {
        output.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_yaml, parse_yaml_with_options, render_yaml, ParseOptions, YamlObject, YamlValue,
    };

    #[test]
    fn parses_dlog_reader_subset_contract() {
        let value = parse_yaml(
            "# top-level comments are ignored\nfile: src/yaml.ts\nschema: 1\ndecisions:\n  fn:yaml:\n    id: \"dec_011\"\n    lines_hint: [1, 3]\n    fingerprint: abc123ef\n    chose: \"double quoted choice\"\n    because: >-\n      folded line one\n      folded line two\n    rejected:\n      - approach: 'single quoted: option # literal'\n        reason: |-\n          literal first\n          literal second\n    timestamp: '2026-06-26T20:31:18.340Z'\n",
        )
        .unwrap();

        let root = as_object(&value);
        assert_eq!(
            root.get("file"),
            Some(&YamlValue::String("src/yaml.ts".to_string()))
        );
        assert_eq!(root.get("schema"), Some(&YamlValue::Number(1)));
        let decisions = as_object(root.get("decisions").unwrap());
        let decision = as_object(decisions.get("fn:yaml").unwrap());
        assert_eq!(
            decision.get("lines_hint"),
            Some(&YamlValue::Array(vec![
                YamlValue::Number(1),
                YamlValue::Number(3)
            ]))
        );
        assert_eq!(
            decision.get("because"),
            Some(&YamlValue::String(
                "folded line one folded line two".to_string()
            ))
        );
        let rejected = as_array(decision.get("rejected").unwrap());
        let rejected_entry = as_object(&rejected[0]);
        assert_eq!(
            rejected_entry.get("approach"),
            Some(&YamlValue::String(
                "single quoted: option # literal".to_string()
            ))
        );
        assert_eq!(
            rejected_entry.get("reason"),
            Some(&YamlValue::String(
                "literal first\nliteral second".to_string()
            ))
        );

        let empty = parse_yaml("decisions: {}\n").unwrap();
        assert!(as_object(as_object(&empty).get("decisions").unwrap())
            .entries()
            .is_empty());
    }

    #[test]
    fn renders_canonical_yaml_subset() {
        let value = YamlValue::Object(YamlObject::from_entries(vec![
            (
                "file".to_string(),
                YamlValue::String("src/wrap.ts".to_string()),
            ),
            ("schema".to_string(), YamlValue::Number(1)),
            (
                "lines_hint".to_string(),
                YamlValue::Array(vec![YamlValue::Number(2), YamlValue::Number(8)]),
            ),
            (
                "timestamp".to_string(),
                YamlValue::String("2026-06-26T20:31:18.340Z".to_string()),
            ),
            (
                "fingerprint_like_float".to_string(),
                YamlValue::String("5e026200".to_string()),
            ),
            (
                "rejected".to_string(),
                YamlValue::Array(vec![YamlValue::Object(YamlObject::from_entries(vec![
                    (
                        "approach".to_string(),
                        YamlValue::String("flow array [x, y]".to_string()),
                    ),
                    (
                        "reason".to_string(),
                        YamlValue::String("keeps mapping sequence shape".to_string()),
                    ),
                ]))]),
            ),
            ("history".to_string(), YamlValue::Array(Vec::new())),
        ]));

        assert_eq!(
            render_yaml(&value),
            "file: src/wrap.ts\nschema: 1\nlines_hint:\n  - 2\n  - 8\ntimestamp: '2026-06-26T20:31:18.340Z'\nfingerprint_like_float: '5e026200'\nrejected:\n  - approach: flow array [x, y]\n    reason: keeps mapping sequence shape\nhistory: []\n"
        );
    }

    #[test]
    fn quotes_mapping_keys_that_end_with_colon() {
        let value = YamlValue::Object(YamlObject::from_entries(vec![
            (
                "export:".to_string(),
                YamlValue::String("empty alias".to_string()),
            ),
            (
                "fn:real".to_string(),
                YamlValue::String("plain".to_string()),
            ),
            (
                "fn:Numeric.\"with\\\"quote\"".to_string(),
                YamlValue::String("embedded quote".to_string()),
            ),
            (
                "fn:Computed.[bad ? { x: 1 } : key]".to_string(),
                YamlValue::String("computed".to_string()),
            ),
        ]));

        let rendered = render_yaml(&value);
        assert_eq!(
            rendered,
            "'export:': empty alias\nfn:real: plain\nfn:Numeric.\"with\\\"quote\": embedded quote\n'fn:Computed.[bad ? { x: 1 } : key]': computed\n"
        );
        let parsed = parse_yaml(&rendered).unwrap();
        let object = match parsed {
            YamlValue::Object(object) => object,
            _ => panic!("expected object"),
        };
        assert_eq!(
            object.get("export:"),
            Some(&YamlValue::String("empty alias".to_string()))
        );
        assert_eq!(
            object.get("fn:Computed.[bad ? { x: 1 } : key]"),
            Some(&YamlValue::String("computed".to_string()))
        );
        assert_eq!(
            object.get("fn:Numeric.\"with\\\"quote\""),
            Some(&YamlValue::String("embedded quote".to_string()))
        );
    }

    #[test]
    fn parses_flow_mappings_and_nested_flow_collections() {
        let value = parse_yaml(
            "decisions: {fn:yaml: {id: dec_001, lines_hint: [1, 3], rejected: [{approach: \"flow, mapping\", reason: 'single quoted: # literal'}]}, \"quoted:key\": [true, null, {inner: [2, 4]}]}\n",
        )
        .unwrap();

        let root = as_object(&value);
        let decisions = as_object(root.get("decisions").unwrap());
        let decision = as_object(decisions.get("fn:yaml").unwrap());
        assert_eq!(
            decision.get("lines_hint"),
            Some(&YamlValue::Array(vec![
                YamlValue::Number(1),
                YamlValue::Number(3)
            ]))
        );
        let rejected = as_array(decision.get("rejected").unwrap());
        let rejected_entry = as_object(&rejected[0]);
        assert_eq!(
            rejected_entry.get("approach"),
            Some(&YamlValue::String("flow, mapping".to_string()))
        );
        assert_eq!(
            rejected_entry.get("reason"),
            Some(&YamlValue::String("single quoted: # literal".to_string()))
        );

        let quoted = as_array(decisions.get("quoted:key").unwrap());
        assert_eq!(quoted[0], YamlValue::Bool(true));
        assert_eq!(quoted[1], YamlValue::Null);
        let nested = as_object(&quoted[2]);
        assert_eq!(
            nested.get("inner"),
            Some(&YamlValue::Array(vec![
                YamlValue::Number(2),
                YamlValue::Number(4)
            ]))
        );
    }

    #[test]
    fn decodes_double_quoted_keys_and_extended_escapes() {
        let value = parse_yaml(
            "\"fn:line\\nkey\": \"A\\x21 \\u263A \\uD83D\\uDE00 \\U0001F642 \\N\\_\\L\\P\\ \"\n",
        )
        .unwrap();

        let object = as_object(&value);
        assert_eq!(
            object.get("fn:line\nkey"),
            Some(&YamlValue::String(
                "A! ☺ 😀 🙂 \u{0085}\u{00a0}\u{2028}\u{2029} ".to_string()
            ))
        );
    }

    #[test]
    fn rejects_unsupported_or_malformed_yaml() {
        assert!(parse_yaml("file: \"unterminated\n")
            .unwrap_err()
            .message()
            .contains("Unterminated"));
        assert!(parse_yaml("file: { unsupported: [true }\n")
            .unwrap_err()
            .message()
            .contains("flow"));
        assert!(parse_yaml("items: [\"unterminated]\n")
            .unwrap_err()
            .message()
            .contains("Unterminated"));
    }

    #[test]
    fn enforces_configured_depth_limit_for_block_yaml() {
        let accepted = parse_yaml_with_options(
            "root:\n  child:\n    grandchild: value\n",
            ParseOptions { max_depth: 2 },
        )
        .unwrap();
        assert_eq!(
            as_object(as_object(&accepted).get("root").unwrap()).get("child"),
            Some(&YamlValue::Object(YamlObject::from_entries(vec![(
                "grandchild".to_string(),
                YamlValue::String("value".to_string())
            )])))
        );

        let rejected = parse_yaml_with_options(
            "root:\n  child:\n    grandchild:\n      leaf: value\n",
            ParseOptions { max_depth: 2 },
        )
        .unwrap_err();
        assert!(rejected.message().contains("depth limit"));
    }

    #[test]
    fn enforces_configured_depth_limit_for_flow_yaml() {
        let accepted = parse_yaml_with_options(
            "root: {child: [1, {leaf: 2}]}\n",
            ParseOptions { max_depth: 4 },
        )
        .unwrap();
        let root = as_object(as_object(&accepted).get("root").unwrap());
        let child = as_array(root.get("child").unwrap());
        assert_eq!(child[0], YamlValue::Number(1));

        let rejected =
            parse_yaml_with_options("root: [[[[[[1]]]]]]\n", ParseOptions { max_depth: 4 })
                .unwrap_err();
        assert!(rejected.message().contains("depth limit"));
    }

    fn as_object(value: &YamlValue) -> &YamlObject {
        match value {
            YamlValue::Object(object) => object,
            _ => panic!("expected object, got {value:?}"),
        }
    }

    fn as_array(value: &YamlValue) -> &[YamlValue] {
        match value {
            YamlValue::Array(values) => values,
            _ => panic!("expected array, got {value:?}"),
        }
    }
}
