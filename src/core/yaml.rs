use std::error::Error;
use std::fmt;

pub const DEFAULT_MAX_DEPTH: usize = 512;

/// YAML whitespace is exactly space and tab (`s-white`), unlike Rust's
/// `char::is_whitespace`, which also matches many Unicode separators (NBSP,
/// em-space, ideographic space, …). Plain-scalar value boundaries must be
/// trimmed with only these two so a value the renderer emits plain (js-yaml
/// only escapes/quotes tab and non-space Unicode whitespace via double quotes,
/// leaving e.g. a trailing em-space in a plain scalar) round-trips intact —
/// trimming with the full Unicode set silently deleted such characters.
const YAML_WHITESPACE: [char; 2] = [' ', '\t'];

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
            // The key sits at column `indent + 2` (past the `- ` marker), so its
            // value — including any block scalar or nested block — is measured
            // against `indent + 2`, matching `parse_additional_item_mapping_entries`.
            let value = self.parse_mapping_value(value_part, indent + 2, depth + 1)?;
            object.insert(key, value);
            self.parse_additional_item_mapping_entries(indent + 2, depth + 1, &mut object)?;
            return Ok(YamlValue::Object(object));
        }

        // A block scalar as a direct sequence item (`- |`, `- >-`, …): its
        // content is indented deeper than the dash, so parse it with the
        // sequence's own indent as the parent.
        if let Some(header) = parse_block_scalar_header(rest.trim()) {
            return self.parse_block_scalar(header, indent);
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
        let trimmed = value_part.trim_start_matches(YAML_WHITESPACE);
        if trimmed.is_empty() {
            let child_indent = self.next_content_indent().unwrap_or(indent + 2);
            return self.parse_block(child_indent, depth);
        }
        if let Some(header) = parse_block_scalar_header(trimmed) {
            return self.parse_block_scalar(header, indent);
        }
        parse_scalar_with_options(trimmed, self.previous_line_number(), self.options, depth)
    }

    fn parse_block_scalar(
        &mut self,
        header: BlockScalarHeader,
        parent_indent: usize,
    ) -> Result<YamlValue, YamlError> {
        // Collect the raw text of every line belonging to the block: a line
        // belongs while it is blank or indented deeper than the parent key.
        // `split_lines` appends one empty entry for the document's final
        // newline; when the block runs to EOF that trailing entry is the line
        // terminator, not real content, so it is dropped below.
        let mut raw_lines: Vec<String> = Vec::new();
        let mut block_indent: Option<usize> = header.explicit_indent.map(|d| parent_indent + d);
        while self.index < self.lines.len() {
            let raw = &self.lines[self.index];
            let leading_spaces = raw.text.bytes().take_while(|byte| *byte == b' ').count();
            let is_all_whitespace = raw.text[leading_spaces..].trim().is_empty();

            if is_all_whitespace {
                // A whitespace-only line is empty unless the block indent is
                // already known and the line carries spaces *beyond* that indent
                // — those extra spaces are more-indented content (js-yaml
                // consumes up to `textIndent` spaces, then treats a non-EOL
                // remainder as content). Before the indent is detected, or when
                // the line is not deeper than the indent, it is a blank line.
                match block_indent {
                    Some(content_indent) if leading_spaces > content_indent => {
                        raw_lines.push(raw.text[content_indent..].to_string());
                    }
                    _ => raw_lines.push(String::new()),
                }
                self.index += 1;
                continue;
            }
            let indent = count_indent(&raw.text)?;
            if indent <= parent_indent {
                break;
            }
            // Auto-detect the block indent from the first content line when no
            // explicit indentation indicator was given.
            let content_indent = *block_indent.get_or_insert(indent);
            let cut = content_indent.min(leading_spaces);
            raw_lines.push(raw.text[cut..].to_string());
            self.index += 1;
        }
        let at_eof = self.index >= self.lines.len();
        if at_eof && raw_lines.last().is_some_and(|line| line.is_empty()) {
            // Drop the phantom trailing entry produced by the document's final
            // newline so chomping sees the true blank-line count.
            raw_lines.pop();
        }

        Ok(YamlValue::String(assemble_block_scalar(&raw_lines, header)))
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
    let value = strip_plain_comment(input).trim_end_matches(YAML_WHITESPACE);
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
    // A valid single-quoted scalar needs both a leading and a trailing quote,
    // which requires at least two bytes. A lone `'` satisfies both
    // starts_with/ends_with (same byte) but must not be treated as an
    // (empty) quoted scalar — slicing `input[1..0]` would panic.
    if input.len() < 2 || !input.starts_with('\'') || !input.ends_with('\'') {
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
        YamlValue::String(value) => write_string_scalar(value, indent),
        YamlValue::Array(values) if values.is_empty() => "[]".to_string(),
        _ => "null".to_string(),
    }
}

/// The block-scalar / quote / plain style chosen for a string, mirroring
/// js-yaml's `STYLE_*` constants. Byte-for-byte parity with `yaml.dump`
/// (the behavioral spec) is required: the `.dlog`/`.dmap` differential harness
/// compares rendered files directly against the TypeScript output.
#[derive(Clone, Copy, PartialEq)]
enum ScalarStyle {
    Plain,
    Single,
    Literal,
    Folded,
    Double,
}

/// Render a string scalar exactly as js-yaml's `writeScalar` does for the
/// options Archiva uses (`lineWidth: 100`, default indent 2, single quoting).
///
/// `indent` is the column the *value* is rendered at, matching js-yaml's
/// `indent = state.indent * max(1, level)`. The folding width decreases with
/// indent but is clamped to a floor of 40 — this floor is also what makes the
/// arithmetic panic-free (the previous `100 - indent` underflowed `usize` and
/// aborted on deeply nested values, audit-flagged data-corruption bug).
fn write_string_scalar(value: &str, indent: usize) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    // js-yaml quotes deprecated YAML 1.1 boolean spellings and base-60 numbers
    // to avoid ambiguity (`noCompatMode` is off by default).
    if is_deprecated_boolean(value) || is_base60(value) {
        return format!("'{}'", value.replace('\'', "''"));
    }

    // Width floor mirrors `max(min(lineWidth, 40), lineWidth - indent)`.
    let line_width = 100usize.saturating_sub(indent).max(40);

    match choose_scalar_style(value, line_width) {
        ScalarStyle::Plain => value.to_string(),
        ScalarStyle::Single => format!("'{}'", value.replace('\'', "''")),
        ScalarStyle::Literal => {
            format!(
                "|{}{}",
                block_header(value),
                drop_ending_newline(&indent_string(value, indent))
            )
        }
        ScalarStyle::Folded => {
            format!(
                ">{}{}",
                block_header(value),
                drop_ending_newline(&indent_string(&fold_string(value, line_width), indent))
            )
        }
        ScalarStyle::Double => format!("\"{}\"", escape_double_quoted(value)),
    }
}

/// Port of js-yaml `chooseScalarStyle` for `singleLineOnly = false`,
/// `quotingType = single`, `forceQuotes = false`.
fn choose_scalar_style(string: &str, line_width: usize) -> ScalarStyle {
    let chars: Vec<char> = string.chars().collect();
    let first = chars[0];
    let last = chars[chars.len() - 1];
    let mut plain = is_plain_safe_first(first) && is_plain_safe_last(last);

    let mut has_line_break = false;
    let mut has_foldable_line = false;
    let width = line_width as isize;
    // js-yaml tracks positions in UTF-16 code units (`string.length` / `i`),
    // advancing the index by 2 for astral chars. Line width and break points
    // are therefore measured in code units, not scalar values — mirror that
    // here with `len_utf16` so folding matches `yaml.dump` byte-for-byte on
    // multibyte (CJK/emoji) text.
    let mut u16_index: isize = 0;
    let mut previous_line_break: isize = -1;
    let mut line_first_is_space = first == ' ';
    let mut prev_char: Option<char> = None;

    for (char_index, &char) in chars.iter().enumerate() {
        if char == '\n' {
            has_line_break = true;
            // Foldable = the line just closed is longer than the width and is
            // not more-indented (its first char is not a space).
            let line_len = u16_index - previous_line_break - 1;
            has_foldable_line = has_foldable_line || (line_len > width && !line_first_is_space);
            previous_line_break = u16_index;
            line_first_is_space = chars.get(char_index + 1) == Some(&' ');
        } else if !is_printable(char) {
            return ScalarStyle::Double;
        }
        plain = plain && is_plain_safe(char, prev_char, true);
        prev_char = Some(char);
        u16_index += char.len_utf16() as isize;
    }
    // Trailing line (no closing `\n`).
    let line_len = u16_index - previous_line_break - 1;
    has_foldable_line = has_foldable_line || (line_len > width && !line_first_is_space);

    if !has_line_break && !has_foldable_line {
        if plain && !is_ambiguous_plain(string) {
            return ScalarStyle::Plain;
        }
        return ScalarStyle::Single;
    }
    // Block styles are valid. (js-yaml's `indentPerLevel > 9` edge case cannot
    // occur here: Archiva always dumps with the default indent of 2.)
    if has_foldable_line {
        ScalarStyle::Folded
    } else {
        ScalarStyle::Literal
    }
}

/// Whether a string would be re-parsed as a non-string scalar under js-yaml's
/// default schema and therefore cannot be emitted plain (`testImplicitResolving`).
/// These are faithful ports of the implicit `resolve*` functions js-yaml checks
/// — null, bool, int, float, timestamp, merge — so the chosen scalar style, and
/// thus the rendered `.dlog`/`.dmap` bytes, match `yaml.dump` exactly.
fn is_ambiguous_plain(value: &str) -> bool {
    resolve_yaml_null(value)
        || resolve_yaml_bool(value)
        || resolve_yaml_int(value)
        || resolve_yaml_float(value)
        || resolve_yaml_timestamp(value)
        || value == "<<"
}

// Port of js-yaml `resolveYamlNull`.
fn resolve_yaml_null(data: &str) -> bool {
    data == "~" || data == "null" || data == "Null" || data == "NULL"
}

// Port of js-yaml `resolveYamlBoolean`.
fn resolve_yaml_bool(data: &str) -> bool {
    matches!(data, "true" | "True" | "TRUE" | "false" | "False" | "FALSE")
}

// Port of js-yaml `resolveYamlInteger`.
fn resolve_yaml_int(data: &str) -> bool {
    let bytes = data.as_bytes();
    let max = bytes.len();
    if max == 0 {
        return false;
    }
    let mut index = 0;
    let mut ch = bytes[index];
    if ch == b'-' || ch == b'+' {
        index += 1;
        if index >= max {
            return false;
        }
        ch = bytes[index];
    }

    if ch == b'0' {
        if index + 1 == max {
            return true; // "0"
        }
        index += 1;
        ch = bytes[index];
        if ch == b'b' {
            index += 1;
            let mut has_digits = false;
            let mut last = ch;
            while index < max {
                last = bytes[index];
                if last == b'_' {
                    index += 1;
                    continue;
                }
                if last != b'0' && last != b'1' {
                    return false;
                }
                has_digits = true;
                index += 1;
            }
            return has_digits && last != b'_';
        }
        if ch == b'x' {
            index += 1;
            let mut has_digits = false;
            let mut last = ch;
            while index < max {
                last = bytes[index];
                if last == b'_' {
                    index += 1;
                    continue;
                }
                if !last.is_ascii_hexdigit() {
                    return false;
                }
                has_digits = true;
                index += 1;
            }
            return has_digits && last != b'_';
        }
        if ch == b'o' {
            index += 1;
            let mut has_digits = false;
            let mut last = ch;
            while index < max {
                last = bytes[index];
                if last == b'_' {
                    index += 1;
                    continue;
                }
                if !(b'0'..=b'7').contains(&last) {
                    return false;
                }
                has_digits = true;
                index += 1;
            }
            return has_digits && last != b'_';
        }
    }

    // base 10 (except leading 0 handled above)
    if ch == b'_' {
        return false;
    }
    let mut has_digits = false;
    let mut last = ch;
    while index < max {
        last = bytes[index];
        if last == b'_' {
            index += 1;
            continue;
        }
        if !last.is_ascii_digit() {
            return false;
        }
        has_digits = true;
        index += 1;
    }
    has_digits && last != b'_'
}

// Port of js-yaml `resolveYamlFloat` / `YAML_FLOAT_PATTERN`:
//   ^(?:[-+]?(?:[0-9][0-9_]*)(?:\.[0-9_]*)?(?:[eE][-+]?[0-9]+)?
//    |\.[0-9_]+(?:[eE][-+]?[0-9]+)?
//    |[-+]?\.(?:inf|Inf|INF)
//    |\.(?:nan|NaN|NAN))$
// plus the "must not end with `_`" guard.
fn resolve_yaml_float(data: &str) -> bool {
    if data.is_empty() || data.as_bytes()[data.len() - 1] == b'_' {
        return false;
    }
    float_alt_signed_mantissa(data)
        || float_alt_dot_leading(data)
        || float_alt_inf(data)
        || float_alt_nan(data)
}

// [-+]? [0-9][0-9_]* (\.[0-9_]*)? ([eE][-+]?[0-9]+)?
fn float_alt_signed_mantissa(data: &str) -> bool {
    let b = data.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    // [0-9][0-9_]*
    if i >= b.len() || !b[i].is_ascii_digit() {
        return false;
    }
    i += 1;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    // (\.[0-9_]*)?
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
            i += 1;
        }
    }
    i = match parse_exponent(b, i) {
        Some(next) => next,
        None => return false,
    };
    i == b.len()
}

// \.[0-9_]+ ([eE][-+]?[0-9]+)?
fn float_alt_dot_leading(data: &str) -> bool {
    let b = data.as_bytes();
    let mut i = 0;
    if i >= b.len() || b[i] != b'.' {
        return false;
    }
    i += 1;
    let digit_start = i;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    if i == digit_start {
        return false;
    }
    i = match parse_exponent(b, i) {
        Some(next) => next,
        None => return false,
    };
    i == b.len()
}

// Optional ([eE][-+]?[0-9]+); returns the index after it, or None if a partial
// exponent is present but malformed.
fn parse_exponent(b: &[u8], mut i: usize) -> Option<usize> {
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        let digit_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == digit_start {
            return None;
        }
    }
    Some(i)
}

// [-+]?\.(inf|Inf|INF)
fn float_alt_inf(data: &str) -> bool {
    let rest = data.strip_prefix(['-', '+']).unwrap_or(data);
    matches!(rest, ".inf" | ".Inf" | ".INF")
}

// \.(nan|NaN|NAN)
fn float_alt_nan(data: &str) -> bool {
    matches!(data, ".nan" | ".NaN" | ".NAN")
}

// Port of js-yaml `resolveYamlTimestamp` (YAML_DATE_REGEXP | YAML_TIMESTAMP_REGEXP).
fn resolve_yaml_timestamp(data: &str) -> bool {
    matches_yaml_date(data) || matches_yaml_timestamp(data)
}

// ^\d{4}-\d{2}-\d{2}$
fn matches_yaml_date(data: &str) -> bool {
    let b = data.as_bytes();
    b.len() == 10
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
        && b[7] == b'-'
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit()
}

// ^\d{4}-\d{1,2}-\d{1,2}([Tt]|[ \t]+)\d{1,2}:\d{2}:\d{2}(\.\d*)?
//  ([ \t]*(Z|[-+]\d{1,2}(:\d{2})?))?$
fn matches_yaml_timestamp(data: &str) -> bool {
    let b = data.as_bytes();
    let mut i = 0;
    // year: 4 digits
    if b.len() < 4 || !b[0..4].iter().all(u8::is_ascii_digit) {
        return false;
    }
    i += 4;
    // -month (1-2 digits)
    if !consume_byte(b, &mut i, b'-') || !consume_digits(b, &mut i, 1, 2) {
        return false;
    }
    // -day (1-2 digits)
    if !consume_byte(b, &mut i, b'-') || !consume_digits(b, &mut i, 1, 2) {
        return false;
    }
    // (T | t | [ \t]+)
    if i < b.len() && (b[i] == b'T' || b[i] == b't') {
        i += 1;
    } else {
        let ws_start = i;
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        if i == ws_start {
            return false;
        }
    }
    // hour (1-2), :minute (2), :second (2)
    if !consume_digits(b, &mut i, 1, 2) {
        return false;
    }
    if !consume_byte(b, &mut i, b':') || !consume_digits(b, &mut i, 2, 2) {
        return false;
    }
    if !consume_byte(b, &mut i, b':') || !consume_digits(b, &mut i, 2, 2) {
        return false;
    }
    // (\.\d*)?
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    // ([ \t]*(Z | [-+]\d{1,2}(:\d{2})?))?
    let mut j = i;
    while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
        j += 1;
    }
    if j < b.len() {
        if b[j] == b'Z' {
            j += 1;
        } else if b[j] == b'-' || b[j] == b'+' {
            j += 1;
            if !consume_digits(b, &mut j, 1, 2) {
                return false;
            }
            if j < b.len() && b[j] == b':' {
                j += 1;
                if !consume_digits(b, &mut j, 2, 2) {
                    return false;
                }
            }
        } else {
            return false;
        }
        i = j;
    }
    // If there was trailing whitespace but no tz, the regex would not match it,
    // so require the whole string to be consumed only after the optional tz.
    i == b.len()
}

fn consume_byte(b: &[u8], i: &mut usize, expected: u8) -> bool {
    if *i < b.len() && b[*i] == expected {
        *i += 1;
        true
    } else {
        false
    }
}

fn consume_digits(b: &[u8], i: &mut usize, min: usize, maxd: usize) -> bool {
    let start = *i;
    while *i < b.len() && *i - start < maxd && b[*i].is_ascii_digit() {
        *i += 1;
    }
    *i - start >= min
}

// [33] s-white ::= s-space | s-tab
fn is_yaml_whitespace(c: char) -> bool {
    c == ' ' || c == '\t'
}

// Printable per YAML 1.2 (js-yaml `isPrintable`): control chars, DEL, the
// BOM, and the line/paragraph separators are not printable and force escaping.
fn is_printable(c: char) -> bool {
    let c = c as u32;
    (0x00020..=0x00007E).contains(&c)
        || ((0x000A1..=0x00D7FF).contains(&c) && c != 0x2028 && c != 0x2029)
        || ((0x0E000..=0x00FFFD).contains(&c) && c != 0xFEFF)
        || (0x10000..=0x10FFFF).contains(&c)
}

fn is_ns_char_or_whitespace(c: char) -> bool {
    is_printable(c) && c != '\u{FEFF}' && c != '\r' && c != '\n'
}

// Port of js-yaml `isPlainSafe`. `inblock` selects the flow-in relaxation:
// inside a block scalar the flow indicators `,[]{}` are plain-safe, whereas in
// flow-out context they are not. Archiva renders all values in block context,
// so callers pass `inblock = true` (matching `writeNode`'s `inblock = block`).
fn is_plain_safe(c: char, prev: Option<char>, inblock: bool) -> bool {
    let c_is_ns_or_ws = is_ns_char_or_whitespace(c);
    let c_is_ns = c_is_ns_or_ws && !is_yaml_whitespace(c);
    let prev_is_ns = prev.is_some_and(|p| is_ns_char_or_whitespace(p) && !is_yaml_whitespace(p));

    let base = if inblock {
        c_is_ns_or_ws
    } else {
        c_is_ns_or_ws && c != ',' && c != '[' && c != ']' && c != '{' && c != '}'
    };

    (base && c != '#' && (prev != Some(':') || c_is_ns))
        || (prev_is_ns && c == '#')
        || (prev == Some(':') && c_is_ns)
}

// Port of js-yaml `isPlainSafeFirst`.
fn is_plain_safe_first(c: char) -> bool {
    is_printable(c)
        && c != '\u{FEFF}'
        && !is_yaml_whitespace(c)
        && !matches!(
            c,
            '-' | '?'
                | ':'
                | ','
                | '['
                | ']'
                | '{'
                | '}'
                | '#'
                | '&'
                | '*'
                | '!'
                | '|'
                | '='
                | '>'
                | '\''
                | '"'
                | '%'
                | '@'
                | '`'
        )
}

// Port of js-yaml `isPlainSafeLast`.
fn is_plain_safe_last(c: char) -> bool {
    !is_yaml_whitespace(c) && c != ':'
}

// js-yaml `needIndentIndicator`: leading `\n*` then a space.
fn need_indent_indicator(string: &str) -> bool {
    let trimmed = string.trim_start_matches('\n');
    trimmed.starts_with(' ')
}

// Port of js-yaml `blockHeader`. Archiva's indent is always 2 (single digit),
// so the indent indicator, when required, is exactly "2".
fn block_header(string: &str) -> String {
    let indicator = if need_indent_indicator(string) {
        "2"
    } else {
        ""
    };
    let bytes = string.as_bytes();
    let clip = bytes.last() == Some(&b'\n');
    let keep = clip && (bytes.len() >= 2 && bytes[bytes.len() - 2] == b'\n' || string == "\n");
    let chomp = if keep {
        "+"
    } else if clip {
        ""
    } else {
        "-"
    };
    format!("{indicator}{chomp}\n")
}

// Port of js-yaml `dropEndingNewline`.
fn drop_ending_newline(string: &str) -> String {
    string.strip_suffix('\n').unwrap_or(string).to_string()
}

// Port of js-yaml `indentString`: indent every non-empty line by `spaces`.
fn indent_string(string: &str, spaces: usize) -> String {
    let ind: String = " ".repeat(spaces);
    let mut result = String::new();
    let mut position = 0;
    let bytes = string.as_bytes();
    while position < bytes.len() {
        let line = match string[position..].find('\n') {
            Some(rel) => {
                let next = position + rel;
                let line = &string[position..=next];
                position = next + 1;
                line
            }
            None => {
                let line = &string[position..];
                position = bytes.len();
                line
            }
        };
        if !line.is_empty() && line != "\n" {
            result.push_str(&ind);
        }
        result.push_str(line);
    }
    result
}

// Port of js-yaml `foldString`. Consecutive newlines and more-indented lines
// (those starting with a space) are preserved rather than folded, so internal
// whitespace runs survive a round-trip — unlike the previous word-splitting
// wrapper, which collapsed them (audit-flagged data-corruption bug).
fn fold_string(string: &str, width: usize) -> String {
    let first_lf = string.find('\n').unwrap_or(string.len());
    let mut result = fold_line(&string[..first_lf], width);

    let mut prev_more_indented = string.starts_with('\n') || string.starts_with(' ');

    // Iterate `(\n+)([^\n]*)` chunks over the remainder.
    let rest = &string[first_lf..];
    let rest_chars: Vec<char> = rest.chars().collect();
    let mut i = 0;
    while i < rest_chars.len() {
        if rest_chars[i] != '\n' {
            // Should not happen: `first_lf` lands on a newline or end.
            break;
        }
        let mut prefix = String::new();
        while i < rest_chars.len() && rest_chars[i] == '\n' {
            prefix.push('\n');
            i += 1;
        }
        let mut line = String::new();
        while i < rest_chars.len() && rest_chars[i] != '\n' {
            line.push(rest_chars[i]);
            i += 1;
        }
        let more_indented = line.starts_with(' ');
        result.push_str(&prefix);
        if !prev_more_indented && !more_indented && !line.is_empty() {
            result.push('\n');
        }
        result.push_str(&fold_line(&line, width));
        prev_more_indented = more_indented;
    }
    result
}

// Port of js-yaml `foldLine`: greedy line breaking at ` [^ ]` boundaries.
// js-yaml indexes and slices the line by UTF-16 code units, so operate on the
// UTF-16 encoding here. Break points always fall on ASCII spaces (single code
// units) and the slice bounds land right after them, so no surrogate pair is
// ever split.
fn fold_line(line: &str, width: usize) -> String {
    if line.is_empty() || line.starts_with(' ') {
        return line.to_string();
    }
    let units: Vec<u16> = line.encode_utf16().collect();
    let width = width as isize;
    let mut start: isize = 0;
    let mut curr: isize = 0;
    let mut result = String::new();

    // Emulate `/ [^ ]/g`: a space (0x20) followed by a non-space; the match
    // index is the space position.
    let mut positions = Vec::new();
    let mut idx = 0;
    while idx + 1 < units.len() {
        if units[idx] == 0x20 && units[idx + 1] != 0x20 {
            positions.push(idx as isize);
        }
        idx += 1;
    }

    for &next in &positions {
        if next - start > width {
            let end = if curr > start { curr } else { next };
            result.push('\n');
            push_u16_slice(&mut result, &units, start, end);
            start = end + 1;
        }
        curr = next;
    }

    result.push('\n');
    if units.len() as isize - start > width && curr > start {
        push_u16_slice(&mut result, &units, start, curr);
        result.push('\n');
        push_u16_slice(&mut result, &units, curr + 1, units.len() as isize);
    } else {
        push_u16_slice(&mut result, &units, start, units.len() as isize);
    }

    // Drop the leading `\n` joiner (js-yaml `result.slice(1)`).
    result[1..].to_string()
}

fn push_u16_slice(out: &mut String, units: &[u16], start: isize, end: isize) {
    let start = start.max(0) as usize;
    let end = (end.max(0) as usize).min(units.len());
    if start >= end {
        return;
    }
    // Slice bounds fall after ASCII spaces, so the range is always a valid
    // UTF-16 subsequence; `from_utf16_lossy` is a defensive no-op here.
    out.push_str(&String::from_utf16_lossy(&units[start..end]));
}

// Port of js-yaml `escapeString` (double-quoted body).
fn escape_double_quoted(string: &str) -> String {
    let mut result = String::new();
    for c in string.chars() {
        if let Some(seq) = escape_sequence(c) {
            result.push_str(seq);
        } else if is_printable(c) {
            result.push(c);
        } else {
            result.push_str(&encode_hex(c));
        }
    }
    result
}

fn escape_sequence(c: char) -> Option<&'static str> {
    match c as u32 {
        0x00 => Some("\\0"),
        0x07 => Some("\\a"),
        0x08 => Some("\\b"),
        0x09 => Some("\\t"),
        0x0A => Some("\\n"),
        0x0B => Some("\\v"),
        0x0C => Some("\\f"),
        0x0D => Some("\\r"),
        0x1B => Some("\\e"),
        0x22 => Some("\\\""),
        0x5C => Some("\\\\"),
        0x85 => Some("\\N"),
        0xA0 => Some("\\_"),
        0x2028 => Some("\\L"),
        0x2029 => Some("\\P"),
        _ => None,
    }
}

// Port of js-yaml `encodeHex`.
fn encode_hex(c: char) -> String {
    let code = c as u32;
    let hex = format!("{code:X}");
    let (handle, width) = if code <= 0xFF {
        ('x', 2)
    } else if code <= 0xFFFF {
        ('u', 4)
    } else {
        ('U', 8)
    };
    format!("\\{handle}{:0>width$}", hex, width = width)
}

fn is_deprecated_boolean(value: &str) -> bool {
    matches!(
        value,
        "y" | "Y"
            | "yes"
            | "Yes"
            | "YES"
            | "on"
            | "On"
            | "ON"
            | "n"
            | "N"
            | "no"
            | "No"
            | "NO"
            | "off"
            | "Off"
            | "OFF"
    )
}

// js-yaml `DEPRECATED_BASE60_SYNTAX`: /^[-+]?[0-9_]+(?::[0-9_]+)+(?:\.[0-9_]*)?$/
fn is_base60(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut i = 0;
    if matches!(bytes.first(), Some(b'-') | Some(b'+')) {
        i += 1;
    }
    // First [0-9_]+ group.
    let group_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
        i += 1;
    }
    if i == group_start {
        return false;
    }
    // One or more (:[0-9_]+) groups.
    let mut colon_groups = 0;
    while i < bytes.len() && bytes[i] == b':' {
        i += 1;
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            i += 1;
        }
        if i == start {
            return false;
        }
        colon_groups += 1;
    }
    if colon_groups == 0 {
        return false;
    }
    // Optional (\.[0-9_]*).
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            i += 1;
        }
    }
    i == bytes.len()
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

/// Parsed leading indicators of a block scalar (`|`/`>` plus optional chomping
/// and explicit indentation width), mirroring js-yaml's `readBlockScalar`.
#[derive(Clone, Copy)]
struct BlockScalarHeader {
    folding: bool,
    chomping: Chomping,
    explicit_indent: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum Chomping {
    Clip,
    Strip,
    Keep,
}

/// Parse a block-scalar header token such as `|`, `>-`, `|+`, `>2`, `|2-`.
/// Returns `None` if the token is not a block-scalar indicator.
fn parse_block_scalar_header(token: &str) -> Option<BlockScalarHeader> {
    let bytes = token.as_bytes();
    let folding = match bytes.first()? {
        b'|' => false,
        b'>' => true,
        _ => return None,
    };
    let mut chomping = Chomping::Clip;
    let mut explicit_indent: Option<usize> = None;
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'+' | b'-' => {
                // A repeated chomping indicator is malformed; reject so the
                // token falls through to plain-scalar handling.
                if chomping != Chomping::Clip {
                    return None;
                }
                chomping = if bytes[i] == b'+' {
                    Chomping::Keep
                } else {
                    Chomping::Strip
                };
            }
            d @ b'1'..=b'9' => {
                if explicit_indent.is_some() {
                    return None;
                }
                explicit_indent = Some((d - b'0') as usize);
            }
            _ => return None,
        }
        i += 1;
    }
    Some(BlockScalarHeader {
        folding,
        chomping,
        explicit_indent,
    })
}

/// Assemble a block scalar from its indent-stripped content lines, applying
/// js-yaml's folding and chomping rules (`readBlockScalar`). Blank lines are
/// empty strings; more-indented lines retain their extra leading spaces.
fn assemble_block_scalar(lines: &[String], header: BlockScalarHeader) -> String {
    let mut result = String::new();
    let mut did_read_content = false;
    let mut at_more_indented = false;
    let mut empty_lines = 0usize;

    for line in lines {
        if line.is_empty() {
            empty_lines += 1;
            continue;
        }
        let more_indented = line.starts_with(' ');
        if header.folding {
            if more_indented {
                at_more_indented = true;
                result.push_str(&"\n".repeat(if did_read_content {
                    1 + empty_lines
                } else {
                    empty_lines
                }));
            } else if at_more_indented {
                at_more_indented = false;
                result.push_str(&"\n".repeat(empty_lines + 1));
            } else if empty_lines == 0 {
                if did_read_content {
                    result.push(' ');
                }
            } else {
                result.push_str(&"\n".repeat(empty_lines));
            }
        } else {
            result.push_str(&"\n".repeat(if did_read_content {
                1 + empty_lines
            } else {
                empty_lines
            }));
        }
        did_read_content = true;
        empty_lines = 0;
        result.push_str(line);
    }

    // End-of-scalar chomping.
    match header.chomping {
        Chomping::Keep => {
            result.push_str(&"\n".repeat(if did_read_content {
                1 + empty_lines
            } else {
                empty_lines
            }));
        }
        Chomping::Clip => {
            if did_read_content {
                result.push('\n');
            }
        }
        Chomping::Strip => {}
    }
    result
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
                content[index + 1..].trim_start_matches(YAML_WHITESPACE),
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

    fn roundtrip_string(value: &str) -> String {
        let mut object = YamlObject::new();
        object.insert("k".to_string(), YamlValue::String(value.to_string()));
        let document = YamlValue::Object(object);
        let rendered = render_yaml(&document);
        let parsed = parse_yaml(&rendered)
            .unwrap_or_else(|error| panic!("parse failed for {value:?}: {error:?}\n{rendered}"));
        match as_object(&parsed).get("k") {
            Some(YamlValue::String(round)) => round.clone(),
            other => panic!("expected string, got {other:?} for {value:?}\n{rendered}"),
        }
    }

    #[test]
    fn string_scalars_round_trip_without_corruption() {
        // Regression coverage for the block-scalar / folding / quoting rewrite:
        // every category below previously either corrupted the value on
        // round-trip or panicked in the renderer.
        let cases = [
            // Whitespace runs must survive folding (were collapsed by the old
            // word-splitting wrapper).
            "We picked the queue-based design.  It decouples producers from consumers and survives restarts.",
            "alpha    beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma",
            // CR / tab force double-quoting (were mangled into block scalars).
            "carriage\rreturn value that also runs well beyond one hundred characters to force a long scalar",
            "tab\tseparated value that likewise runs beyond one hundred characters to force a long scalar body",
            &("crlf\r\nspanning".to_string() + &"z".repeat(120)),
            // Trailing / leading whitespace on a long value.
            &("trailing spaces  ".to_string() + &"z".repeat(110)),
            &("   leading spaces ".to_string() + &"z".repeat(110)),
            // Multiline literal blocks (chomping must not add a trailing newline).
            "line one\nline two",
            "line one\nline two\n",
            "line one\nline two\n\n",
            "\n",
            // Plain multi-word text under the wrap width.
            "short plain value",
            // Unicode whitespace at the value boundary must survive: the
            // renderer emits it plain (only tab / space govern YAML trimming),
            // so a full-Unicode trim on parse would silently delete it.
            "value\u{2003}",
            "\u{2003}value",
            "value\u{3000}",
            "\u{1680}value\u{205F}",
            "\u{2003}",
        ];
        for case in cases {
            assert_eq!(roundtrip_string(case), *case, "round-trip mismatch");
        }
    }

    #[test]
    fn deeply_nested_long_string_renders_without_panicking() {
        // The old renderer computed `100 - indent` for the fold width, which
        // underflowed `usize` and aborted once the value sat past column 100.
        let mut value =
            YamlValue::String("a long value that exceeds one hundred characters ".repeat(3));
        for index in 0..80 {
            let mut object = YamlObject::new();
            object.insert(format!("k{index}"), value);
            value = YamlValue::Object(object);
        }
        let rendered = render_yaml(&value);
        assert_eq!(parse_yaml(&rendered).unwrap(), value);
    }
}
