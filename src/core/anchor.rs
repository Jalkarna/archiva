#![allow(clippy::too_many_arguments, clippy::needless_range_loop)]

use std::collections::HashMap;

use crate::core::error::{ArchivaError, Result};
use crate::core::ordered_map::OrderedMap;
use crate::core::paths::RelativePath;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnchorKind {
    Function,
    Class,
    Struct,
    Enum,
    Trait,
    Module,
    Impl,
    Method,
    Export,
    Block,
}

impl AnchorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Trait => "trait",
            Self::Module => "module",
            Self::Impl => "impl",
            Self::Method => "method",
            Self::Export => "export",
            Self::Block => "block",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorInfo {
    pub anchor: String,
    pub start: u32,
    pub end: u32,
    pub complexity: u32,
    pub kind: AnchorKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnchorDiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorDiagnostic {
    pub severity: AnchorDiagnosticSeverity,
    pub line: u32,
    pub column: u32,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnchorExtraction {
    pub anchors: OrderedMap<String, AnchorInfo>,
    pub diagnostics: Vec<AnchorDiagnostic>,
    pub complete: bool,
}

#[derive(Clone, Debug)]
struct ExportCandidate {
    name: String,
    start: u32,
    end: u32,
    complexity: u32,
    priority_group: usize,
    order: usize,
    function_priority: bool,
    export_assignment_priority: bool,
}

impl ExportCandidate {
    fn new(
        name: String,
        start: u32,
        end: u32,
        complexity: u32,
        order: usize,
        function_priority: bool,
    ) -> Self {
        Self {
            name,
            start,
            end,
            complexity,
            priority_group: 0,
            order,
            function_priority,
            export_assignment_priority: false,
        }
    }

    fn with_priority_group(mut self, priority_group: usize) -> Self {
        self.priority_group = priority_group;
        self
    }

    fn with_export_assignment_priority(mut self) -> Self {
        self.export_assignment_priority = true;
        self
    }
}

#[derive(Clone, Debug)]
struct BlockCandidate {
    anchor: String,
    start: u32,
    end: u32,
    complexity: u32,
    order: usize,
}

impl BlockCandidate {
    fn new(anchor: String, start: u32, end: u32, complexity: u32, order: usize) -> Self {
        Self {
            anchor,
            start,
            end,
            complexity,
            order,
        }
    }
}

#[derive(Clone, Debug)]
struct NamespaceAliasSpec {
    local_name: String,
    export_name: String,
    local_declarations: HashMap<String, ExportCandidate>,
    order: usize,
}

#[derive(Clone, Copy, Debug)]
struct TsxMalformedJsxArrowRecovery {
    token_limit: usize,
    eq_index: usize,
    end_line: u32,
}

#[derive(Clone, Debug)]
struct Token {
    text: String,
    line: u32,
    start: usize,
    end: usize,
}

#[derive(Clone, Debug)]
struct Tokenization {
    tokens: Vec<Token>,
    diagnostics: Vec<AnchorDiagnostic>,
}

struct RustLineMap<'a> {
    lines: Vec<&'a str>,
}

impl<'a> RustLineMap<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            lines: source.lines().collect(),
        }
    }

    fn leading_item_start_line(&self, item_line: u32) -> u32 {
        let Some(mut first_line) = usize::try_from(item_line)
            .ok()
            .and_then(|line| line.checked_sub(1))
        else {
            return item_line;
        };
        let mut cursor = first_line.checked_sub(1);
        while let Some(line_index) = cursor {
            let trimmed = self.lines.get(line_index).copied().unwrap_or("").trim();
            if trimmed.is_empty() {
                break;
            }
            if trimmed.starts_with("///") {
                first_line = line_index;
                cursor = line_index.checked_sub(1);
                continue;
            }
            if let Some(start_line) = self.block_doc_start_line(line_index) {
                first_line = start_line;
                cursor = start_line.checked_sub(1);
                continue;
            }
            if let Some(start_line) = self.attribute_start_line(line_index) {
                first_line = start_line;
                cursor = start_line.checked_sub(1);
                continue;
            }
            break;
        }
        u32::try_from(first_line + 1).unwrap_or(item_line)
    }

    fn block_doc_start_line(&self, end_line: usize) -> Option<usize> {
        let trimmed = self.lines.get(end_line)?.trim();
        if !trimmed.contains("*/") && !trimmed.starts_with("/**") {
            return None;
        }
        let mut cursor = end_line;
        loop {
            let line = self.lines.get(cursor)?.trim();
            if line.is_empty() {
                return None;
            }
            if line.starts_with("/**") && !line.starts_with("/*!") {
                return Some(cursor);
            }
            cursor = cursor.checked_sub(1)?;
        }
    }

    fn attribute_start_line(&self, end_line: usize) -> Option<usize> {
        let mut cursor = end_line;
        loop {
            let line = self.lines.get(cursor)?.trim();
            if line.is_empty() || line.contains('{') || line.contains('}') || line.ends_with(';') {
                return None;
            }
            if line.starts_with("#[") {
                return Some(cursor);
            }
            cursor = cursor.checked_sub(1)?;
        }
    }
}

pub fn extract_anchors(file: &RelativePath, source: &str) -> AnchorExtraction {
    if is_rust_file(file) {
        return extract_rust_anchors(source);
    }
    if is_c_family_file(file) {
        return extract_c_family_anchors(source);
    }

    let tokenization = tokenize_with_diagnostics(source);
    let all_tokens = tokenization.tokens;
    let all_parens = matching_tokens(&all_tokens, "(", ")");
    let mut diagnostics = tokenization.diagnostics;
    let mut malformed_jsx_arrow = None;
    let token_limit = if is_tsx_file(file) {
        if let Some(limit) = tsx_ambiguous_generic_arrow_limit(&all_tokens) {
            limit
        } else if let Some(recovery) =
            tsx_unclosed_jsx_arrow_recovery(source, &all_tokens, &all_parens)
        {
            push_token_diagnostic(
                source,
                &all_tokens[recovery.eq_index],
                "incomplete TSX JSX initializer prevented complete anchor extraction",
                &mut diagnostics,
            );
            malformed_jsx_arrow = Some((recovery.eq_index, recovery.end_line));
            recovery.token_limit
        } else {
            if let Some(eq_index) = tsx_mismatched_jsx_arrow_diagnostic(&all_tokens, &all_parens) {
                push_token_diagnostic(
                    source,
                    &all_tokens[eq_index],
                    "mismatched TSX JSX initializer prevented complete anchor extraction",
                    &mut diagnostics,
                );
            }
            all_tokens.len()
        }
    } else {
        all_tokens.len()
    };
    let tokens = &all_tokens[..token_limit];
    let parens = matching_tokens(tokens, "(", ")");
    let braces = matching_tokens(tokens, "{", "}");
    let brackets = matching_tokens(tokens, "[", "]");
    collect_delimiter_diagnostics(source, tokens, &parens, "(", ")", &mut diagnostics);
    collect_delimiter_diagnostics(source, tokens, &braces, "{", "}", &mut diagnostics);
    collect_delimiter_diagnostics(source, tokens, &brackets, "[", "]", &mut diagnostics);
    let mut builder = AnchorBuilder::new();
    let mut exports = Vec::new();
    let mut declarations = HashMap::new();

    collect_function_anchors(
        source,
        tokens,
        &parens,
        &braces,
        &mut builder,
        &mut exports,
        &mut declarations,
    );
    collect_class_anchors(
        source,
        tokens,
        &parens,
        &braces,
        &mut builder,
        &mut exports,
        &mut declarations,
    );
    collect_default_export_anchors(tokens, &parens, &braces, &mut exports);
    collect_variable_anchors(
        source,
        tokens,
        &parens,
        &braces,
        malformed_jsx_arrow,
        &mut builder,
        &mut exports,
        &mut declarations,
    );
    collect_type_like_export_anchors(tokens, &braces, &mut exports, &mut declarations);
    collect_import_equals_declarations(tokens, &mut declarations);
    collect_export_alias_anchors(tokens, &declarations, &mut exports);
    collect_export_import_anchors(tokens, &mut exports);
    collect_export_assignment_namespace_anchors(source, tokens, &parens, &braces, &mut exports);
    collect_export_anchors(&mut builder, exports);
    let mut blocks = Vec::new();
    collect_if_block_candidates(source, tokens, &parens, &braces, 0, 0, &mut blocks);
    collect_template_if_block_candidates(source, &mut blocks);
    collect_block_anchors(&mut builder, blocks);

    AnchorExtraction {
        anchors: builder.anchors,
        complete: diagnostics.is_empty(),
        diagnostics,
    }
}

pub fn anchor_exists(file: &RelativePath, source: &str, anchor: &str) -> bool {
    extract_anchors(file, source)
        .anchors
        .get_str(anchor)
        .is_some()
}

pub fn assert_anchor_exists(file: &RelativePath, source: &str, anchor: &str) -> Result<()> {
    let extraction = extract_anchors(file, source);
    if extraction.anchors.get_str(anchor).is_some() {
        return Ok(());
    }
    let mut available = extraction
        .anchors
        .iter()
        .map(|(anchor, _)| anchor.as_str().to_string())
        .collect::<Vec<_>>();
    available.sort();
    let suggestion = if available.is_empty() {
        format!(" No anchors were found in {}.", file.as_str())
    } else {
        format!(
            " Available anchors in {}: {}.",
            file.as_str(),
            available.join(", ")
        )
    };
    Err(ArchivaError::cli(format!(
        "Anchor \"{}\" does not exist in {}. A decision recorded against a missing anchor is an immediate orphan.{}",
        anchor,
        file.as_str(),
        suggestion
    )))
}

struct AnchorBuilder {
    anchors: OrderedMap<String, AnchorInfo>,
    counts: HashMap<String, u32>,
}

impl AnchorBuilder {
    fn new() -> Self {
        Self {
            anchors: OrderedMap::new(),
            counts: HashMap::new(),
        }
    }

    fn add(&mut self, base: String, start: u32, end: u32, complexity: u32, kind: AnchorKind) {
        let seen = *self.counts.get(&base).unwrap_or(&0);
        self.counts.insert(base.clone(), seen + 1);
        let anchor = if seen == 0 {
            base
        } else {
            format!("{}#{}", base, seen + 1)
        };
        self.anchors.insert(
            anchor.clone(),
            AnchorInfo {
                anchor,
                start,
                end,
                complexity,
                kind,
            },
        );
    }
}

fn extract_rust_anchors(source: &str) -> AnchorExtraction {
    let tokenization = rust_tokenize_with_diagnostics(source);
    let tokens = tokenization.tokens;
    let mut diagnostics = tokenization.diagnostics;
    let braces = matching_tokens(&tokens, "{", "}");
    let parens = matching_tokens(&tokens, "(", ")");
    let brackets = matching_tokens(&tokens, "[", "]");
    collect_delimiter_diagnostics(source, &tokens, &parens, "(", ")", &mut diagnostics);
    collect_delimiter_diagnostics(source, &tokens, &braces, "{", "}", &mut diagnostics);
    collect_delimiter_diagnostics(source, &tokens, &brackets, "[", "]", &mut diagnostics);
    let line_map = RustLineMap::new(source);
    let mut builder = AnchorBuilder::new();
    let mut exports = Vec::new();
    let mut blocks = Vec::new();

    collect_rust_item_anchors(
        source,
        &line_map,
        &tokens,
        &braces,
        0,
        tokens.len(),
        "",
        &mut builder,
        &mut exports,
        &mut blocks,
        true,
    );
    collect_export_anchors(&mut builder, exports);
    collect_block_anchors(&mut builder, blocks);

    AnchorExtraction {
        anchors: builder.anchors,
        complete: diagnostics.is_empty(),
        diagnostics,
    }
}

#[derive(Clone, Debug)]
struct CFamilyTypeScope {
    name: String,
    kind: AnchorKind,
    open: usize,
    close: usize,
}

fn extract_c_family_anchors(source: &str) -> AnchorExtraction {
    let tokenization = tokenize_with_diagnostics(source);
    let tokens = tokenization.tokens;
    let mut diagnostics = tokenization.diagnostics;
    let parens = matching_tokens(&tokens, "(", ")");
    let braces = matching_tokens(&tokens, "{", "}");
    let brackets = matching_tokens(&tokens, "[", "]");
    collect_delimiter_diagnostics(source, &tokens, &parens, "(", ")", &mut diagnostics);
    collect_delimiter_diagnostics(source, &tokens, &braces, "{", "}", &mut diagnostics);
    collect_delimiter_diagnostics(source, &tokens, &brackets, "[", "]", &mut diagnostics);

    let mut builder = AnchorBuilder::new();
    let type_scopes = collect_c_family_type_anchors(&tokens, &braces, &mut builder);
    collect_c_family_function_anchors(&tokens, &parens, &braces, &type_scopes, &mut builder);

    let mut blocks = Vec::new();
    collect_if_block_candidates(source, &tokens, &parens, &braces, 0, 0, &mut blocks);
    collect_block_anchors(&mut builder, blocks);

    AnchorExtraction {
        anchors: builder.anchors,
        complete: diagnostics.is_empty(),
        diagnostics,
    }
}

fn collect_c_family_type_anchors(
    tokens: &[Token],
    braces: &[Option<usize>],
    builder: &mut AnchorBuilder,
) -> Vec<CFamilyTypeScope> {
    let mut scopes = Vec::new();
    for index in 0..tokens.len() {
        let (kind, mut name_index) = match tokens[index].text.as_str() {
            "class" => (AnchorKind::Class, index + 1),
            "struct" => (AnchorKind::Struct, index + 1),
            "enum" => {
                let next = tokens.get(index + 1).map(|token| token.text.as_str());
                let name_index = if matches!(next, Some("class" | "struct")) {
                    index + 2
                } else {
                    index + 1
                };
                (AnchorKind::Enum, name_index)
            }
            _ => continue,
        };
        if tokens
            .get(index.checked_sub(1).unwrap_or(index))
            .is_some_and(|token| token.text == ".")
        {
            continue;
        }
        let Some(open) = find_next_until(tokens, index + 1, "{", &[";", "=", ")", ","]) else {
            continue;
        };
        let Some(close) = braces.get(open).and_then(|match_index| *match_index) else {
            continue;
        };
        if !tokens
            .get(name_index)
            .is_some_and(|token| is_identifier(&token.text))
        {
            name_index = close + 1;
        }
        let Some(name_token) = tokens.get(name_index) else {
            continue;
        };
        if !is_identifier(&name_token.text) || c_family_keyword(&name_token.text) {
            continue;
        }
        let prefix = match kind {
            AnchorKind::Class => "class",
            AnchorKind::Struct => "struct",
            AnchorKind::Enum => "enum",
            _ => continue,
        };
        builder.add(
            format!("{prefix}:{}", c_family_anchor_name(&name_token.text)),
            tokens[index].line,
            tokens[close].line,
            1,
            kind,
        );
        scopes.push(CFamilyTypeScope {
            name: c_family_anchor_name(&name_token.text),
            kind,
            open,
            close,
        });
    }
    scopes
}

fn collect_c_family_function_anchors(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    type_scopes: &[CFamilyTypeScope],
    builder: &mut AnchorBuilder,
) {
    let mut function_ranges = Vec::<(usize, usize)>::new();
    for body_open in 0..tokens.len() {
        if tokens[body_open].text != "{" {
            continue;
        }
        let Some(body_close) = braces.get(body_open).and_then(|match_index| *match_index) else {
            continue;
        };
        if type_scopes.iter().any(|scope| scope.open == body_open) {
            continue;
        }
        if function_ranges
            .iter()
            .any(|(open, close)| *open < body_open && body_open < *close)
        {
            continue;
        }
        let Some((name, name_index, kind)) =
            c_family_function_anchor(tokens, parens, body_open, type_scopes)
        else {
            continue;
        };
        builder.add(
            format!("fn:{name}"),
            tokens[name_index].line,
            tokens[body_close].line,
            complexity_between(tokens, name_index, body_close, None),
            kind,
        );
        function_ranges.push((body_open, body_close));
    }
}

fn c_family_function_anchor(
    tokens: &[Token],
    parens: &[Option<usize>],
    body_open: usize,
    type_scopes: &[CFamilyTypeScope],
) -> Option<(String, usize, AnchorKind)> {
    let param_close = c_family_parameter_close_before_body(tokens, body_open)?;
    let param_open = parens
        .get(param_close)
        .and_then(|match_index| *match_index)?;
    let (mut name, name_index) = c_family_qualified_name_before(tokens, param_open)?;
    if c_family_control_keyword(&tokens[name_index].text) {
        return None;
    }
    let mut kind = if name.contains('.') {
        AnchorKind::Method
    } else {
        AnchorKind::Function
    };
    if !name.contains('.') {
        if let Some(scope) = type_scopes
            .iter()
            .filter(|scope| {
                matches!(scope.kind, AnchorKind::Class | AnchorKind::Struct)
                    && scope.open < body_open
                    && body_open < scope.close
            })
            .min_by_key(|scope| scope.close - scope.open)
        {
            name = format!("{}.{}", scope.name, name);
            kind = AnchorKind::Method;
        }
    }
    Some((name, name_index, kind))
}

fn c_family_parameter_close_before_body(tokens: &[Token], body_open: usize) -> Option<usize> {
    let mut index = body_open.checked_sub(1)?;
    loop {
        match tokens[index].text.as_str() {
            ")" => return Some(index),
            "const" | "volatile" | "noexcept" | "override" | "final" | "&" | "&&" => {}
            _ => return None,
        }
        index = index.checked_sub(1)?;
    }
}

fn c_family_qualified_name_before(
    tokens: &[Token],
    before_index: usize,
) -> Option<(String, usize)> {
    let mut name_index = before_index.checked_sub(1)?;
    while tokens
        .get(name_index)
        .is_some_and(|token| matches!(token.text.as_str(), "*" | "&" | "&&"))
    {
        name_index = name_index.checked_sub(1)?;
    }
    let name_token = tokens.get(name_index)?;
    if !is_identifier(&name_token.text) || c_family_keyword(&name_token.text) {
        return None;
    }
    let mut parts = vec![c_family_anchor_name(&name_token.text)];
    let mut start_index = name_index;
    let mut cursor = name_index;
    while cursor >= 3
        && tokens[cursor - 1].text == ":"
        && tokens[cursor - 2].text == ":"
        && is_identifier(&tokens[cursor - 3].text)
        && !c_family_keyword(&tokens[cursor - 3].text)
    {
        cursor -= 3;
        start_index = cursor;
        parts.push(c_family_anchor_name(&tokens[cursor].text));
    }
    parts.reverse();
    Some((parts.join("."), start_index))
}

fn c_family_anchor_name(name: &str) -> String {
    name.to_string()
}

fn c_family_control_keyword(value: &str) -> bool {
    matches!(
        value,
        "if" | "for" | "while" | "switch" | "catch" | "sizeof" | "alignof" | "return"
    )
}

fn c_family_keyword(value: &str) -> bool {
    matches!(
        value,
        "auto"
            | "bool"
            | "break"
            | "case"
            | "catch"
            | "char"
            | "class"
            | "const"
            | "continue"
            | "default"
            | "delete"
            | "do"
            | "double"
            | "else"
            | "enum"
            | "explicit"
            | "extern"
            | "float"
            | "for"
            | "friend"
            | "goto"
            | "if"
            | "inline"
            | "int"
            | "long"
            | "namespace"
            | "new"
            | "operator"
            | "private"
            | "protected"
            | "public"
            | "register"
            | "return"
            | "short"
            | "signed"
            | "sizeof"
            | "static"
            | "struct"
            | "switch"
            | "template"
            | "typedef"
            | "union"
            | "unsigned"
            | "virtual"
            | "void"
            | "volatile"
            | "while"
    )
}

fn collect_rust_item_anchors(
    source: &str,
    line_map: &RustLineMap<'_>,
    tokens: &[Token],
    braces: &[Option<usize>],
    start: usize,
    end: usize,
    scope_prefix: &str,
    builder: &mut AnchorBuilder,
    exports: &mut Vec<ExportCandidate>,
    blocks: &mut Vec<BlockCandidate>,
    allow_exports: bool,
) {
    let mut index = start;
    while index < end {
        if !is_rust_direct_scope_member(tokens, start, index) {
            index += 1;
            continue;
        }
        let (item_index, visibility_index) = rust_item_dispatch_index(tokens, index, end);
        if item_index >= end || !is_rust_direct_scope_member(tokens, start, item_index) {
            index += 1;
            continue;
        }
        let item_start = visibility_index.unwrap_or(item_index);
        let is_public = allow_exports && visibility_index.is_some();
        match tokens[item_index].text.as_str() {
            "fn" => {
                if let Some(body_close) = collect_rust_function_anchor_at(
                    source,
                    line_map,
                    tokens,
                    braces,
                    item_index,
                    item_start,
                    is_public,
                    end,
                    scope_prefix,
                    builder,
                    exports,
                    blocks,
                ) {
                    index = body_close + 1;
                } else {
                    index += 1;
                }
            }
            "impl" => {
                if let Some(body_close) = collect_rust_impl_anchor_at(
                    source,
                    line_map,
                    tokens,
                    braces,
                    item_index,
                    end,
                    scope_prefix,
                    builder,
                    exports,
                    blocks,
                    allow_exports,
                ) {
                    index = body_close + 1;
                } else {
                    index += 1;
                }
            }
            "struct" | "enum" | "trait" | "mod" => {
                if let Some(item_end) = collect_rust_structural_anchor_at(
                    source,
                    line_map,
                    tokens,
                    braces,
                    item_index,
                    item_start,
                    is_public,
                    end,
                    scope_prefix,
                    builder,
                    exports,
                    blocks,
                    allow_exports,
                ) {
                    index = item_end + 1;
                } else {
                    index += 1;
                }
            }
            "use" if is_public => {
                if let Some(item_end) = collect_rust_use_export_at(
                    line_map,
                    tokens,
                    item_start,
                    item_index,
                    end,
                    scope_prefix,
                    exports,
                ) {
                    index = item_end + 1;
                } else {
                    index += 1;
                }
            }
            "type" | "const" | "static" if is_public => {
                if let Some(item_end) = collect_rust_named_export_at(
                    line_map,
                    tokens,
                    item_start,
                    item_index,
                    end,
                    scope_prefix,
                    exports,
                ) {
                    index = item_end + 1;
                } else {
                    index += 1;
                }
            }
            "{" => {
                if let Some(close) = braces
                    .get(item_index)
                    .and_then(|match_index| *match_index)
                    .filter(|close| *close < end)
                {
                    if rust_block_can_contain_local_items(tokens, start, item_index) {
                        collect_rust_item_anchors(
                            source,
                            line_map,
                            tokens,
                            braces,
                            item_index + 1,
                            close,
                            scope_prefix,
                            builder,
                            exports,
                            blocks,
                            false,
                        );
                    }
                    index = close + 1;
                } else {
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
}

fn collect_rust_function_anchor_at(
    source: &str,
    line_map: &RustLineMap<'_>,
    tokens: &[Token],
    braces: &[Option<usize>],
    index: usize,
    item_start: usize,
    is_public: bool,
    scope_end: usize,
    scope_prefix: &str,
    builder: &mut AnchorBuilder,
    exports: &mut Vec<ExportCandidate>,
    blocks: &mut Vec<BlockCandidate>,
) -> Option<usize> {
    let name_index = tokens
        .get(index + 1)
        .filter(|token| is_rust_identifier(&token.text))
        .map(|_| index + 1)?;
    let body_open = rust_body_open_until(tokens, name_index + 1, scope_end)?;
    let body_close = braces
        .get(body_open)
        .and_then(|match_index| *match_index)
        .filter(|close| *close < scope_end)?;
    let scoped_name = format!(
        "{}{}",
        scope_prefix,
        rust_anchor_name(&tokens[name_index].text)
    );
    let complexity = rust_complexity_between(tokens, braces, body_open + 1, body_close);
    let start_line = line_map.leading_item_start_line(tokens[item_start].line);
    let local_prefix = format!("{scoped_name}.");
    builder.add(
        format!("fn:{scoped_name}"),
        start_line,
        tokens[body_close].line,
        complexity,
        AnchorKind::Function,
    );
    if is_public {
        exports.push(ExportCandidate::new(
            scoped_name.clone(),
            start_line,
            tokens[body_close].line,
            complexity,
            tokens[item_start].start,
            false,
        ));
    }
    collect_rust_item_anchors(
        source,
        line_map,
        tokens,
        braces,
        body_open + 1,
        body_close,
        &local_prefix,
        builder,
        exports,
        blocks,
        false,
    );
    collect_rust_if_block_candidates(source, tokens, braces, body_open + 1, body_close, blocks);
    Some(body_close)
}

fn collect_rust_structural_anchor_at(
    source: &str,
    line_map: &RustLineMap<'_>,
    tokens: &[Token],
    braces: &[Option<usize>],
    index: usize,
    item_start: usize,
    is_public: bool,
    scope_end: usize,
    scope_prefix: &str,
    builder: &mut AnchorBuilder,
    exports: &mut Vec<ExportCandidate>,
    blocks: &mut Vec<BlockCandidate>,
    allow_exports: bool,
) -> Option<usize> {
    let (prefix, kind) = match tokens[index].text.as_str() {
        "struct" => ("struct", AnchorKind::Struct),
        "enum" => ("enum", AnchorKind::Enum),
        "trait" => ("trait", AnchorKind::Trait),
        "mod" => ("mod", AnchorKind::Module),
        _ => return None,
    };
    let name_index = tokens
        .get(index + 1)
        .filter(|token| is_rust_identifier(&token.text))
        .map(|_| index + 1)?;
    let item_end = rust_item_end_until(tokens, braces, name_index + 1, scope_end)?;
    let name = rust_anchor_name(&tokens[name_index].text);
    let scoped_name = format!("{scope_prefix}{name}");
    let start_line = line_map.leading_item_start_line(tokens[item_start].line);
    builder.add(
        format!("{prefix}:{scoped_name}"),
        start_line,
        tokens[item_end].line,
        1,
        kind,
    );
    if is_public {
        exports.push(ExportCandidate::new(
            scoped_name.clone(),
            start_line,
            tokens[item_end].line,
            1,
            tokens[item_start].start,
            false,
        ));
    }
    if tokens[item_end].text == "}" {
        let body_open = braces.get(item_end).and_then(|match_index| *match_index)?;
        if kind == AnchorKind::Trait {
            collect_rust_trait_method_anchors(
                source,
                line_map,
                tokens,
                braces,
                body_open,
                item_end,
                &scoped_name,
                builder,
                exports,
                blocks,
            );
        } else if kind == AnchorKind::Module {
            let nested_prefix = format!("{scoped_name}.");
            collect_rust_item_anchors(
                source,
                line_map,
                tokens,
                braces,
                body_open + 1,
                item_end,
                &nested_prefix,
                builder,
                exports,
                blocks,
                allow_exports,
            );
        }
    }
    Some(item_end)
}

fn collect_rust_impl_anchor_at(
    source: &str,
    line_map: &RustLineMap<'_>,
    tokens: &[Token],
    braces: &[Option<usize>],
    index: usize,
    scope_end: usize,
    scope_prefix: &str,
    builder: &mut AnchorBuilder,
    exports: &mut Vec<ExportCandidate>,
    blocks: &mut Vec<BlockCandidate>,
    allow_exports: bool,
) -> Option<usize> {
    let body_open = rust_body_open_until(tokens, index + 1, scope_end)?;
    let body_close = braces
        .get(body_open)
        .and_then(|match_index| *match_index)
        .filter(|close| *close < scope_end)?;
    let subject = rust_impl_subject(tokens, index, body_open)?;
    let scoped_subject = RustImplSubject {
        impl_name: format!("{scope_prefix}{}", subject.impl_name),
        method_prefix: format!("{scope_prefix}{}", subject.method_prefix),
    };
    let start_line = line_map.leading_item_start_line(tokens[index].line);
    builder.add(
        format!("impl:{}", scoped_subject.impl_name),
        start_line,
        tokens[body_close].line,
        1,
        AnchorKind::Impl,
    );
    collect_rust_impl_method_anchors(
        source,
        line_map,
        tokens,
        braces,
        body_open,
        body_close,
        &scoped_subject,
        builder,
        exports,
        blocks,
        allow_exports,
    );
    Some(body_close)
}

fn collect_rust_impl_method_anchors(
    source: &str,
    line_map: &RustLineMap<'_>,
    tokens: &[Token],
    braces: &[Option<usize>],
    body_open: usize,
    body_close: usize,
    subject: &RustImplSubject,
    builder: &mut AnchorBuilder,
    exports: &mut Vec<ExportCandidate>,
    blocks: &mut Vec<BlockCandidate>,
    allow_exports: bool,
) {
    let mut index = body_open + 1;
    while index < body_close {
        if !is_rust_direct_impl_member(tokens, body_open, index) {
            index += 1;
            continue;
        }
        let (item_index, visibility_index) = rust_item_dispatch_index(tokens, index, body_close);
        if item_index >= body_close
            || tokens[item_index].text != "fn"
            || !is_rust_direct_impl_member(tokens, body_open, item_index)
        {
            index += 1;
            continue;
        }
        let item_start = visibility_index.unwrap_or(item_index);
        let is_public = allow_exports && visibility_index.is_some();
        let Some(name_index) = tokens
            .get(item_index + 1)
            .filter(|token| is_rust_identifier(&token.text))
            .map(|_| item_index + 1)
        else {
            index += 1;
            continue;
        };
        let Some(method_body_open) = rust_body_open_until(tokens, name_index + 1, body_close)
        else {
            index += 1;
            continue;
        };
        let Some(method_body_close) = braces
            .get(method_body_open)
            .and_then(|match_index| *match_index)
            .filter(|close| *close <= body_close)
        else {
            index += 1;
            continue;
        };
        let method_name = format!(
            "{}.{}",
            subject.method_prefix,
            rust_anchor_name(&tokens[name_index].text)
        );
        let start_line = line_map.leading_item_start_line(tokens[item_start].line);
        let complexity =
            rust_complexity_between(tokens, braces, method_body_open + 1, method_body_close);
        builder.add(
            format!("fn:{method_name}"),
            start_line,
            tokens[method_body_close].line,
            complexity,
            AnchorKind::Method,
        );
        if is_public {
            exports.push(ExportCandidate::new(
                method_name.clone(),
                start_line,
                tokens[method_body_close].line,
                complexity,
                tokens[item_start].start,
                false,
            ));
        }
        let local_prefix = format!("{method_name}.");
        collect_rust_item_anchors(
            source,
            line_map,
            tokens,
            braces,
            method_body_open + 1,
            method_body_close,
            &local_prefix,
            builder,
            exports,
            blocks,
            false,
        );
        collect_rust_if_block_candidates(
            source,
            tokens,
            braces,
            method_body_open + 1,
            method_body_close,
            blocks,
        );
        index = method_body_close + 1;
    }
}

fn collect_rust_trait_method_anchors(
    source: &str,
    line_map: &RustLineMap<'_>,
    tokens: &[Token],
    braces: &[Option<usize>],
    body_open: usize,
    body_close: usize,
    trait_name: &str,
    builder: &mut AnchorBuilder,
    exports: &mut Vec<ExportCandidate>,
    blocks: &mut Vec<BlockCandidate>,
) {
    let mut index = body_open + 1;
    while index < body_close {
        if tokens[index].text != "fn" || !is_rust_direct_item_member(tokens, body_open, index) {
            index += 1;
            continue;
        }
        let Some(name_index) = tokens
            .get(index + 1)
            .filter(|token| is_rust_identifier(&token.text))
            .map(|_| index + 1)
        else {
            index += 1;
            continue;
        };
        let Some((method_end, has_body)) =
            rust_trait_method_end(tokens, braces, name_index + 1, body_close)
        else {
            index += 1;
            continue;
        };
        let method_name = format!(
            "{}.{}",
            trait_name,
            rust_anchor_name(&tokens[name_index].text)
        );
        let method_body_open = if has_body {
            rust_body_open_until(tokens, name_index + 1, method_end)
        } else {
            None
        };
        let complexity = method_body_open
            .map(|body_open| rust_complexity_between(tokens, braces, body_open + 1, method_end))
            .unwrap_or(1);
        let start_line = line_map.leading_item_start_line(tokens[index].line);
        builder.add(
            format!("fn:{method_name}"),
            start_line,
            tokens[method_end].line,
            complexity,
            AnchorKind::Method,
        );
        if let Some(method_body_open) = method_body_open {
            let local_prefix = format!("{method_name}.");
            collect_rust_item_anchors(
                source,
                line_map,
                tokens,
                braces,
                method_body_open + 1,
                method_end,
                &local_prefix,
                builder,
                exports,
                blocks,
                false,
            );
            collect_rust_if_block_candidates(
                source,
                tokens,
                braces,
                method_body_open + 1,
                method_end,
                blocks,
            );
        }
        index = method_end + 1;
    }
}

fn collect_rust_if_block_candidates(
    source: &str,
    tokens: &[Token],
    braces: &[Option<usize>],
    start: usize,
    end: usize,
    blocks: &mut Vec<BlockCandidate>,
) {
    let mut index = start;
    while index <= end && index < tokens.len() {
        if let Some(skip_end) = rust_complexity_skip_end(tokens, braces, start, end, index) {
            index = skip_end + 1;
            continue;
        }
        if tokens[index].text != "if" {
            index += 1;
            continue;
        }
        let Some(body_open) = rust_if_body_open_until(tokens, index + 1, end) else {
            index += 1;
            continue;
        };
        let Some(body_close) = braces
            .get(body_open)
            .and_then(|match_index| *match_index)
            .filter(|close| *close <= end)
        else {
            index += 1;
            continue;
        };
        let condition = source
            .get(tokens[index].end..tokens[body_open].start)
            .unwrap_or("");
        let normalized = normalize_if_condition(condition);
        if !normalized.is_empty() {
            let complexity = rust_complexity_between(tokens, braces, index, body_close);
            if complexity >= 3 {
                blocks.push(BlockCandidate::new(
                    format!("block:if_{normalized}"),
                    tokens[index].line,
                    tokens[body_close].line,
                    complexity,
                    tokens[index].start,
                ));
            }
        }
        index = body_close + 1;
    }
}

fn collect_rust_use_export_at(
    line_map: &RustLineMap<'_>,
    tokens: &[Token],
    item_start: usize,
    use_index: usize,
    scope_end: usize,
    scope_prefix: &str,
    exports: &mut Vec<ExportCandidate>,
) -> Option<usize> {
    let item_end = rust_semicolon_end_until(tokens, use_index + 1, scope_end)?;
    let mut names = Vec::new();
    rust_collect_use_export_names(tokens, use_index + 1, item_end, &mut names);
    let start_line = line_map.leading_item_start_line(tokens[item_start].line);
    for name in names {
        exports.push(ExportCandidate::new(
            format!("{scope_prefix}{name}"),
            start_line,
            tokens[item_end].line,
            1,
            tokens[item_start].start,
            false,
        ));
    }
    Some(item_end)
}

fn collect_rust_named_export_at(
    line_map: &RustLineMap<'_>,
    tokens: &[Token],
    item_start: usize,
    item_index: usize,
    scope_end: usize,
    scope_prefix: &str,
    exports: &mut Vec<ExportCandidate>,
) -> Option<usize> {
    let name_index = rust_named_export_name_index(tokens, item_index, scope_end)?;
    let item_end = rust_semicolon_end_until(tokens, name_index + 1, scope_end)?;
    let start_line = line_map.leading_item_start_line(tokens[item_start].line);
    exports.push(ExportCandidate::new(
        format!(
            "{}{}",
            scope_prefix,
            rust_anchor_name(&tokens[name_index].text)
        ),
        start_line,
        tokens[item_end].line,
        1,
        tokens[item_start].start,
        false,
    ));
    Some(item_end)
}

fn rust_collect_use_export_names(
    tokens: &[Token],
    start: usize,
    end: usize,
    names: &mut Vec<String>,
) {
    let mut cursor = start;
    while cursor < end {
        if tokens[cursor].text == "as"
            && tokens
                .get(cursor + 1)
                .is_some_and(|token| is_rust_identifier(&token.text))
        {
            names.push(rust_anchor_name(&tokens[cursor + 1].text));
            cursor += 2;
            continue;
        }
        if is_rust_identifier(&tokens[cursor].text)
            && !rust_use_non_export_identifier(&tokens[cursor].text)
            && tokens
                .get(cursor + 1)
                .is_none_or(|token| matches!(token.text.as_str(), "," | "}"))
            && tokens
                .get(cursor.wrapping_sub(1))
                .is_none_or(|token| token.text != "as")
        {
            names.push(rust_anchor_name(&tokens[cursor].text));
        }
        cursor += 1;
    }
}

fn rust_use_non_export_identifier(value: &str) -> bool {
    matches!(value, "crate" | "super" | "self" | "Self")
}

fn rust_named_export_name_index(
    tokens: &[Token],
    item_index: usize,
    limit: usize,
) -> Option<usize> {
    let mut cursor = item_index + 1;
    if tokens[item_index].text == "static"
        && tokens.get(cursor).is_some_and(|token| token.text == "mut")
    {
        cursor += 1;
    }
    tokens
        .get(cursor)
        .filter(|token| cursor < limit && is_rust_identifier(&token.text))
        .map(|_| cursor)
}

fn rust_item_dispatch_index(
    tokens: &[Token],
    index: usize,
    limit: usize,
) -> (usize, Option<usize>) {
    let mut cursor = index;
    let mut visibility_index = None;
    if tokens.get(cursor).is_some_and(|token| token.text == "pub") {
        visibility_index = Some(cursor);
        cursor = rust_visibility_end(tokens, cursor, limit);
    }
    loop {
        if cursor >= limit {
            return (cursor, visibility_index);
        }
        match tokens[cursor].text.as_str() {
            "async" | "unsafe" | "default" => cursor += 1,
            "const"
                if tokens.get(cursor + 1).is_some_and(|token| {
                    matches!(token.text.as_str(), "fn" | "unsafe" | "extern")
                }) =>
            {
                cursor += 1;
            }
            "extern" => cursor += 1,
            _ => return (cursor, visibility_index),
        }
    }
}

fn rust_visibility_end(tokens: &[Token], pub_index: usize, limit: usize) -> usize {
    let mut cursor = pub_index + 1;
    if tokens.get(cursor).is_none_or(|token| token.text != "(") {
        return cursor;
    }
    let mut depth = 0_i32;
    while cursor < limit {
        match tokens[cursor].text.as_str() {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return cursor + 1;
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    pub_index + 1
}

#[derive(Clone, Debug)]
struct RustImplSubject {
    impl_name: String,
    method_prefix: String,
}

fn rust_impl_subject(
    tokens: &[Token],
    impl_index: usize,
    body_open: usize,
) -> Option<RustImplSubject> {
    let header_start = rust_skip_leading_impl_generics(tokens, impl_index + 1, body_open);
    let for_index = rust_header_keyword(tokens, header_start, body_open, "for");
    let (impl_name, method_prefix) = if let Some(for_index) = for_index {
        let trait_name = rust_first_path_terminal(tokens, header_start, for_index)?;
        let type_name = rust_first_path_terminal(tokens, for_index + 1, body_open)?;
        (
            format!("{type_name}.{trait_name}"),
            format!("{type_name}.{trait_name}"),
        )
    } else {
        let type_name = rust_first_path_terminal(tokens, header_start, body_open)?;
        (type_name.clone(), type_name)
    };
    Some(RustImplSubject {
        impl_name,
        method_prefix,
    })
}

fn rust_skip_leading_impl_generics(tokens: &[Token], mut index: usize, limit: usize) -> usize {
    while index < limit && matches!(tokens[index].text.as_str(), "unsafe" | "const") {
        index += 1;
    }
    if index < limit && tokens[index].text == "<" {
        index = rust_angle_group_end(tokens, index, limit)
            .map(|end| end + 1)
            .unwrap_or(index);
    }
    while index < limit && matches!(tokens[index].text.as_str(), "unsafe" | "const") {
        index += 1;
    }
    index
}

fn rust_header_keyword(
    tokens: &[Token],
    start: usize,
    limit: usize,
    keyword: &str,
) -> Option<usize> {
    let mut angle_depth = 0_i32;
    let mut paren_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    for index in start..limit {
        match tokens[index].text.as_str() {
            "<" => angle_depth += 1,
            ">" if angle_depth > 0 => angle_depth -= 1,
            "(" => paren_depth += 1,
            ")" if paren_depth > 0 => paren_depth -= 1,
            "[" => bracket_depth += 1,
            "]" if bracket_depth > 0 => bracket_depth -= 1,
            value
                if value == keyword
                    && angle_depth == 0
                    && paren_depth == 0
                    && bracket_depth == 0 =>
            {
                return Some(index);
            }
            _ => {}
        }
    }
    None
}

fn rust_first_path_terminal(tokens: &[Token], start: usize, limit: usize) -> Option<String> {
    let mut index = start;
    while index < limit {
        match tokens[index].text.as_str() {
            "<" => {
                index = rust_angle_group_end(tokens, index, limit)
                    .map(|end| end + 1)
                    .unwrap_or(index + 1);
            }
            "dyn" | "mut" | "ref" => index += 1,
            "where" => return None,
            value if is_rust_identifier(value) => {
                let mut terminal = rust_anchor_name(value);
                let mut cursor = index;
                while cursor + 2 < limit
                    && tokens[cursor + 1].text == "::"
                    && is_rust_identifier(&tokens[cursor + 2].text)
                {
                    terminal = rust_anchor_name(&tokens[cursor + 2].text);
                    cursor += 2;
                }
                return Some(terminal);
            }
            _ => index += 1,
        }
    }
    None
}

fn rust_angle_group_end(tokens: &[Token], start: usize, limit: usize) -> Option<usize> {
    let mut depth = 0_i32;
    for index in start..limit {
        match tokens[index].text.as_str() {
            "<" => depth += 1,
            ">" => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn rust_body_open_until(tokens: &[Token], start: usize, limit: usize) -> Option<usize> {
    let mut angle_depth = 0_i32;
    let mut paren_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    for index in start..limit {
        match tokens[index].text.as_str() {
            ";" if angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 => return None,
            "{" if angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 => {
                return Some(index);
            }
            "<" => angle_depth += 1,
            ">" if angle_depth > 0 => angle_depth -= 1,
            "(" => paren_depth += 1,
            ")" if paren_depth > 0 => paren_depth -= 1,
            "[" => bracket_depth += 1,
            "]" if bracket_depth > 0 => bracket_depth -= 1,
            _ => {}
        }
    }
    None
}

fn rust_if_body_open_until(tokens: &[Token], start: usize, limit: usize) -> Option<usize> {
    let mut angle_depth = 0_i32;
    let mut paren_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    for index in start..=limit.min(tokens.len().saturating_sub(1)) {
        match tokens[index].text.as_str() {
            "{" if angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 => {
                return Some(index);
            }
            ";" if angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 => return None,
            "<" => angle_depth += 1,
            ">" if angle_depth > 0 => angle_depth -= 1,
            "(" => paren_depth += 1,
            ")" if paren_depth > 0 => paren_depth -= 1,
            "[" => bracket_depth += 1,
            "]" if bracket_depth > 0 => bracket_depth -= 1,
            _ => {}
        }
    }
    None
}

fn rust_item_end_until(
    tokens: &[Token],
    braces: &[Option<usize>],
    start: usize,
    limit: usize,
) -> Option<usize> {
    let mut angle_depth = 0_i32;
    let mut paren_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    for index in start..limit {
        match tokens[index].text.as_str() {
            ";" if angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 => {
                return Some(index);
            }
            "{" if angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 => {
                return braces.get(index).and_then(|match_index| *match_index);
            }
            "<" => angle_depth += 1,
            ">" if angle_depth > 0 => angle_depth -= 1,
            "(" => paren_depth += 1,
            ")" if paren_depth > 0 => paren_depth -= 1,
            "[" => bracket_depth += 1,
            "]" if bracket_depth > 0 => bracket_depth -= 1,
            _ => {}
        }
    }
    None
}

fn rust_semicolon_end_until(tokens: &[Token], start: usize, limit: usize) -> Option<usize> {
    let mut angle_depth = 0_i32;
    let mut paren_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    let mut brace_depth = 0_i32;
    for index in start..limit {
        match tokens[index].text.as_str() {
            ";" if angle_depth == 0
                && paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0 =>
            {
                return Some(index);
            }
            "<" => angle_depth += 1,
            ">" if angle_depth > 0 => angle_depth -= 1,
            "(" => paren_depth += 1,
            ")" if paren_depth > 0 => paren_depth -= 1,
            "[" => bracket_depth += 1,
            "]" if bracket_depth > 0 => bracket_depth -= 1,
            "{" => brace_depth += 1,
            "}" if brace_depth > 0 => brace_depth -= 1,
            _ => {}
        }
    }
    None
}

fn rust_trait_method_end(
    tokens: &[Token],
    braces: &[Option<usize>],
    start: usize,
    limit: usize,
) -> Option<(usize, bool)> {
    let mut angle_depth = 0_i32;
    let mut paren_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    for index in start..limit {
        match tokens[index].text.as_str() {
            ";" if angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 => {
                return Some((index, false));
            }
            "{" if angle_depth == 0 && paren_depth == 0 && bracket_depth == 0 => {
                return braces
                    .get(index)
                    .and_then(|match_index| *match_index)
                    .filter(|close| *close <= limit)
                    .map(|close| (close, true));
            }
            "<" => angle_depth += 1,
            ">" if angle_depth > 0 => angle_depth -= 1,
            "(" => paren_depth += 1,
            ")" if paren_depth > 0 => paren_depth -= 1,
            "[" => bracket_depth += 1,
            "]" if bracket_depth > 0 => bracket_depth -= 1,
            _ => {}
        }
    }
    None
}

fn is_rust_direct_impl_member(tokens: &[Token], body_open: usize, index: usize) -> bool {
    is_rust_direct_item_member(tokens, body_open, index)
}

fn is_rust_direct_item_member(tokens: &[Token], body_open: usize, index: usize) -> bool {
    rust_depths_before(tokens, body_open + 1, index) == (0, 0, 0)
}

fn is_rust_direct_scope_member(tokens: &[Token], start: usize, index: usize) -> bool {
    rust_depths_before(tokens, start, index) == (0, 0, 0)
}

fn rust_block_can_contain_local_items(
    tokens: &[Token],
    scope_start: usize,
    open_index: usize,
) -> bool {
    !rust_block_header_contains(tokens, scope_start, open_index, "!")
        && !rust_block_header_contains(tokens, scope_start, open_index, "extern")
}

fn rust_block_header_contains(
    tokens: &[Token],
    scope_start: usize,
    open_index: usize,
    needle: &str,
) -> bool {
    let mut cursor = open_index;
    while cursor > scope_start {
        cursor -= 1;
        match tokens[cursor].text.as_str() {
            "{" | "}" | ";" => break,
            value if value == needle => return true,
            _ => {}
        }
    }
    false
}

fn rust_depths_before(tokens: &[Token], start: usize, end: usize) -> (i32, i32, i32) {
    let mut brace_depth = 0_i32;
    let mut paren_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    for token in &tokens[start..end] {
        match token.text.as_str() {
            "{" => brace_depth += 1,
            "}" => brace_depth -= 1,
            "(" => paren_depth += 1,
            ")" => paren_depth -= 1,
            "[" => bracket_depth += 1,
            "]" => bracket_depth -= 1,
            _ => {}
        }
    }
    (brace_depth, paren_depth, bracket_depth)
}

fn rust_complexity_between(
    tokens: &[Token],
    braces: &[Option<usize>],
    start: usize,
    end: usize,
) -> u32 {
    let mut complexity = 1;
    let mut index = start;
    while index <= end && index < tokens.len() {
        if let Some(skip_end) = rust_complexity_skip_end(tokens, braces, start, end, index) {
            index = skip_end + 1;
            continue;
        }
        match tokens[index].text.as_str() {
            "if" | "for" | "while" | "loop" | "&&" | "||" | "?" | "=>" => complexity += 1,
            _ => {}
        }
        index += 1;
    }
    complexity
}

fn rust_complexity_skip_end(
    tokens: &[Token],
    braces: &[Option<usize>],
    range_start: usize,
    end: usize,
    index: usize,
) -> Option<usize> {
    if let Some(item_end) = rust_complexity_item_end(tokens, braces, index, end) {
        return Some(item_end);
    }
    match tokens[index].text.as_str() {
        "{" if rust_macro_group_open(tokens, range_start, index)
            || rust_block_header_contains(tokens, range_start, index, "extern") =>
        {
            braces
                .get(index)
                .and_then(|match_index| *match_index)
                .filter(|close| *close <= end)
        }
        "(" | "[" if rust_macro_group_open(tokens, range_start, index) => {
            rust_delimited_group_end(tokens, index, end)
        }
        _ => None,
    }
}

fn rust_complexity_item_end(
    tokens: &[Token],
    braces: &[Option<usize>],
    index: usize,
    end: usize,
) -> Option<usize> {
    let limit = end.saturating_add(1).min(tokens.len());
    let (item_index, _) = rust_item_dispatch_index(tokens, index, limit);
    if item_index >= limit {
        return None;
    }
    match tokens[item_index].text.as_str() {
        "fn" => {
            let name_index = tokens
                .get(item_index + 1)
                .filter(|token| is_rust_identifier(&token.text))
                .map(|_| item_index + 1)?;
            rust_item_end_until(tokens, braces, name_index + 1, limit)
        }
        "impl" => {
            let body_open = rust_body_open_until(tokens, item_index + 1, limit)?;
            braces
                .get(body_open)
                .and_then(|match_index| *match_index)
                .filter(|close| *close < limit)
        }
        "struct" | "enum" | "trait" | "mod" => {
            let name_index = tokens
                .get(item_index + 1)
                .filter(|token| is_rust_identifier(&token.text))
                .map(|_| item_index + 1)?;
            rust_item_end_until(tokens, braces, name_index + 1, limit)
        }
        "use" => rust_semicolon_end_until(tokens, item_index + 1, limit),
        "type" | "const" | "static" => {
            let name_index = rust_named_export_name_index(tokens, item_index, limit)?;
            rust_semicolon_end_until(tokens, name_index + 1, limit)
        }
        _ => None,
    }
}

fn rust_macro_group_open(tokens: &[Token], range_start: usize, open_index: usize) -> bool {
    if open_index > range_start && tokens[open_index - 1].text == "!" {
        return true;
    }
    open_index >= range_start + 3
        && tokens[open_index - 1].text != ";"
        && tokens[open_index - 2].text == "!"
        && tokens[open_index - 3].text == "macro_rules"
}

fn rust_delimited_group_end(tokens: &[Token], open_index: usize, end: usize) -> Option<usize> {
    let close = match tokens.get(open_index)?.text.as_str() {
        "(" => ")",
        "[" => "]",
        "{" => "}",
        _ => return None,
    };
    let open = tokens[open_index].text.as_str();
    let mut depth = 0_i32;
    for index in open_index..=end.min(tokens.len().saturating_sub(1)) {
        match tokens[index].text.as_str() {
            value if value == open => depth += 1,
            value if value == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn rust_anchor_name(name: &str) -> String {
    name.strip_prefix("r#").unwrap_or(name).to_string()
}

fn is_rust_identifier(value: &str) -> bool {
    value
        .strip_prefix("r#")
        .map_or_else(|| is_identifier(value), is_identifier)
}

fn rust_tokenize_with_diagnostics(source: &str) -> Tokenization {
    let bytes = source.as_bytes();
    let mut tokens = Vec::<Token>::new();
    let mut diagnostics = Vec::<AnchorDiagnostic>::new();
    let mut index = 0;
    let mut line = 1_u32;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\n' {
            line += 1;
            index += 1;
            continue;
        }
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            let start = index;
            if !rust_skip_block_comment(bytes, &mut index, &mut line) {
                push_byte_diagnostic(
                    source,
                    start,
                    "unterminated block comment prevented complete anchor extraction",
                    &mut diagnostics,
                );
            }
            continue;
        }
        if rust_raw_string_opener_len(bytes, index).is_some() {
            let start = index;
            if let Some(next_index) = rust_raw_string_end(bytes, index) {
                line += count_newlines(&source[index..next_index]);
                index = next_index;
            } else {
                push_byte_diagnostic(
                    source,
                    start,
                    "unterminated raw string prevented complete anchor extraction",
                    &mut diagnostics,
                );
                line += count_newlines(&source[index..]);
                index = bytes.len();
            }
            continue;
        }
        if rust_starts_byte_string_or_char(bytes, index) {
            let start = index;
            if !rust_skip_quoted(bytes, &mut index, &mut line, 1) {
                push_byte_diagnostic(
                    source,
                    start,
                    "unterminated Rust byte string or byte char prevented complete anchor extraction",
                    &mut diagnostics,
                );
            }
            continue;
        }
        if byte == b'"' {
            let start = index;
            if !rust_skip_quoted(bytes, &mut index, &mut line, 0) {
                push_byte_diagnostic(
                    source,
                    start,
                    "unterminated Rust string prevented complete anchor extraction",
                    &mut diagnostics,
                );
            }
            continue;
        }
        if byte == b'\'' {
            let start = index;
            if rust_skip_lifetime_or_char(bytes, &mut index, &mut line) == Some(false) {
                push_byte_diagnostic(
                    source,
                    start,
                    "unterminated Rust char prevented complete anchor extraction",
                    &mut diagnostics,
                );
            }
            continue;
        }
        if let Some((text, end)) = rust_raw_identifier_token(source, index) {
            let start = index;
            index = end;
            tokens.push(Token {
                text,
                line,
                start,
                end: index,
            });
            continue;
        }
        if byte.is_ascii_digit() {
            let start = index;
            index = numeric_literal_end(bytes, index);
            tokens.push(Token {
                text: source[start..index].to_string(),
                line,
                start,
                end: index,
            });
            continue;
        }
        if let Some((text, end)) = identifier_token(source, index) {
            let start = index;
            index = end;
            tokens.push(Token {
                text,
                line,
                start,
                end: index,
            });
            continue;
        }
        let start = index;
        let text = match (byte, bytes.get(index + 1).copied()) {
            (b':', Some(b':')) => {
                index += 2;
                "::".to_string()
            }
            (b'=', Some(b'>')) => {
                index += 2;
                "=>".to_string()
            }
            (b'&', Some(b'&')) => {
                index += 2;
                "&&".to_string()
            }
            (b'|', Some(b'|')) => {
                index += 2;
                "||".to_string()
            }
            (b'-', Some(b'>')) => {
                index += 2;
                "->".to_string()
            }
            (b'>', Some(b'=')) => {
                index += 2;
                ">=".to_string()
            }
            (b'<', Some(b'=')) => {
                index += 2;
                "<=".to_string()
            }
            (b'=', Some(b'=')) => {
                index += 2;
                "==".to_string()
            }
            (b'!', Some(b'=')) => {
                index += 2;
                "!=".to_string()
            }
            _ => {
                let character = source[index..].chars().next().unwrap_or('\0');
                index += character.len_utf8();
                source[start..index].to_string()
            }
        };
        tokens.push(Token {
            text,
            line,
            start,
            end: index,
        });
    }

    Tokenization {
        tokens,
        diagnostics,
    }
}

fn rust_raw_identifier_token(source: &str, start: usize) -> Option<(String, usize)> {
    let bytes = source.as_bytes();
    if bytes.get(start) != Some(&b'r') || bytes.get(start + 1) != Some(&b'#') {
        return None;
    }
    let (name, end) = identifier_token(source, start + 2)?;
    Some((format!("r#{name}"), end))
}

fn rust_starts_byte_string_or_char(bytes: &[u8], index: usize) -> bool {
    bytes.get(index) == Some(&b'b') && matches!(bytes.get(index + 1), Some(b'"') | Some(b'\''))
}

fn rust_skip_quoted(bytes: &[u8], index: &mut usize, line: &mut u32, prefix_len: usize) -> bool {
    *index += prefix_len;
    let quote = bytes[*index];
    *index += 1;
    while *index < bytes.len() {
        if bytes[*index] == b'\n' {
            *line += 1;
        }
        if bytes[*index] == b'\\' {
            *index = (*index + 2).min(bytes.len());
            continue;
        }
        if bytes[*index] == quote {
            *index += 1;
            return true;
        }
        *index += 1;
    }
    false
}

fn rust_skip_lifetime_or_char(bytes: &[u8], index: &mut usize, line: &mut u32) -> Option<bool> {
    let start = *index;
    let lifetime_start = start + 1;
    if lifetime_start < bytes.len()
        && (bytes[lifetime_start].is_ascii_alphabetic() || bytes[lifetime_start] == b'_')
    {
        let mut lifetime_end = lifetime_start + 1;
        while lifetime_end < bytes.len()
            && (bytes[lifetime_end].is_ascii_alphanumeric() || bytes[lifetime_end] == b'_')
        {
            lifetime_end += 1;
        }
        if bytes.get(lifetime_end) != Some(&b'\'') {
            *index = lifetime_end;
            return None;
        }
    }
    Some(rust_skip_quoted(bytes, index, line, 0))
}

fn rust_skip_block_comment(bytes: &[u8], index: &mut usize, line: &mut u32) -> bool {
    let mut depth = 0_i32;
    while *index + 1 < bytes.len() {
        if bytes[*index] == b'\n' {
            *line += 1;
            *index += 1;
            continue;
        }
        if bytes[*index] == b'/' && bytes[*index + 1] == b'*' {
            depth += 1;
            *index += 2;
            continue;
        }
        if bytes[*index] == b'*' && bytes[*index + 1] == b'/' {
            depth -= 1;
            *index += 2;
            if depth == 0 {
                return true;
            }
            continue;
        }
        *index += 1;
    }
    *index = bytes.len();
    false
}

fn rust_raw_string_opener_len(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start;
    if bytes.get(index) == Some(&b'b') && bytes.get(index + 1) == Some(&b'r') {
        index += 2;
    } else if bytes.get(index) == Some(&b'r') {
        index += 1;
    } else {
        return None;
    }
    while bytes.get(index) == Some(&b'#') {
        index += 1;
    }
    if bytes.get(index) != Some(&b'"') {
        return None;
    }
    Some(index + 1 - start)
}

fn rust_raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start;
    if bytes.get(index) == Some(&b'b') && bytes.get(index + 1) == Some(&b'r') {
        index += 2;
    } else if bytes.get(index) == Some(&b'r') {
        index += 1;
    } else {
        return None;
    }
    let hash_start = index;
    while bytes.get(index) == Some(&b'#') {
        index += 1;
    }
    if bytes.get(index) != Some(&b'"') {
        return None;
    }
    let hash_count = index - hash_start;
    index += 1;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            let mut matched = true;
            for offset in 0..hash_count {
                if bytes.get(index + 1 + offset) != Some(&b'#') {
                    matched = false;
                    break;
                }
            }
            if matched {
                return Some(index + 1 + hash_count);
            }
        }
        index += 1;
    }
    Some(bytes.len())
}

fn collect_function_anchors(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    builder: &mut AnchorBuilder,
    exports: &mut Vec<ExportCandidate>,
    declarations: &mut HashMap<String, ExportCandidate>,
) {
    for index in 0..tokens.len() {
        if tokens[index].text != "function" || !is_top_level(tokens, index) {
            continue;
        }
        if is_function_expression_token(tokens, index) {
            continue;
        }
        let Some(name_index) = function_name_index(tokens, index) else {
            continue;
        };
        let Some(body_open) = function_body_open(tokens, name_index, parens) else {
            if is_declared_declaration(tokens, index) {
                let name = tokens[name_index].text.clone();
                let Some((start, end, complexity)) =
                    declaration_range(source, tokens, parens, braces, index, name_index)
                else {
                    record_export_function(
                        source, tokens, parens, braces, index, name_index, exports,
                    );
                    continue;
                };
                declarations.entry(name.clone()).or_insert_with(|| {
                    ExportCandidate::new(name.clone(), start, end, complexity, index, true)
                });
                builder.add(
                    format!("fn:{name}"),
                    start,
                    end,
                    complexity,
                    AnchorKind::Function,
                );
            }
            record_export_function(source, tokens, parens, braces, index, name_index, exports);
            continue;
        };
        let body_close = function_body_end_index(tokens, braces, body_open);
        let name = tokens[name_index].text.clone();
        let start_index =
            decorated_start_index(tokens, leading_export_index(tokens, index).unwrap_or(index));
        let start = tokens[start_index].line;
        let end = tokens[body_close].line;
        let complexity = complexity_between(tokens, index, body_close, None);
        declarations.entry(name.clone()).or_insert_with(|| {
            ExportCandidate::new(name.clone(), start, end, complexity, index, true)
        });
        builder.add(
            format!("fn:{name}"),
            start,
            end,
            complexity,
            AnchorKind::Function,
        );
        record_export_function(source, tokens, parens, braces, index, name_index, exports);
    }
}

fn is_function_expression_token(tokens: &[Token], function_index: usize) -> bool {
    let Some(previous_index) = function_index.checked_sub(1) else {
        return false;
    };
    let expression_marker_index = if tokens[previous_index].text == "async" {
        previous_index.checked_sub(1)
    } else {
        Some(previous_index)
    };
    expression_marker_index.is_some_and(|index| {
        matches!(
            tokens[index].text.as_str(),
            "=" | "(" | "[" | "," | ":" | "=>" | "return"
        )
    })
}

fn collect_class_anchors(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    builder: &mut AnchorBuilder,
    exports: &mut Vec<ExportCandidate>,
    declarations: &mut HashMap<String, ExportCandidate>,
) {
    for index in 0..tokens.len() {
        if tokens[index].text != "class" || !is_top_level(tokens, index) {
            continue;
        }
        if is_class_expression_token(tokens, index) {
            continue;
        }
        let name_index = class_name_index(tokens, index);
        if name_index.is_none() && !is_anonymous_default_class(tokens, index) {
            continue;
        }
        let body_search_start = name_index
            .map(|name_index| name_index + 1)
            .unwrap_or(index + 1);
        let Some(body_open) = find_next_text(tokens, body_search_start, "{") else {
            continue;
        };
        let Some(body_close) = braces.get(body_open).and_then(|match_index| *match_index) else {
            continue;
        };
        let start_index =
            decorated_start_index(tokens, leading_export_index(tokens, index).unwrap_or(index));
        let start = tokens[start_index].line;
        let malformed_recovery = malformed_class_member_recovery(
            source,
            tokens,
            parens,
            braces,
            body_open + 1,
            body_close,
        );
        let body_limit = malformed_recovery
            .as_ref()
            .map(|recovery| recovery.method_limit)
            .unwrap_or(body_close);
        let end_index = malformed_recovery
            .as_ref()
            .map(|recovery| recovery.end_index)
            .unwrap_or(body_close)
            .max(body_open);
        let end = tokens[end_index].line;
        let complexity = complexity_between(tokens, index, end_index, None);
        let class_name = name_index.map(|name_index| tokens[name_index].text.clone());
        if let Some(name) = &class_name {
            declarations.entry(name.clone()).or_insert_with(|| {
                ExportCandidate::new(name.clone(), start, end, complexity, index, false)
            });
            builder.add(
                format!("class:{name}"),
                start,
                end,
                complexity,
                AnchorKind::Class,
            );
        }
        record_export_class(
            tokens,
            index,
            class_name.as_deref(),
            start,
            end,
            complexity,
            exports,
        );
        collect_class_methods(
            source,
            tokens,
            parens,
            braces,
            body_open,
            body_limit,
            class_name.as_deref(),
            is_declared_declaration(tokens, index),
            builder,
        );
    }
}

fn is_class_expression_token(tokens: &[Token], class_index: usize) -> bool {
    class_index.checked_sub(1).is_some_and(|index| {
        matches!(
            tokens[index].text.as_str(),
            "=" | "(" | "[" | "," | ":" | "=>" | "return"
        )
    })
}

fn collect_class_methods(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    body_open: usize,
    body_close: usize,
    class_name: Option<&str>,
    allow_signature_methods: bool,
    builder: &mut AnchorBuilder,
) {
    let mut index = body_open + 1;
    while index < body_close {
        if tokens[index].text == "@" {
            index = decorator_expression_end(tokens, index, body_close) + 1;
            continue;
        }
        if tokens[index].text == "{" {
            index = braces
                .get(index)
                .and_then(|match_index| *match_index)
                .unwrap_or(index)
                + 1;
            continue;
        }
        if is_method_modifier(&tokens[index].text) {
            index += 1;
            continue;
        }
        let Some(method) = class_method_signature(source, tokens, braces, index, body_close) else {
            if let Some(next_index) =
                class_field_next_index(source, tokens, braces, index, body_close, false)
            {
                index = next_index;
                continue;
            }
            index += 1;
            continue;
        };
        if tokens[index].text == "constructor"
            || tokens[index].text == "get"
            || tokens[index].text == "set"
        {
            index += 1;
            continue;
        }
        if index > body_open && matches!(tokens[index - 1].text.as_str(), "get" | "set") {
            index += 1;
            continue;
        }

        let Some(body_open_index) =
            find_next_until(tokens, method.paren_index + 1, "{", &[";", "}"])
        else {
            if allow_signature_methods
                || method_has_leading_modifier(tokens, body_open, method.start_index, "abstract")
            {
                let method_end = method_signature_end(tokens, method.paren_index, body_close);
                builder.add(
                    method_anchor(class_name, &method.name),
                    tokens[decorated_start_index(tokens, method.start_index)].line,
                    tokens[method_end].line,
                    1,
                    AnchorKind::Method,
                );
                index = method_end + 1;
                continue;
            }
            index += 1;
            continue;
        };
        if body_open_index >= body_close {
            index += 1;
            continue;
        }
        if let Some(method_end) = malformed_method_parameter_end_index(
            source,
            tokens,
            parens,
            braces,
            &method,
            body_open_index,
            body_close,
        ) {
            builder.add(
                method_anchor(class_name, &method.name),
                tokens[decorated_start_index(tokens, method.start_index)].line,
                tokens[method_end].line,
                complexity_between(tokens, method.start_index, method_end, None),
                AnchorKind::Method,
            );
            index = method_end + 1;
            continue;
        }
        let Some(method_close) = braces
            .get(body_open_index)
            .and_then(|match_index| *match_index)
            .filter(|match_index| *match_index <= body_close)
        else {
            index += 1;
            continue;
        };
        builder.add(
            method_anchor(class_name, &method.name),
            tokens[decorated_start_index(tokens, method.start_index)].line,
            tokens[method_close].line,
            complexity_between(tokens, method.start_index, method_close, None),
            AnchorKind::Method,
        );
        index = method_close + 1;
    }
}

struct ClassMethodSignature {
    name: String,
    start_index: usize,
    paren_index: usize,
}

fn class_method_signature(
    source: &str,
    tokens: &[Token],
    braces: &[Option<usize>],
    index: usize,
    body_close: usize,
) -> Option<ClassMethodSignature> {
    if tokens[index].text == "*" {
        let name_index = index + 1;
        if name_index < body_close && is_identifier(&tokens[name_index].text) {
            let paren_index = optional_method_paren_index(tokens, name_index, body_close)?;
            return Some(ClassMethodSignature {
                name: tokens[name_index].text.clone(),
                start_index: name_index,
                paren_index,
            });
        }
        return None;
    }

    if tokens[index].text == "[" {
        let close_index = computed_method_name_close_index(tokens, braces, index, body_close)?;
        if let Some(paren_index) = optional_method_paren_index(tokens, close_index, body_close) {
            let name = source
                .get(tokens[index].start..tokens[close_index].end)
                .unwrap_or("")
                .to_string();
            return Some(ClassMethodSignature {
                name,
                start_index: index,
                paren_index,
            });
        }
        return None;
    }

    if tokens[index].text == "#" {
        let name_index = index + 1;
        if name_index < body_close && is_identifier(&tokens[name_index].text) {
            let paren_index = optional_method_paren_index(tokens, name_index, body_close)?;
            return Some(ClassMethodSignature {
                name: format!("#{}", tokens[name_index].text),
                start_index: index,
                paren_index,
            });
        }
        return None;
    }

    if is_literal_method_name(&tokens[index].text) {
        let paren_index = optional_method_paren_index(tokens, index, body_close)?;
        return Some(ClassMethodSignature {
            name: tokens[index].text.clone(),
            start_index: index,
            paren_index,
        });
    }

    if is_identifier(&tokens[index].text) {
        let paren_index = optional_method_paren_index(tokens, index, body_close)?;
        return Some(ClassMethodSignature {
            name: tokens[index].text.clone(),
            start_index: index,
            paren_index,
        });
    }

    None
}

fn optional_method_paren_index(
    tokens: &[Token],
    name_end_index: usize,
    body_close: usize,
) -> Option<usize> {
    let next_index = name_end_index + 1;
    if next_index < body_close && tokens[next_index].text == "(" {
        return Some(next_index);
    }
    if next_index + 1 < body_close
        && tokens[next_index].text == "?"
        && tokens[next_index + 1].text == "("
    {
        return Some(next_index + 1);
    }
    None
}

fn class_field_next_index(
    source: &str,
    tokens: &[Token],
    braces: &[Option<usize>],
    index: usize,
    body_close: usize,
    allow_computed_member_boundary: bool,
) -> Option<usize> {
    let mut cursor = class_field_name_end_index(tokens, braces, index, body_close)?;
    if cursor < body_close && matches!(tokens[cursor].text.as_str(), "?" | "!") {
        cursor += 1;
    }
    if cursor >= body_close {
        return Some(body_close);
    }
    if tokens[cursor].text == "(" {
        return None;
    }
    if !matches!(tokens[cursor].text.as_str(), "=" | ":" | ";")
        && tokens[cursor].line == tokens[index].line
    {
        return None;
    }
    Some(class_field_declaration_next_index(
        source,
        tokens,
        braces,
        cursor,
        body_close,
        allow_computed_member_boundary,
    ))
}

fn class_field_name_end_index(
    tokens: &[Token],
    braces: &[Option<usize>],
    index: usize,
    body_close: usize,
) -> Option<usize> {
    match tokens.get(index).map(|token| token.text.as_str()) {
        Some("accessor") => {
            let name_index = index + 1;
            if name_index < body_close
                && (is_identifier(&tokens[name_index].text)
                    || is_literal_method_name(&tokens[name_index].text))
            {
                Some(name_index + 1)
            } else {
                None
            }
        }
        Some("#") => {
            let name_index = index + 1;
            if name_index < body_close && is_identifier(&tokens[name_index].text) {
                Some(name_index + 1)
            } else {
                None
            }
        }
        Some("[") => computed_method_name_close_index(tokens, braces, index, body_close)
            .map(|close_index| close_index + 1),
        Some(text) if is_identifier(text) || is_literal_method_name(text) => Some(index + 1),
        _ => None,
    }
}

fn class_field_declaration_next_index(
    source: &str,
    tokens: &[Token],
    braces: &[Option<usize>],
    start: usize,
    body_close: usize,
    allow_computed_member_boundary: bool,
) -> usize {
    let mut paren_depth = 0_i32;
    let mut brace_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    let mut index = start;
    let mut seen_initializer_token = false;
    while index < body_close {
        if seen_initializer_token
            && paren_depth == 0
            && brace_depth == 0
            && bracket_depth == 0
            && index > start
            && tokens[index].line > tokens[index - 1].line
            && (allow_computed_member_boundary || tokens[index].text != "[")
            && previous_token_can_end_field_initializer(tokens, index)
            && is_class_member_boundary(source, tokens, braces, index, body_close)
        {
            return index;
        }
        match tokens[index].text.as_str() {
            "(" => paren_depth += 1,
            ")" if paren_depth > 0 => paren_depth -= 1,
            "{" => {
                if let Some(close_index) = braces
                    .get(index)
                    .and_then(|match_index| *match_index)
                    .filter(|match_index| *match_index < body_close)
                {
                    seen_initializer_token = true;
                    index = close_index + 1;
                    continue;
                }
                brace_depth += 1;
            }
            "}" if brace_depth > 0 => brace_depth -= 1,
            "[" => bracket_depth += 1,
            "]" if bracket_depth > 0 => bracket_depth -= 1,
            ";" if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 => {
                return index + 1;
            }
            _ => {}
        }
        if !matches!(tokens[index].text.as_str(), "=" | ":" | "?" | "!") {
            seen_initializer_token = true;
        }
        index += 1;
    }
    body_close
}

fn previous_token_can_end_field_initializer(tokens: &[Token], index: usize) -> bool {
    let Some(previous) = index
        .checked_sub(1)
        .and_then(|previous| tokens.get(previous))
    else {
        return false;
    };
    !matches!(
        previous.text.as_str(),
        "=" | ":"
            | "?"
            | "!"
            | "&&"
            | "||"
            | "??"
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "."
            | ","
            | "("
            | "["
            | "{"
            | "=>"
            | "extends"
            | "as"
            | "satisfies"
    )
}

fn is_class_member_boundary(
    source: &str,
    tokens: &[Token],
    braces: &[Option<usize>],
    index: usize,
    body_close: usize,
) -> bool {
    if tokens[index].text == "@" {
        return true;
    }
    let mut cursor = index;
    while cursor < body_close && is_method_modifier(&tokens[cursor].text) {
        cursor += 1;
    }
    class_method_signature(source, tokens, braces, cursor, body_close).is_some()
        || class_field_name_end_index(tokens, braces, cursor, body_close).is_some_and(|end_index| {
            let mut after_name = end_index;
            if after_name < body_close && matches!(tokens[after_name].text.as_str(), "?" | "!") {
                after_name += 1;
            }
            after_name >= body_close
                || matches!(tokens[after_name].text.as_str(), "=" | ":" | ";")
                || tokens[after_name].line > tokens[cursor].line
        })
}

fn method_has_leading_modifier(
    tokens: &[Token],
    body_open: usize,
    method_start_index: usize,
    modifier: &str,
) -> bool {
    let mut cursor = method_start_index;
    while cursor > body_open + 1 {
        cursor -= 1;
        if tokens[cursor].text == modifier {
            return true;
        }
        if !is_method_modifier(&tokens[cursor].text) {
            break;
        }
    }
    false
}

#[derive(Clone, Debug)]
struct MalformedClassMemberRecovery {
    method_limit: usize,
    end_index: usize,
}

fn malformed_class_member_recovery(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    start: usize,
    body_close: usize,
) -> Option<MalformedClassMemberRecovery> {
    let mut index = start;
    while index + 1 < body_close {
        if tokens[index].text == "@" {
            index = decorator_expression_end(tokens, index, body_close) + 1;
            continue;
        }
        if let Some(next_index) =
            class_field_next_index(source, tokens, braces, index, body_close, true)
        {
            if next_index < body_close
                && matches!(tokens[next_index].text.as_str(), "[" | "*")
                && next_index > index
                && tokens[next_index - 1].text != ";"
                && tokens[next_index].line > tokens[next_index - 1].line
            {
                return Some(MalformedClassMemberRecovery {
                    method_limit: next_index,
                    end_index: same_line_end_index(tokens, next_index, body_close),
                });
            }
            index = next_index;
            continue;
        }
        if is_numeric_literal(&tokens[index].text)
            && is_identifier(&tokens[index + 1].text)
            && tokens[index].line == tokens[index + 1].line
            && tokens
                .get(index + 2)
                .is_some_and(|token| matches!(token.text.as_str(), "(" | ";" | "="))
        {
            return Some(MalformedClassMemberRecovery {
                method_limit: index,
                end_index: index.checked_sub(1).unwrap_or(index),
            });
        }
        if tokens[index].text == "[" {
            let malformed = computed_method_name_close_index(tokens, braces, index, body_close)
                .map(|close_index| {
                    optional_method_paren_index(tokens, close_index, body_close).is_none()
                })
                .unwrap_or(true);
            if malformed {
                return Some(MalformedClassMemberRecovery {
                    method_limit: index,
                    end_index: same_line_end_index(tokens, index, body_close),
                });
            }
        }
        if let Some(method) = class_method_signature(source, tokens, braces, index, body_close) {
            if let Some(body_open_index) =
                find_next_until(tokens, method.paren_index + 1, "{", &[";", "}"])
            {
                if let Some(end_index) = malformed_method_parameter_end_index(
                    source,
                    tokens,
                    parens,
                    braces,
                    &method,
                    body_open_index,
                    body_close,
                ) {
                    return Some(MalformedClassMemberRecovery {
                        method_limit: (end_index + 1).min(body_close),
                        end_index,
                    });
                }
            }
        }
        index += 1;
    }
    None
}

fn malformed_method_parameter_end_index(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    method: &ClassMethodSignature,
    body_open_index: usize,
    body_close: usize,
) -> Option<usize> {
    if parens
        .get(method.paren_index)
        .and_then(|match_index| *match_index)
        .is_some_and(|paren_close| paren_close < body_open_index)
    {
        return None;
    }
    let after_body = braces
        .get(body_open_index)
        .and_then(|match_index| *match_index)
        .filter(|match_index| *match_index < body_close)
        .map(|method_close| method_close + 1)
        .unwrap_or(body_open_index + 1);
    Some(
        next_class_member_start_index(source, tokens, braces, after_body, body_close)
            .unwrap_or_else(|| body_close.saturating_sub(1).max(method.start_index)),
    )
}

fn next_class_member_start_index(
    source: &str,
    tokens: &[Token],
    braces: &[Option<usize>],
    start: usize,
    body_close: usize,
) -> Option<usize> {
    let mut index = start;
    while index < body_close {
        if tokens[index].text == "@" {
            index = decorator_expression_end(tokens, index, body_close) + 1;
            continue;
        }
        if tokens[index].text == "{" {
            index = braces
                .get(index)
                .and_then(|match_index| *match_index)
                .unwrap_or(index)
                + 1;
            continue;
        }
        if is_method_modifier(&tokens[index].text) {
            index += 1;
            continue;
        }
        if let Some(next_index) =
            class_field_next_index(source, tokens, braces, index, body_close, false)
        {
            index = next_index;
            continue;
        }
        if class_method_signature(source, tokens, braces, index, body_close).is_some() {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn computed_method_name_close_index(
    tokens: &[Token],
    braces: &[Option<usize>],
    open_index: usize,
    body_close: usize,
) -> Option<usize> {
    let mut index = open_index + 1;
    while index < body_close {
        match tokens[index].text.as_str() {
            "]" => return Some(index),
            ";" => return None,
            "{" => {
                let close_index = braces
                    .get(index)
                    .and_then(|match_index| *match_index)
                    .filter(|match_index| *match_index < body_close)?;
                index = close_index + 1;
            }
            _ => index += 1,
        }
    }
    None
}

fn same_line_end_index(tokens: &[Token], start: usize, limit: usize) -> usize {
    let line = tokens[start].line;
    let mut index = start;
    while index + 1 < limit && tokens[index + 1].line == line {
        index += 1;
    }
    index
}

fn method_anchor(class_name: Option<&str>, method_name: &str) -> String {
    match class_name {
        Some(class_name) => format!("fn:{class_name}.{method_name}"),
        None => format!("fn:{method_name}"),
    }
}

fn is_tsx_file(file: &RelativePath) -> bool {
    file.as_str().ends_with(".tsx")
}

fn is_rust_file(file: &RelativePath) -> bool {
    file.as_str().ends_with(".rs")
}

fn is_c_family_file(file: &RelativePath) -> bool {
    let lower = file.as_str().to_ascii_lowercase();
    matches!(
        lower.rsplit_once('.').map(|(_, extension)| extension),
        Some("c" | "h" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "ipp" | "inc")
    )
}

fn tsx_ambiguous_generic_arrow_limit(tokens: &[Token]) -> Option<usize> {
    for index in 0..tokens.len() {
        if !is_variable_declaration_keyword(&tokens[index].text) || !is_top_level(tokens, index) {
            continue;
        }
        let Some(eq_index) = find_next_until(tokens, index + 1, "=", &[";"]) else {
            continue;
        };
        if tsx_initializer_starts_with_ambiguous_bare_generic_arrow(tokens, eq_index) {
            return Some(index);
        }
    }
    None
}

fn tsx_initializer_starts_with_ambiguous_bare_generic_arrow(
    tokens: &[Token],
    eq_index: usize,
) -> bool {
    let lt_index = eq_index + 1;
    if tokens.get(lt_index).is_none_or(|token| token.text != "<") {
        return false;
    }
    let Some(name_index) = tokens
        .get(lt_index + 1)
        .filter(|token| is_identifier(&token.text))
        .map(|_| lt_index + 1)
    else {
        return false;
    };
    if tokens
        .get(name_index + 1)
        .is_none_or(|token| token.text != ">")
    {
        return false;
    }
    let paren_index = name_index + 2;
    if tokens
        .get(paren_index)
        .is_none_or(|token| token.text != "(")
    {
        return false;
    }
    find_next_until(tokens, paren_index + 1, "=>", &[";"]).is_some()
}

fn tsx_unclosed_jsx_arrow_recovery(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
) -> Option<TsxMalformedJsxArrowRecovery> {
    for index in 0..tokens.len() {
        if !is_variable_declaration_keyword(&tokens[index].text) || !is_top_level(tokens, index) {
            continue;
        }
        let Some(eq_index) = find_next_until(tokens, index + 1, "=", &[",", ";"]) else {
            continue;
        };
        let Some(arrow_index) = variable_arrow_index(tokens, parens, eq_index + 1) else {
            continue;
        };
        let Some(open_index) = tokens
            .get(arrow_index + 1)
            .filter(|token| token.text == "<")
            .map(|_| arrow_index + 1)
        else {
            continue;
        };
        if !tsx_starts_unclosed_jsx_element(tokens, open_index) {
            continue;
        }
        let token_limit =
            next_top_level_declaration_after(tokens, open_index + 1).unwrap_or(tokens.len());
        return Some(TsxMalformedJsxArrowRecovery {
            token_limit,
            eq_index,
            end_line: count_newlines(source) + 1,
        });
    }
    None
}

fn tsx_mismatched_jsx_arrow_diagnostic(
    tokens: &[Token],
    parens: &[Option<usize>],
) -> Option<usize> {
    for index in 0..tokens.len() {
        if !is_variable_declaration_keyword(&tokens[index].text) || !is_top_level(tokens, index) {
            continue;
        }
        let Some(eq_index) = find_next_until(tokens, index + 1, "=", &[",", ";"]) else {
            continue;
        };
        let Some(arrow_index) = variable_arrow_index(tokens, parens, eq_index + 1) else {
            continue;
        };
        let Some(open_index) = tokens
            .get(arrow_index + 1)
            .filter(|token| token.text == "<")
            .map(|_| arrow_index + 1)
        else {
            continue;
        };
        if tsx_starts_mismatched_jsx_element(tokens, open_index) {
            return Some(eq_index);
        }
    }
    None
}

fn tsx_starts_unclosed_jsx_element(tokens: &[Token], open_index: usize) -> bool {
    if !tokens
        .get(open_index + 1)
        .is_some_and(|token| is_identifier(&token.text))
    {
        return false;
    }
    let Some(close_index) = find_next_until(tokens, open_index + 2, ">", &[";"]) else {
        return false;
    };
    if tokens
        .get(close_index.checked_sub(1).unwrap_or(close_index))
        .is_some_and(|token| token.text == "/")
    {
        return false;
    }
    let mut cursor = close_index + 1;
    while cursor + 3 < tokens.len() {
        if tokens[cursor].text == "<" && tokens[cursor + 1].text == "/" {
            return false;
        }
        cursor += 1;
    }
    true
}

fn tsx_starts_mismatched_jsx_element(tokens: &[Token], open_index: usize) -> bool {
    let Some(open_name) = tokens
        .get(open_index + 1)
        .filter(|token| is_identifier(&token.text))
        .map(|token| token.text.as_str())
    else {
        return false;
    };
    let Some(close_index) = find_next_until(tokens, open_index + 2, ">", &[";"]) else {
        return false;
    };
    if tokens
        .get(close_index.checked_sub(1).unwrap_or(close_index))
        .is_some_and(|token| token.text == "/")
    {
        return false;
    }
    let mut cursor = close_index + 1;
    while cursor + 3 < tokens.len() {
        if tokens[cursor].text == "<" && tokens[cursor + 1].text == "/" {
            return tokens[cursor + 3].text == ">" && tokens[cursor + 2].text != open_name;
        }
        cursor += 1;
    }
    false
}

fn next_top_level_declaration_after(tokens: &[Token], start: usize) -> Option<usize> {
    for index in start..tokens.len() {
        if is_top_level(tokens, index)
            && matches!(
                tokens[index].text.as_str(),
                "function" | "class" | "const" | "let" | "var" | "using" | "export"
            )
        {
            return Some(index);
        }
    }
    None
}

fn collect_variable_anchors(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    malformed_jsx_arrow: Option<(usize, u32)>,
    builder: &mut AnchorBuilder,
    exports: &mut Vec<ExportCandidate>,
    declarations: &mut HashMap<String, ExportCandidate>,
) {
    let brackets = matching_tokens(tokens, "[", "]");
    for index in 0..tokens.len() {
        if !is_variable_declaration_keyword(&tokens[index].text) || !is_top_level(tokens, index) {
            continue;
        }
        if tokens[index].text == "const"
            && tokens
                .get(index + 1)
                .is_some_and(|token| token.text == "enum")
        {
            continue;
        }
        let mut cursor = index + 1;
        while cursor < tokens.len() {
            if tokens[cursor].text == ";" || tokens[cursor].text == "}" {
                break;
            }
            if matches!(tokens[cursor].text.as_str(), "{" | "[") {
                let Some(pattern_close) =
                    binding_pattern_close_index(tokens, braces, &brackets, cursor)
                else {
                    cursor += 1;
                    continue;
                };
                for declaration in binding_pattern_declarations(
                    tokens,
                    parens,
                    braces,
                    &brackets,
                    cursor,
                    pattern_close,
                ) {
                    declarations
                        .entry(declaration.name.clone())
                        .or_insert_with(|| declaration.clone());
                    record_export_variable(tokens, index, declaration, exports);
                }
                cursor =
                    variable_declaration_after_binding_pattern_next_cursor(tokens, pattern_close);
                continue;
            }
            if !is_identifier(&tokens[cursor].text) {
                cursor += 1;
                continue;
            }
            let name_index = cursor;
            let name = tokens[name_index].text.clone();

            let declaration = if let Some(eq_index) = variable_declaration_initializer_eq_index(
                tokens,
                parens,
                braces,
                &brackets,
                name_index + 1,
                tokens.len(),
            ) {
                let declaration = variable_initializer_range(
                    tokens,
                    parens,
                    braces,
                    eq_index,
                    malformed_jsx_arrow,
                )
                .map(|(start, end, complexity)| {
                    builder.add(
                        format!("fn:{name}"),
                        start,
                        end,
                        complexity,
                        AnchorKind::Function,
                    );
                    ExportCandidate::new(name.clone(), start, end, complexity, name_index, false)
                })
                .unwrap_or_else(|| {
                    ExportCandidate::new(
                        name.clone(),
                        tokens[name_index].line,
                        variable_declaration_end_line(tokens, eq_index),
                        1,
                        name_index,
                        false,
                    )
                });
                cursor = variable_declaration_after_initializer_next_cursor(tokens, eq_index);
                declaration
            } else {
                let end_index = variable_declaration_without_initializer_end_index(
                    tokens,
                    parens,
                    braces,
                    &brackets,
                    name_index,
                    tokens.len(),
                );
                let declaration = ExportCandidate::new(
                    name.clone(),
                    tokens[name_index].line,
                    tokens[end_index].line,
                    1,
                    name_index,
                    false,
                );
                cursor = variable_declaration_without_initializer_next_cursor(tokens, end_index);
                declaration
            };

            declarations
                .entry(name.clone())
                .or_insert_with(|| declaration.clone());
            record_export_variable(tokens, index, declaration, exports);
        }
        let _ = source;
    }
}

#[derive(Clone, Debug)]
struct BindingNameCandidate {
    name: String,
    start_index: usize,
    end_index: usize,
}

fn binding_pattern_close_index(
    tokens: &[Token],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    open_index: usize,
) -> Option<usize> {
    match tokens.get(open_index).map(|token| token.text.as_str()) {
        Some("{") => braces.get(open_index).and_then(|match_index| *match_index),
        Some("[") => brackets
            .get(open_index)
            .and_then(|match_index| *match_index),
        _ => None,
    }
}

fn binding_pattern_declarations(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    pattern_open: usize,
    pattern_close: usize,
) -> Vec<ExportCandidate> {
    let mut bindings = Vec::new();
    collect_variable_binding_names(
        tokens,
        parens,
        braces,
        brackets,
        pattern_open,
        pattern_close,
        &mut bindings,
    );
    bindings
        .into_iter()
        .map(|binding| {
            ExportCandidate::new(
                binding.name,
                tokens[binding.start_index].line,
                tokens[binding.end_index].line,
                complexity_between(tokens, binding.start_index, binding.end_index, None),
                binding.start_index,
                false,
            )
        })
        .collect()
}

fn collect_variable_binding_names(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    pattern_open: usize,
    pattern_close: usize,
    bindings: &mut Vec<BindingNameCandidate>,
) {
    match tokens.get(pattern_open).map(|token| token.text.as_str()) {
        Some("{") => collect_variable_object_binding_names(
            tokens,
            parens,
            braces,
            brackets,
            pattern_open,
            pattern_close,
            bindings,
        ),
        Some("[") => collect_variable_array_binding_names(
            tokens,
            parens,
            braces,
            brackets,
            pattern_open,
            pattern_close,
            bindings,
        ),
        _ => {}
    }
}

fn collect_variable_object_binding_names(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    pattern_open: usize,
    pattern_close: usize,
    bindings: &mut Vec<BindingNameCandidate>,
) {
    let mut cursor = pattern_open + 1;
    while cursor < pattern_close {
        if tokens[cursor].text == "," {
            cursor += 1;
            continue;
        }
        let element_end =
            binding_element_end_index(tokens, parens, braces, brackets, cursor, pattern_close);
        collect_variable_object_binding_element(
            tokens,
            parens,
            braces,
            brackets,
            cursor,
            element_end,
            bindings,
        );
        cursor = element_end + 1;
        if cursor < pattern_close && tokens[cursor].text == "," {
            cursor += 1;
        }
    }
}

fn collect_variable_array_binding_names(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    pattern_open: usize,
    pattern_close: usize,
    bindings: &mut Vec<BindingNameCandidate>,
) {
    let mut cursor = pattern_open + 1;
    while cursor < pattern_close {
        if tokens[cursor].text == "," {
            cursor += 1;
            continue;
        }
        let element_end =
            binding_element_end_index(tokens, parens, braces, brackets, cursor, pattern_close);
        collect_variable_binding_target(
            tokens,
            parens,
            braces,
            brackets,
            cursor,
            element_end,
            bindings,
        );
        cursor = element_end + 1;
        if cursor < pattern_close && tokens[cursor].text == "," {
            cursor += 1;
        }
    }
}

fn collect_variable_object_binding_element(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    start: usize,
    end: usize,
    bindings: &mut Vec<BindingNameCandidate>,
) {
    let start = skip_binding_rest_dots(tokens, start, end);
    if start > end {
        return;
    }
    let target_start =
        find_top_level_token_in_range(tokens, parens, braces, brackets, start, end, ":")
            .map(|colon_index| colon_index + 1)
            .unwrap_or(start);
    collect_variable_binding_target(
        tokens,
        parens,
        braces,
        brackets,
        target_start,
        end,
        bindings,
    );
}

fn collect_variable_binding_target(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    start: usize,
    end: usize,
    bindings: &mut Vec<BindingNameCandidate>,
) {
    let start = skip_binding_rest_dots(tokens, start, end);
    if start > end {
        return;
    }
    match tokens[start].text.as_str() {
        "{" | "[" => {
            if let Some(pattern_close) =
                binding_pattern_close_index(tokens, braces, brackets, start)
                    .filter(|close_index| *close_index <= end)
            {
                collect_variable_binding_names(
                    tokens,
                    parens,
                    braces,
                    brackets,
                    start,
                    pattern_close,
                    bindings,
                );
            }
        }
        text if is_identifier(text) => {
            let end_index = find_top_level_token_in_range(
                tokens,
                parens,
                braces,
                brackets,
                start + 1,
                end,
                "=",
            )
            .map(|_| end)
            .unwrap_or(start);
            bindings.push(BindingNameCandidate {
                name: tokens[start].text.clone(),
                start_index: start,
                end_index,
            });
        }
        _ => {}
    }
}

fn binding_element_end_index(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    start: usize,
    limit: usize,
) -> usize {
    let mut cursor = start;
    let mut last = start;
    while cursor < limit {
        if tokens[cursor].text == "," {
            return last;
        }
        if let Some(close_index) =
            grouped_token_close_index(tokens, parens, braces, brackets, cursor)
                .filter(|close_index| *close_index < limit)
        {
            last = close_index;
            cursor = close_index + 1;
            continue;
        }
        last = cursor;
        cursor += 1;
    }
    last
}

fn find_top_level_token_in_range(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    start: usize,
    end: usize,
    text: &str,
) -> Option<usize> {
    let mut cursor = start;
    while cursor <= end && cursor < tokens.len() {
        if tokens[cursor].text == text {
            return Some(cursor);
        }
        if let Some(close_index) =
            grouped_token_close_index(tokens, parens, braces, brackets, cursor)
                .filter(|close_index| *close_index <= end)
        {
            cursor = close_index + 1;
            continue;
        }
        cursor += 1;
    }
    None
}

fn grouped_token_close_index(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    open_index: usize,
) -> Option<usize> {
    match tokens.get(open_index).map(|token| token.text.as_str()) {
        Some("(") => parens.get(open_index).and_then(|match_index| *match_index),
        Some("{") => braces.get(open_index).and_then(|match_index| *match_index),
        Some("[") => brackets
            .get(open_index)
            .and_then(|match_index| *match_index),
        _ => None,
    }
}

fn skip_binding_rest_dots(tokens: &[Token], start: usize, end: usize) -> usize {
    let mut cursor = start;
    while cursor <= end && tokens[cursor].text == "." {
        cursor += 1;
    }
    cursor
}

fn variable_declaration_after_binding_pattern_next_cursor(
    tokens: &[Token],
    pattern_close: usize,
) -> usize {
    if let Some(eq_index) = find_next_until(tokens, pattern_close + 1, "=", &[",", ";"]) {
        return variable_declaration_after_initializer_next_cursor(tokens, eq_index);
    }
    match tokens
        .get(pattern_close + 1)
        .map(|token| token.text.as_str())
    {
        Some(",") => pattern_close + 2,
        _ => tokens.len(),
    }
}

fn collect_export_anchors(builder: &mut AnchorBuilder, mut exports: Vec<ExportCandidate>) {
    exports.sort_by(|left, right| {
        left.export_assignment_priority
            .cmp(&right.export_assignment_priority)
            .reverse()
            .then_with(|| left.priority_group.cmp(&right.priority_group))
            .then_with(|| {
                left.function_priority
                    .cmp(&right.function_priority)
                    .reverse()
            })
            .then_with(|| left.order.cmp(&right.order))
    });
    let mut seen = HashMap::<String, bool>::new();
    for exported in exports {
        if seen.insert(exported.name.clone(), true).is_some() {
            continue;
        }
        builder.add(
            format!("export:{}", exported.name),
            exported.start,
            exported.end,
            exported.complexity,
            AnchorKind::Export,
        );
    }
}

fn collect_default_export_anchors(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    exports: &mut Vec<ExportCandidate>,
) {
    for index in 0..tokens.len() {
        if tokens[index].text != "export" || !is_top_level(tokens, index) {
            continue;
        }
        let mut cursor = index + 1;
        if tokens
            .get(cursor)
            .is_none_or(|token| token.text != "default")
        {
            continue;
        }
        cursor += 1;
        while tokens
            .get(cursor)
            .is_some_and(|token| is_export_default_modifier(&token.text))
        {
            cursor += 1;
        }

        let Some(token) = tokens.get(cursor) else {
            continue;
        };
        let candidate = match token.text.as_str() {
            "function" if function_name_index(tokens, cursor).is_none() => {
                anonymous_function_export(tokens, parens, braces, cursor)
            }
            "class" => None,
            "interface" => type_like_default_export(tokens, braces, cursor),
            "type" | "enum" | "namespace" | "module" => None,
            _ => default_expression_export(tokens, braces, cursor),
        };
        if let Some((start, end, complexity)) = candidate {
            exports.push(ExportCandidate::new(
                "default".to_string(),
                start,
                end,
                complexity,
                cursor,
                token.text == "function",
            ));
        }
    }
}

fn collect_export_alias_anchors(
    tokens: &[Token],
    declarations: &HashMap<String, ExportCandidate>,
    exports: &mut Vec<ExportCandidate>,
) {
    for index in 0..tokens.len() {
        if tokens[index].text != "export" || !is_top_level(tokens, index) {
            continue;
        }
        let mut open_index = index + 1;
        if tokens
            .get(open_index)
            .is_some_and(|token| token.text == "type")
        {
            open_index += 1;
        }
        if tokens.get(open_index).is_none_or(|token| token.text != "{") {
            continue;
        }
        let Some(close_index) = find_next_until(tokens, open_index + 1, "}", &[";"]) else {
            continue;
        };
        if tokens
            .get(close_index + 1)
            .is_some_and(|token| token.text == "from")
        {
            continue;
        }
        if export_alias_list_has_quoted_export_name(tokens, open_index + 1, close_index) {
            continue;
        }

        let mut cursor = open_index + 1;
        while cursor < close_index {
            if tokens[cursor].text == "type"
                && tokens
                    .get(cursor + 1)
                    .is_some_and(|token| is_identifier(&token.text))
            {
                cursor += 1;
            }
            if !is_identifier(&tokens[cursor].text) {
                cursor += 1;
                continue;
            }
            let local_name = tokens[cursor].text.clone();
            let mut exported_name = local_name.clone();
            let mut stop_after_specifier = false;
            if cursor + 1 < close_index && tokens[cursor + 1].text == "as" {
                if cursor + 2 < close_index && is_identifier(&tokens[cursor + 2].text) {
                    exported_name = tokens[cursor + 2].text.clone();
                } else {
                    let (recovered_name, stop_after_alias) =
                        invalid_export_alias_target_recovery(tokens, cursor + 2, close_index);
                    exported_name = recovered_name;
                    stop_after_specifier = stop_after_alias;
                }
                cursor = export_specifier_next_cursor(tokens, cursor + 1, close_index);
            } else {
                cursor += 1;
            }

            if let Some(declaration) = declarations.get(&local_name) {
                exports.push(ExportCandidate::new(
                    exported_name,
                    declaration.start,
                    declaration.end,
                    declaration.complexity,
                    index,
                    false,
                ));
            }
            if stop_after_specifier {
                break;
            }
        }
    }
}

fn collect_import_equals_declarations(
    tokens: &[Token],
    declarations: &mut HashMap<String, ExportCandidate>,
) {
    for index in 0..tokens.len() {
        if tokens[index].text != "import" || !is_top_level(tokens, index) {
            continue;
        }
        let Some(name_index) = import_alias_name_index(tokens, index) else {
            continue;
        };
        if import_alias_tail_is_non_alias(tokens, name_index) {
            continue;
        }
        let end_index = import_alias_end_index(tokens, name_index);
        let start_index = leading_export_index(tokens, index).unwrap_or(index);
        declarations
            .entry(tokens[name_index].text.clone())
            .or_insert_with(|| {
                ExportCandidate::new(
                    tokens[name_index].text.clone(),
                    tokens[start_index].line,
                    tokens[end_index].line,
                    1,
                    index,
                    false,
                )
            });
    }
}

fn import_alias_name_index(tokens: &[Token], import_index: usize) -> Option<usize> {
    let candidate = import_index + 1;
    let token = tokens.get(candidate)?;
    if token.text == "type" {
        if tokens
            .get(candidate + 1)
            .is_some_and(|next| next.text == "=")
        {
            return Some(candidate);
        }
        return tokens
            .get(candidate + 1)
            .filter(|next| is_identifier(&next.text))
            .map(|_| candidate + 1);
    }
    is_identifier(&token.text).then_some(candidate)
}

fn import_alias_tail_is_non_alias(tokens: &[Token], name_index: usize) -> bool {
    tokens
        .get(name_index + 1)
        .is_some_and(|token| matches!(token.text.as_str(), "," | "from"))
}

fn import_alias_end_index(tokens: &[Token], name_index: usize) -> usize {
    let start = name_index + 1;
    let limit = next_top_level_declaration_after(tokens, start).unwrap_or(tokens.len());
    for index in start..limit {
        if tokens[index].text == ";" {
            return index;
        }
    }
    if limit > start {
        limit - 1
    } else {
        name_index
    }
}

fn export_specifier_next_cursor(tokens: &[Token], start: usize, close_index: usize) -> usize {
    let mut cursor = start + 1;
    while cursor < close_index && tokens[cursor].text != "," {
        cursor += 1;
    }
    cursor
}

fn export_alias_list_has_quoted_export_name(
    tokens: &[Token],
    start: usize,
    close_index: usize,
) -> bool {
    let mut cursor = start;
    while cursor < close_index {
        if tokens[cursor].text == "as"
            && tokens
                .get(cursor + 1)
                .is_some_and(|token| is_string_literal(&token.text))
        {
            return true;
        }
        cursor += 1;
    }
    false
}

fn invalid_export_alias_target_recovery(
    tokens: &[Token],
    target_index: usize,
    close_index: usize,
) -> (String, bool) {
    let Some(token) = tokens
        .get(target_index)
        .filter(|_| target_index < close_index)
    else {
        return (String::new(), false);
    };
    if token.text == "#" {
        if tokens
            .get(target_index + 1)
            .filter(|_| target_index + 1 < close_index)
            .is_some_and(|next| is_identifier(&next.text))
        {
            return (format!("#{}", tokens[target_index + 1].text), false);
        }
        return ("#".to_string(), false);
    }
    if matches!(token.text.as_str(), "," | "." | "?" | ":" | ")" | "]") {
        return (String::new(), false);
    }
    (String::new(), !is_identifier(&token.text))
}

fn collect_export_import_anchors(tokens: &[Token], exports: &mut Vec<ExportCandidate>) {
    for index in 0..tokens.len() {
        if tokens[index].text != "export" || !is_top_level(tokens, index) {
            continue;
        }
        if tokens
            .get(index + 1)
            .is_none_or(|token| token.text != "import")
        {
            continue;
        }
        let Some(name_index) = import_alias_name_index(tokens, index + 1) else {
            continue;
        };
        if import_alias_tail_is_non_alias(tokens, name_index) {
            continue;
        }
        let end_index = import_alias_end_index(tokens, name_index);
        exports.push(ExportCandidate::new(
            tokens[name_index].text.clone(),
            tokens[index].line,
            tokens[end_index].line,
            1,
            index,
            false,
        ));
    }
}

fn collect_export_assignment_namespace_anchors(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    exports: &mut Vec<ExportCandidate>,
) {
    for index in 0..tokens.len() {
        if tokens[index].text != "export" || !is_top_level(tokens, index) {
            continue;
        }
        if tokens.get(index + 1).is_none_or(|token| token.text != "=") {
            continue;
        }
        let Some(namespace_name) = tokens
            .get(index + 2)
            .filter(|token| is_identifier(&token.text))
            .map(|token| token.text.as_str())
        else {
            continue;
        };
        collect_exported_namespace_members(source, tokens, parens, braces, namespace_name, exports);
    }
}

fn collect_exported_namespace_members(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    namespace_name: &str,
    exports: &mut Vec<ExportCandidate>,
) {
    collect_exported_enum_members(tokens, parens, braces, namespace_name, exports);
    let ambient_declarations =
        collect_ambient_namespace_declarations(source, tokens, parens, braces, namespace_name);
    let merged_exported_declarations = collect_merged_exported_namespace_declarations(
        source,
        tokens,
        parens,
        braces,
        namespace_name,
    );
    let merged_alias_declarations = collect_merged_namespace_alias_declarations(
        source,
        tokens,
        parens,
        braces,
        namespace_name,
        &merged_exported_declarations,
    );
    for index in 0..tokens.len() {
        if !matches!(tokens[index].text.as_str(), "namespace" | "module")
            || !is_top_level(tokens, index)
        {
            continue;
        }
        if tokens
            .get(index + 1)
            .is_none_or(|token| token.text != namespace_name)
        {
            continue;
        }
        let Some(body_open) = find_next_until(tokens, index + 2, "{", &[";"]) else {
            continue;
        };
        let Some(body_close) = braces.get(body_open).and_then(|match_index| *match_index) else {
            continue;
        };
        if tokens.get(index + 2).is_some_and(|token| token.text == ".") {
            if let Some(nested_name_index) = tokens
                .get(index + 3)
                .filter(|token| is_identifier(&token.text))
                .map(|_| index + 3)
            {
                exports.push(
                    ExportCandidate::new(
                        tokens[nested_name_index].text.clone(),
                        tokens[index].line,
                        tokens[body_close].line,
                        1,
                        index,
                        false,
                    )
                    .with_priority_group(index)
                    .with_export_assignment_priority(),
                );
            }
            continue;
        }
        let suppress_exported_namespace_members =
            exported_namespace_block_exports_suppressed(tokens, index, namespace_name);
        let declarations = collect_namespace_member_declarations(
            source, tokens, parens, braces, body_open, body_close,
        );
        let declared_namespace = is_declared_declaration(tokens, index);
        let merged_alias_declarations = if declared_namespace {
            None
        } else {
            Some(merge_namespace_alias_declarations(
                &merged_alias_declarations,
                &declarations,
            ))
        };
        if declared_namespace
            && !namespace_body_has_export_alias_list(tokens, braces, body_open, body_close)
        {
            let mut implicit_exports = declarations.values().cloned().collect::<Vec<_>>();
            implicit_exports.sort_by_key(|candidate| candidate.order);
            for mut candidate in implicit_exports {
                candidate.function_priority = tokens
                    .get(candidate.order)
                    .is_some_and(|token| token.text == "function");
                exports.push(
                    candidate
                        .with_priority_group(index)
                        .with_export_assignment_priority(),
                );
            }
        }
        if suppress_exported_namespace_members {
            continue;
        }
        let mut cursor = body_open + 1;
        while cursor < body_close {
            if tokens[cursor].text == "{" {
                cursor = braces
                    .get(cursor)
                    .and_then(|match_index| *match_index)
                    .unwrap_or(cursor)
                    + 1;
                continue;
            }
            if tokens[cursor].text != "export" {
                cursor += 1;
                continue;
            }
            if namespace_export_alias_open_index(tokens, cursor, body_close).is_some() {
                let alias_declarations = if declared_namespace {
                    &ambient_declarations
                } else {
                    merged_alias_declarations.as_ref().unwrap_or(&declarations)
                };
                if let Some(next_cursor) = collect_namespace_export_aliases(
                    tokens,
                    braces,
                    cursor,
                    body_close,
                    alias_declarations,
                    index,
                    exports,
                ) {
                    cursor = next_cursor;
                } else {
                    cursor += 1;
                }
                continue;
            }
            let Some((candidates, next_cursor)) = exported_namespace_member_candidates(
                source, tokens, parens, braces, cursor, body_close,
            ) else {
                cursor += 1;
                continue;
            };
            for candidate in candidates {
                exports.push(
                    candidate
                        .with_priority_group(index)
                        .with_export_assignment_priority(),
                );
            }
            cursor = next_cursor;
        }
    }
}

fn collect_merged_exported_namespace_declarations(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    namespace_name: &str,
) -> HashMap<String, ExportCandidate> {
    let mut declarations = HashMap::new();
    for index in 0..tokens.len() {
        if !matches!(tokens[index].text.as_str(), "namespace" | "module")
            || !is_top_level(tokens, index)
        {
            continue;
        }
        if tokens
            .get(index + 1)
            .is_none_or(|token| token.text != namespace_name)
        {
            continue;
        }
        if tokens.get(index + 2).is_some_and(|token| token.text == ".") {
            continue;
        }
        let Some(body_open) = find_next_until(tokens, index + 2, "{", &[";"]) else {
            continue;
        };
        let Some(body_close) = braces.get(body_open).and_then(|match_index| *match_index) else {
            continue;
        };
        let mut cursor = body_open + 1;
        while cursor < body_close {
            if tokens[cursor].text == "{" {
                cursor = braces
                    .get(cursor)
                    .and_then(|match_index| *match_index)
                    .unwrap_or(cursor)
                    + 1;
                continue;
            }
            if tokens[cursor].text != "export" {
                cursor += 1;
                continue;
            }
            if let Some(open_index) = namespace_export_alias_open_index(tokens, cursor, body_close)
            {
                cursor = braces
                    .get(open_index)
                    .and_then(|match_index| *match_index)
                    .map(|close_index| close_index + 1)
                    .unwrap_or(cursor + 1)
                    .min(body_close);
                continue;
            }
            let Some((candidates, next_cursor)) = exported_namespace_member_candidates(
                source, tokens, parens, braces, cursor, body_close,
            ) else {
                cursor += 1;
                continue;
            };
            for candidate in candidates {
                declarations
                    .entry(candidate.name.clone())
                    .or_insert(candidate);
            }
            cursor = next_cursor;
        }
    }
    declarations
}

fn collect_merged_namespace_alias_declarations(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    namespace_name: &str,
    direct_declarations: &HashMap<String, ExportCandidate>,
) -> HashMap<String, ExportCandidate> {
    let mut specs = Vec::new();
    for index in 0..tokens.len() {
        if !matches!(tokens[index].text.as_str(), "namespace" | "module")
            || !is_top_level(tokens, index)
        {
            continue;
        }
        if tokens
            .get(index + 1)
            .is_none_or(|token| token.text != namespace_name)
        {
            continue;
        }
        if tokens.get(index + 2).is_some_and(|token| token.text == ".") {
            continue;
        }
        let Some(body_open) = find_next_until(tokens, index + 2, "{", &[";"]) else {
            continue;
        };
        let Some(body_close) = braces.get(body_open).and_then(|match_index| *match_index) else {
            continue;
        };
        let local_declarations = collect_namespace_member_declarations(
            source, tokens, parens, braces, body_open, body_close,
        );
        let mut cursor = body_open + 1;
        while cursor < body_close {
            if tokens[cursor].text == "{" {
                cursor = braces
                    .get(cursor)
                    .and_then(|match_index| *match_index)
                    .unwrap_or(cursor)
                    + 1;
                continue;
            }
            if tokens[cursor].text != "export"
                || namespace_export_alias_open_index(tokens, cursor, body_close).is_none()
            {
                cursor += 1;
                continue;
            }
            if let Some((next_cursor, mut alias_specs)) = namespace_export_alias_specs(
                tokens,
                braces,
                cursor,
                body_close,
                &local_declarations,
            ) {
                specs.append(&mut alias_specs);
                cursor = next_cursor;
            } else {
                cursor += 1;
            }
        }
    }

    let mut declarations = direct_declarations.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for spec in &specs {
            if spec.export_name.is_empty() {
                continue;
            }
            if declarations.contains_key(&spec.export_name) {
                continue;
            }
            let target = spec
                .local_declarations
                .get(&spec.local_name)
                .or_else(|| declarations.get(&spec.local_name));
            if let Some(target) = target {
                let mut candidate = target.clone();
                candidate.name = spec.export_name.clone();
                candidate.function_priority = false;
                declarations.insert(spec.export_name.clone(), candidate);
                changed = true;
            }
        }
    }
    declarations
}

fn merge_namespace_alias_declarations(
    merged_exported_declarations: &HashMap<String, ExportCandidate>,
    local_declarations: &HashMap<String, ExportCandidate>,
) -> HashMap<String, ExportCandidate> {
    let mut declarations = merged_exported_declarations.clone();
    for (name, candidate) in local_declarations {
        declarations.insert(name.clone(), candidate.clone());
    }
    declarations
}

fn collect_exported_enum_members(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    enum_name: &str,
    exports: &mut Vec<ExportCandidate>,
) {
    let brackets = matching_tokens(tokens, "[", "]");
    for index in 0..tokens.len() {
        if tokens[index].text != "enum" || !is_top_level(tokens, index) {
            continue;
        }
        if tokens
            .get(index + 1)
            .is_none_or(|token| token.text != enum_name)
        {
            continue;
        }
        if leading_export_index_for_type_like(tokens, index).is_some()
            && has_non_export_namespace_merge(tokens, index, enum_name)
        {
            continue;
        }
        let Some(body_open) = find_next_until(tokens, index + 2, "{", &[";"]) else {
            continue;
        };
        let Some(body_close) = braces.get(body_open).and_then(|match_index| *match_index) else {
            continue;
        };
        let mut cursor = body_open + 1;
        while cursor < body_close {
            if tokens[cursor].text == "," {
                cursor += 1;
                continue;
            }
            let member_start = cursor;
            let member_end =
                enum_member_end_index(tokens, parens, braces, &brackets, member_start, body_close);
            if let Some(name) = enum_member_export_name(tokens.get(member_start)) {
                exports.push(
                    ExportCandidate::new(
                        name,
                        tokens[member_start].line,
                        tokens[member_end].line,
                        1,
                        member_start,
                        false,
                    )
                    .with_export_assignment_priority(),
                );
            }
            cursor = if member_end + 1 < body_close && tokens[member_end + 1].text == "," {
                member_end + 2
            } else {
                member_end + 1
            };
        }
    }
}

fn has_non_export_namespace_merge(tokens: &[Token], declaration_index: usize, name: &str) -> bool {
    for index in 0..tokens.len() {
        if index == declaration_index
            || !matches!(tokens[index].text.as_str(), "namespace" | "module")
            || !is_top_level(tokens, index)
        {
            continue;
        }
        if tokens.get(index + 1).is_none_or(|token| token.text != name) {
            continue;
        }
        if tokens.get(index + 2).is_some_and(|token| token.text == ".") {
            continue;
        }
        if leading_export_index(tokens, index).is_none() {
            return true;
        }
    }
    false
}

fn exported_namespace_block_exports_suppressed(
    tokens: &[Token],
    namespace_index: usize,
    namespace_name: &str,
) -> bool {
    if leading_export_index(tokens, namespace_index).is_none() {
        return false;
    }
    has_non_export_namespace_merge(tokens, namespace_index, namespace_name)
        || has_non_export_value_merge(tokens, namespace_index, namespace_name)
}

fn has_non_export_value_merge(
    tokens: &[Token],
    namespace_index: usize,
    namespace_name: &str,
) -> bool {
    for index in 0..tokens.len() {
        if index == namespace_index || !is_top_level(tokens, index) {
            continue;
        }
        if !matches!(tokens[index].text.as_str(), "function" | "class" | "enum") {
            continue;
        }
        let Some(name_index) = declaration_name_index(tokens, index) else {
            continue;
        };
        if tokens[name_index].text != namespace_name {
            continue;
        }
        let export_index = leading_export_index_for_type_like(tokens, index);
        if export_index.is_none() {
            return true;
        }
        if export_index.is_some_and(|export_index| is_default_export(tokens, export_index, index)) {
            return true;
        }
    }
    false
}

fn declaration_name_index(tokens: &[Token], declaration_index: usize) -> Option<usize> {
    match tokens.get(declaration_index)?.text.as_str() {
        "function" => function_name_index(tokens, declaration_index),
        "class" => class_name_index(tokens, declaration_index),
        "enum" => next_identifier(tokens, declaration_index + 1),
        _ => None,
    }
}

fn enum_member_end_index(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    start: usize,
    body_close: usize,
) -> usize {
    let mut cursor = start;
    let mut last = start;
    while cursor < body_close {
        match tokens[cursor].text.as_str() {
            "," => return last,
            "(" => {
                if let Some(close_index) = parens.get(cursor).and_then(|match_index| *match_index) {
                    if close_index < body_close {
                        last = close_index;
                        cursor = close_index + 1;
                        continue;
                    }
                }
            }
            "{" => {
                if let Some(close_index) = braces.get(cursor).and_then(|match_index| *match_index) {
                    if close_index < body_close {
                        last = close_index;
                        cursor = close_index + 1;
                        continue;
                    }
                }
            }
            "[" => {
                if let Some(close_index) = brackets.get(cursor).and_then(|match_index| *match_index)
                {
                    if close_index < body_close {
                        last = close_index;
                        cursor = close_index + 1;
                        continue;
                    }
                }
            }
            _ => {}
        }
        last = cursor;
        cursor += 1;
    }
    last
}

fn enum_member_export_name(token: Option<&Token>) -> Option<String> {
    let text = &token?.text;
    if is_identifier(text) || is_numeric_literal(text) {
        return Some(text.clone());
    }
    if is_string_literal(text) {
        return Some(unquote_string_literal(text));
    }
    None
}

fn unquote_string_literal(value: &str) -> String {
    let mut chars = value.chars();
    let Some(quote) = chars.next() else {
        return String::new();
    };
    let mut output = String::new();
    let mut escaped = false;
    for character in chars {
        if escaped {
            output.push(match character {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'b' => '\u{0008}',
                'f' => '\u{000c}',
                'v' => '\u{000b}',
                _ => character,
            });
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == quote {
            break;
        }
        output.push(character);
    }
    output
}

fn collect_ambient_namespace_declarations(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    namespace_name: &str,
) -> HashMap<String, ExportCandidate> {
    let mut declarations = HashMap::new();
    for index in 0..tokens.len() {
        if !matches!(tokens[index].text.as_str(), "namespace" | "module")
            || !is_top_level(tokens, index)
            || !is_declared_declaration(tokens, index)
        {
            continue;
        }
        if tokens
            .get(index + 1)
            .is_none_or(|token| token.text != namespace_name)
        {
            continue;
        }
        if tokens.get(index + 2).is_some_and(|token| token.text == ".") {
            continue;
        }
        let Some(body_open) = find_next_until(tokens, index + 2, "{", &[";"]) else {
            continue;
        };
        let Some(body_close) = braces.get(body_open).and_then(|match_index| *match_index) else {
            continue;
        };
        for candidate in collect_namespace_member_declarations(
            source, tokens, parens, braces, body_open, body_close,
        )
        .into_values()
        {
            declarations
                .entry(candidate.name.clone())
                .or_insert(candidate);
        }
    }
    declarations
}

fn namespace_body_has_export_alias_list(
    tokens: &[Token],
    braces: &[Option<usize>],
    body_open: usize,
    body_close: usize,
) -> bool {
    let mut cursor = body_open + 1;
    while cursor < body_close {
        if tokens[cursor].text == "{" {
            cursor = braces
                .get(cursor)
                .and_then(|match_index| *match_index)
                .unwrap_or(cursor)
                + 1;
            continue;
        }
        if namespace_export_alias_open_index(tokens, cursor, body_close).is_some() {
            return true;
        }
        cursor += 1;
    }
    false
}

fn collect_namespace_member_declarations(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    body_open: usize,
    body_close: usize,
) -> HashMap<String, ExportCandidate> {
    let mut declarations = HashMap::new();
    let mut cursor = body_open + 1;
    while cursor < body_close {
        if tokens[cursor].text == "{" {
            cursor = braces
                .get(cursor)
                .and_then(|match_index| *match_index)
                .unwrap_or(cursor)
                + 1;
            continue;
        }
        let mut declaration_index = cursor;
        if tokens[declaration_index].text == "export" {
            if namespace_export_alias_open_index(tokens, declaration_index, body_close).is_some() {
                cursor =
                    token_index_after_line_or_block(tokens, braces, declaration_index, body_close);
                continue;
            }
            declaration_index += 1;
        }
        while declaration_index < body_close
            && is_leading_declaration_modifier(&tokens[declaration_index].text)
        {
            declaration_index += 1;
        }
        let Some((candidates, next_cursor)) = namespace_member_declaration_candidates(
            source,
            tokens,
            parens,
            braces,
            declaration_index,
            body_close,
        ) else {
            cursor += 1;
            continue;
        };
        for candidate in candidates {
            declarations
                .entry(candidate.name.clone())
                .or_insert(candidate);
        }
        cursor = next_cursor;
    }
    declarations
}

fn namespace_member_declaration_candidates(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    declaration_index: usize,
    body_close: usize,
) -> Option<(Vec<ExportCandidate>, usize)> {
    match tokens.get(declaration_index)?.text.as_str() {
        "function" => {
            let name_index = function_name_index(tokens, declaration_index)?;
            let (start, end, complexity) = declaration_range(
                source,
                tokens,
                parens,
                braces,
                declaration_index,
                name_index,
            )?;
            Some((
                vec![ExportCandidate::new(
                    tokens[name_index].text.clone(),
                    start,
                    end,
                    complexity,
                    declaration_index,
                    false,
                )],
                token_index_after_line_or_block(tokens, braces, declaration_index, body_close),
            ))
        }
        "class" => {
            let name_index = class_name_index(tokens, declaration_index)?;
            let body_open = find_next_until(tokens, name_index + 1, "{", &[";"])?;
            let body_end = braces
                .get(body_open)
                .and_then(|match_index| *match_index)
                .unwrap_or(body_open);
            Some((
                vec![ExportCandidate::new(
                    tokens[name_index].text.clone(),
                    tokens[declaration_index].line,
                    tokens[body_end].line,
                    complexity_between(tokens, declaration_index, body_end, None),
                    declaration_index,
                    false,
                )],
                (body_end + 1).min(body_close),
            ))
        }
        "const" | "let" | "var" => namespace_variable_declaration_candidates(
            tokens,
            parens,
            braces,
            declaration_index,
            body_close,
        ),
        "interface" | "type" | "enum" | "namespace" | "module" => {
            let name_index = next_identifier(tokens, declaration_index + 1)?;
            let end_index = type_like_declaration_end(tokens, braces, declaration_index);
            Some((
                vec![ExportCandidate::new(
                    tokens[name_index].text.clone(),
                    tokens[declaration_index].line,
                    tokens[end_index].line,
                    1,
                    declaration_index,
                    false,
                )],
                (end_index + 1).min(body_close),
            ))
        }
        _ => None,
    }
}

fn namespace_variable_declaration_candidates(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    declaration_index: usize,
    body_close: usize,
) -> Option<(Vec<ExportCandidate>, usize)> {
    let mut candidates = Vec::new();
    let mut cursor = declaration_index + 1;
    let brackets = matching_tokens(tokens, "[", "]");
    let binding_parens = matching_tokens(tokens, "(", ")");
    while cursor < body_close {
        match tokens[cursor].text.as_str() {
            "," => {
                cursor += 1;
                continue;
            }
            ";" | "}" => break,
            _ => {}
        }
        if matches!(tokens[cursor].text.as_str(), "{" | "[") {
            let pattern_open = cursor;
            let pattern_close = if tokens[pattern_open].text == "{" {
                braces
                    .get(pattern_open)
                    .and_then(|match_index| *match_index)
            } else {
                brackets
                    .get(pattern_open)
                    .and_then(|match_index| *match_index)
            };
            let Some(pattern_close) = pattern_close.filter(|close| *close < body_close) else {
                break;
            };
            let mut bindings = Vec::new();
            collect_binding_pattern_names(
                tokens,
                braces,
                &brackets,
                &binding_parens,
                pattern_open,
                &mut bindings,
            );
            for (name, line, order) in bindings {
                candidates.push(ExportCandidate::new(name, line, line, 1, order, false));
            }
            if let Some(eq_index) =
                find_next_until(tokens, pattern_close + 1, "=", &[",", ";", "}"])
            {
                let Some(separator_index) =
                    variable_declaration_separator_after_initializer(tokens, eq_index)
                else {
                    return Some((candidates, body_close));
                };
                if separator_index >= body_close {
                    return Some((candidates, body_close));
                }
                if tokens[separator_index].text == "," {
                    cursor = separator_index + 1;
                    continue;
                }
                return Some((candidates, (separator_index + 1).min(body_close)));
            }
            let separator_index = find_next_until(tokens, pattern_close + 1, ",", &[";", "}"])
                .or_else(|| find_next_until(tokens, pattern_close + 1, ";", &["}"]))
                .unwrap_or(body_close);
            if separator_index >= body_close || tokens[separator_index].text != "," {
                return Some((candidates, (separator_index + 1).min(body_close)));
            }
            cursor = separator_index + 1;
            continue;
        }
        if !is_identifier(&tokens[cursor].text) {
            break;
        }
        let name_index = cursor;
        if let Some(eq_index) = variable_declaration_initializer_eq_index(
            tokens,
            parens,
            braces,
            &brackets,
            name_index + 1,
            body_close,
        ) {
            let (start, end, complexity) = variable_initializer_range(
                tokens, parens, braces, eq_index, None,
            )
            .unwrap_or_else(|| {
                (
                    tokens[name_index].line,
                    variable_declaration_end_line(tokens, eq_index),
                    1,
                )
            });
            candidates.push(ExportCandidate::new(
                tokens[name_index].text.clone(),
                start,
                end,
                complexity,
                name_index,
                false,
            ));
            let Some(separator_index) =
                variable_declaration_separator_after_initializer(tokens, eq_index)
            else {
                return Some((candidates, body_close));
            };
            if separator_index >= body_close {
                return Some((candidates, body_close));
            }
            if tokens[separator_index].text == "," {
                cursor = separator_index + 1;
                continue;
            }
            return Some((candidates, (separator_index + 1).min(body_close)));
        }
        let end_index = variable_declaration_without_initializer_end_index(
            tokens, parens, braces, &brackets, name_index, body_close,
        );
        let separator_index = end_index + 1;
        candidates.push(ExportCandidate::new(
            tokens[name_index].text.clone(),
            tokens[name_index].line,
            tokens[end_index].line,
            1,
            name_index,
            false,
        ));
        if separator_index >= body_close || tokens[separator_index].text != "," {
            return Some((candidates, (separator_index + 1).min(body_close)));
        }
        cursor = separator_index + 1;
    }
    if candidates.is_empty() {
        None
    } else {
        Some((candidates, cursor.min(body_close)))
    }
}

fn collect_binding_pattern_names(
    tokens: &[Token],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    parens: &[Option<usize>],
    open_index: usize,
    bindings: &mut Vec<(String, u32, usize)>,
) {
    match tokens.get(open_index).map(|token| token.text.as_str()) {
        Some("{") => {
            collect_object_binding_names(tokens, braces, brackets, parens, open_index, bindings)
        }
        Some("[") => {
            collect_array_binding_names(tokens, braces, brackets, parens, open_index, bindings)
        }
        _ => {}
    }
}

fn collect_object_binding_names(
    tokens: &[Token],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    parens: &[Option<usize>],
    open_index: usize,
    bindings: &mut Vec<(String, u32, usize)>,
) {
    let Some(close_index) = braces.get(open_index).and_then(|match_index| *match_index) else {
        return;
    };
    let mut cursor = open_index + 1;
    while cursor < close_index {
        if tokens[cursor].text == "," {
            cursor += 1;
            continue;
        }
        let element_end =
            binding_element_end(tokens, braces, brackets, parens, cursor, close_index);
        if let Some(rest_width) = binding_rest_width(tokens, cursor, element_end) {
            collect_binding_target_names(
                tokens,
                braces,
                brackets,
                parens,
                cursor + rest_width,
                element_end,
                bindings,
            );
        } else if let Some(colon_index) =
            binding_top_level_text(tokens, braces, brackets, parens, cursor, element_end, ":")
        {
            collect_binding_target_names(
                tokens,
                braces,
                brackets,
                parens,
                colon_index + 1,
                element_end,
                bindings,
            );
        } else if is_identifier(&tokens[cursor].text) {
            bindings.push((tokens[cursor].text.clone(), tokens[cursor].line, cursor));
        }
        cursor = if element_end < close_index {
            element_end + 1
        } else {
            element_end
        };
    }
}

fn collect_array_binding_names(
    tokens: &[Token],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    parens: &[Option<usize>],
    open_index: usize,
    bindings: &mut Vec<(String, u32, usize)>,
) {
    let Some(close_index) = brackets
        .get(open_index)
        .and_then(|match_index| *match_index)
    else {
        return;
    };
    let mut cursor = open_index + 1;
    while cursor < close_index {
        if tokens[cursor].text == "," {
            cursor += 1;
            continue;
        }
        let element_end =
            binding_element_end(tokens, braces, brackets, parens, cursor, close_index);
        collect_binding_target_names(
            tokens,
            braces,
            brackets,
            parens,
            cursor,
            element_end,
            bindings,
        );
        cursor = if element_end < close_index {
            element_end + 1
        } else {
            element_end
        };
    }
}

fn collect_binding_target_names(
    tokens: &[Token],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    parens: &[Option<usize>],
    mut start: usize,
    end: usize,
    bindings: &mut Vec<(String, u32, usize)>,
) {
    while let Some(rest_width) = binding_rest_width(tokens, start, end) {
        start += rest_width;
    }
    let Some(token) = tokens.get(start) else {
        return;
    };
    match token.text.as_str() {
        "{" | "[" => {
            collect_binding_pattern_names(tokens, braces, brackets, parens, start, bindings)
        }
        text if is_identifier(text) => {
            bindings.push((text.to_string(), token.line, start));
        }
        _ => {}
    }
}

fn binding_rest_width(tokens: &[Token], index: usize, end: usize) -> Option<usize> {
    if index >= end {
        return None;
    }
    if tokens[index].text == "..." {
        return Some(1);
    }
    if index + 2 < end
        && tokens[index].text == "."
        && tokens[index + 1].text == "."
        && tokens[index + 2].text == "."
    {
        return Some(3);
    }
    None
}

fn binding_element_end(
    tokens: &[Token],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    parens: &[Option<usize>],
    start: usize,
    limit: usize,
) -> usize {
    let mut cursor = start;
    while cursor < limit {
        if tokens[cursor].text == "," {
            return cursor;
        }
        if tokens[cursor].text == "{" {
            if let Some(close_index) = braces.get(cursor).and_then(|match_index| *match_index) {
                if close_index < limit {
                    cursor = close_index + 1;
                    continue;
                }
            }
        }
        if tokens[cursor].text == "[" {
            if let Some(close_index) = brackets.get(cursor).and_then(|match_index| *match_index) {
                if close_index < limit {
                    cursor = close_index + 1;
                    continue;
                }
            }
        }
        if tokens[cursor].text == "(" {
            if let Some(close_index) = parens.get(cursor).and_then(|match_index| *match_index) {
                if close_index < limit {
                    cursor = close_index + 1;
                    continue;
                }
            }
        }
        cursor += 1;
    }
    limit
}

fn binding_top_level_text(
    tokens: &[Token],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    parens: &[Option<usize>],
    start: usize,
    end: usize,
    text: &str,
) -> Option<usize> {
    let mut cursor = start;
    while cursor < end {
        if tokens[cursor].text == text {
            return Some(cursor);
        }
        if tokens[cursor].text == "{" {
            if let Some(close_index) = braces.get(cursor).and_then(|match_index| *match_index) {
                if close_index < end {
                    cursor = close_index + 1;
                    continue;
                }
            }
        }
        if tokens[cursor].text == "[" {
            if let Some(close_index) = brackets.get(cursor).and_then(|match_index| *match_index) {
                if close_index < end {
                    cursor = close_index + 1;
                    continue;
                }
            }
        }
        if tokens[cursor].text == "(" {
            if let Some(close_index) = parens.get(cursor).and_then(|match_index| *match_index) {
                if close_index < end {
                    cursor = close_index + 1;
                    continue;
                }
            }
        }
        cursor += 1;
    }
    None
}

fn collect_namespace_export_aliases(
    tokens: &[Token],
    braces: &[Option<usize>],
    export_index: usize,
    body_close: usize,
    declarations: &HashMap<String, ExportCandidate>,
    priority_group: usize,
    exports: &mut Vec<ExportCandidate>,
) -> Option<usize> {
    let (next_cursor, specs) =
        namespace_export_alias_specs(tokens, braces, export_index, body_close, declarations)?;
    for spec in specs {
        if let Some(declaration) = declarations.get(&spec.local_name) {
            let mut candidate = declaration.clone();
            candidate.name = spec.export_name;
            candidate.order = spec.order;
            candidate.function_priority = false;
            exports.push(
                candidate
                    .with_priority_group(priority_group)
                    .with_export_assignment_priority(),
            );
        }
    }
    Some(next_cursor)
}

fn namespace_export_alias_specs(
    tokens: &[Token],
    braces: &[Option<usize>],
    export_index: usize,
    body_close: usize,
    local_declarations: &HashMap<String, ExportCandidate>,
) -> Option<(usize, Vec<NamespaceAliasSpec>)> {
    let open_index = namespace_export_alias_open_index(tokens, export_index, body_close)?;
    let close_index = braces
        .get(open_index)
        .and_then(|match_index| *match_index)?;
    if close_index > body_close {
        return None;
    }
    if export_alias_list_has_quoted_export_name(tokens, open_index + 1, close_index) {
        return Some(((close_index + 1).min(body_close), Vec::new()));
    }
    let mut specs = Vec::new();
    let mut cursor = open_index + 1;
    while cursor < close_index {
        if tokens[cursor].text == "," {
            cursor += 1;
            continue;
        }
        if tokens[cursor].text == "type" {
            cursor += 1;
        }
        if !tokens
            .get(cursor)
            .is_some_and(|token| is_identifier(&token.text))
        {
            cursor += 1;
            continue;
        }
        let local_name = tokens[cursor].text.clone();
        let mut export_name = local_name.clone();
        let mut next_cursor = cursor + 1;
        let mut stop_after_specifier = false;
        if tokens
            .get(next_cursor)
            .is_some_and(|token| token.text == "as")
        {
            if tokens
                .get(next_cursor + 1)
                .is_some_and(|token| is_identifier(&token.text))
            {
                export_name = tokens[next_cursor + 1].text.clone();
            } else {
                let (recovered_name, stop_after_alias) =
                    invalid_export_alias_target_recovery(tokens, next_cursor + 1, close_index);
                export_name = recovered_name;
                stop_after_specifier = stop_after_alias;
            }
            next_cursor = export_specifier_next_cursor(tokens, next_cursor, close_index);
        }
        specs.push(NamespaceAliasSpec {
            local_name,
            export_name,
            local_declarations: local_declarations.clone(),
            order: cursor,
        });
        if stop_after_specifier {
            break;
        }
        cursor = next_cursor;
        while cursor < close_index && tokens[cursor].text != "," {
            cursor += 1;
        }
    }
    Some(((close_index + 1).min(body_close), specs))
}

fn namespace_export_alias_open_index(
    tokens: &[Token],
    export_index: usize,
    body_close: usize,
) -> Option<usize> {
    if export_index >= body_close || tokens.get(export_index)?.text != "export" {
        return None;
    }
    let direct_open = export_index + 1;
    if direct_open < body_close
        && tokens
            .get(direct_open)
            .is_some_and(|token| token.text == "{")
    {
        return Some(direct_open);
    }
    let type_open = export_index + 2;
    if direct_open < body_close
        && type_open < body_close
        && tokens
            .get(direct_open)
            .is_some_and(|token| token.text == "type")
        && tokens.get(type_open).is_some_and(|token| token.text == "{")
    {
        return Some(type_open);
    }
    None
}

fn exported_namespace_member_candidates(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    export_index: usize,
    body_close: usize,
) -> Option<(Vec<ExportCandidate>, usize)> {
    let mut declaration_index = export_index + 1;
    while declaration_index < body_close
        && is_leading_declaration_modifier(&tokens[declaration_index].text)
    {
        declaration_index += 1;
    }
    match tokens.get(declaration_index)?.text.as_str() {
        "function" => {
            let name_index = function_name_index(tokens, declaration_index)?;
            let (start, end, complexity) = declaration_range(
                source,
                tokens,
                parens,
                braces,
                declaration_index,
                name_index,
            )?;
            Some((
                vec![ExportCandidate::new(
                    tokens[name_index].text.clone(),
                    start,
                    end,
                    complexity,
                    declaration_index,
                    true,
                )],
                token_index_after_line_or_block(tokens, braces, declaration_index, body_close),
            ))
        }
        "class" => {
            let name_index = class_name_index(tokens, declaration_index)?;
            let body_open = find_next_until(tokens, name_index + 1, "{", &[";"])?;
            let body_end = braces
                .get(body_open)
                .and_then(|match_index| *match_index)
                .unwrap_or(body_open);
            Some((
                vec![ExportCandidate::new(
                    tokens[name_index].text.clone(),
                    tokens[export_index].line,
                    tokens[body_end].line,
                    complexity_between(tokens, declaration_index, body_end, None),
                    declaration_index,
                    false,
                )],
                (body_end + 1).min(body_close),
            ))
        }
        "const" | "let" | "var" => namespace_variable_declaration_candidates(
            tokens,
            parens,
            braces,
            declaration_index,
            body_close,
        ),
        "interface" | "type" | "enum" | "namespace" | "module" => {
            let name_index = next_identifier(tokens, declaration_index + 1)?;
            let end_index = type_like_declaration_end(tokens, braces, declaration_index);
            Some((
                vec![ExportCandidate::new(
                    tokens[name_index].text.clone(),
                    tokens[export_index].line,
                    tokens[end_index].line,
                    1,
                    declaration_index,
                    false,
                )],
                (end_index + 1).min(body_close),
            ))
        }
        _ => None,
    }
}

fn token_index_after_line_or_block(
    tokens: &[Token],
    braces: &[Option<usize>],
    start: usize,
    limit: usize,
) -> usize {
    let line = tokens[start].line;
    let mut cursor = start;
    while cursor < limit {
        if tokens[cursor].text == "{" {
            return braces
                .get(cursor)
                .and_then(|match_index| *match_index)
                .map(|index| index + 1)
                .unwrap_or(cursor + 1)
                .min(limit);
        }
        if tokens[cursor].text == ";" || tokens[cursor].line != line {
            return (cursor + 1).min(limit);
        }
        cursor += 1;
    }
    limit
}

fn collect_type_like_export_anchors(
    tokens: &[Token],
    braces: &[Option<usize>],
    exports: &mut Vec<ExportCandidate>,
    declarations: &mut HashMap<String, ExportCandidate>,
) {
    for index in 0..tokens.len() {
        if !matches!(
            tokens[index].text.as_str(),
            "interface" | "type" | "enum" | "namespace" | "module"
        ) || !is_top_level(tokens, index)
        {
            continue;
        }
        if tokens[index].text == "type"
            && tokens.get(index + 1).is_some_and(|token| token.text == "{")
        {
            continue;
        }
        if tokens[index].text == "type" && index > 0 && tokens[index - 1].text == "import" {
            continue;
        }
        let export_index = leading_export_index_for_type_like(tokens, index);
        let default_export =
            export_index.is_some_and(|export_index| is_default_export(tokens, export_index, index));
        if default_export && matches!(tokens[index].text.as_str(), "type" | "namespace" | "module")
        {
            continue;
        }
        let Some(name_index) = next_identifier(tokens, index + 1) else {
            continue;
        };
        let name = tokens[name_index].text.clone();
        let end_index = type_like_declaration_end(tokens, braces, index);
        let order = if tokens[index].text == "enum" {
            end_index
        } else {
            index
        };
        let candidate = ExportCandidate::new(
            name.clone(),
            tokens[export_index.unwrap_or(index)].line,
            tokens[end_index].line,
            1,
            order,
            false,
        );
        declarations
            .entry(name)
            .or_insert_with(|| candidate.clone());
        if export_index.is_some() && !default_export {
            exports.push(candidate);
        }
    }
}

fn collect_block_anchors(builder: &mut AnchorBuilder, mut blocks: Vec<BlockCandidate>) {
    blocks.sort_by_key(|block| block.order);
    for block in blocks {
        builder.add(
            block.anchor,
            block.start,
            block.end,
            block.complexity,
            AnchorKind::Block,
        );
    }
}

fn collect_if_block_candidates(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    line_offset: u32,
    order_offset: usize,
    blocks: &mut Vec<BlockCandidate>,
) {
    for index in 0..tokens.len() {
        if tokens[index].text != "if" {
            continue;
        }
        let Some(paren_open) = find_next_text(tokens, index + 1, "(") else {
            continue;
        };
        let Some(paren_close) = parens.get(paren_open).and_then(|match_index| *match_index) else {
            continue;
        };
        let Some(end_index) = if_statement_end(tokens, parens, braces, paren_close + 1) else {
            continue;
        };
        let condition = source
            .get(tokens[paren_open].end..tokens[paren_close].start)
            .unwrap_or("");
        let normalized = normalize_if_condition(condition);
        if normalized.is_empty() {
            continue;
        }
        let complexity = complexity_between(tokens, index, end_index, Some(index));
        if complexity >= 3 {
            blocks.push(BlockCandidate::new(
                format!("block:if_{normalized}"),
                tokens[index].line + line_offset,
                tokens[end_index].line + line_offset,
                complexity,
                order_offset + tokens[index].start,
            ));
        }
    }
}

fn collect_template_if_block_candidates(source: &str, blocks: &mut Vec<BlockCandidate>) {
    collect_template_if_block_candidates_with_offset(source, 0, 0, blocks);
}

fn collect_template_if_block_candidates_with_offset(
    source: &str,
    source_offset: usize,
    line_offset: u32,
    blocks: &mut Vec<BlockCandidate>,
) {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut line = line_offset + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                line += 1;
                index += 1;
            }
            b'\'' | b'"' => skip_quoted_source(bytes, &mut index, &mut line),
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() {
                    if bytes[index] == b'\n' {
                        line += 1;
                    }
                    if bytes[index] == b'*' && bytes[index + 1] == b'/' {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            b'`' => scan_template_literal_for_if_blocks(
                source,
                source_offset,
                &mut index,
                &mut line,
                blocks,
            ),
            _ => index += 1,
        }
    }
}

fn scan_template_literal_for_if_blocks(
    source: &str,
    source_offset: usize,
    index: &mut usize,
    line: &mut u32,
    blocks: &mut Vec<BlockCandidate>,
) {
    let bytes = source.as_bytes();
    *index += 1;
    while *index < bytes.len() {
        match bytes[*index] {
            b'\n' => {
                *line += 1;
                *index += 1;
            }
            b'\\' => {
                *index = (*index + 2).min(bytes.len());
            }
            b'`' => {
                *index += 1;
                break;
            }
            b'$' if bytes.get(*index + 1) == Some(&b'{') => {
                let expression_start = *index + 2;
                let expression_start_line = *line;
                if let Some(expression_end) = template_expression_end(source, expression_start) {
                    collect_template_expression_if_blocks(
                        source,
                        expression_start,
                        expression_end,
                        expression_start_line,
                        source_offset,
                        blocks,
                    );
                    *line += count_newlines(&source[*index..=expression_end]);
                    *index = expression_end + 1;
                } else if source.as_bytes()[expression_start..].contains(&b'`') {
                    collect_template_expression_if_blocks(
                        source,
                        expression_start,
                        source.len(),
                        expression_start_line,
                        source_offset,
                        blocks,
                    );
                    *line += count_newlines(&source[*index..]);
                    *index = source.len();
                } else {
                    *index += 2;
                }
            }
            _ => *index += 1,
        }
    }
}

fn collect_template_expression_if_blocks(
    source: &str,
    expression_start: usize,
    expression_end: usize,
    expression_start_line: u32,
    source_offset: usize,
    blocks: &mut Vec<BlockCandidate>,
) {
    let expression = &source[expression_start..expression_end];
    let tokens = tokenize(expression);
    let parens = matching_tokens(&tokens, "(", ")");
    let braces = matching_tokens(&tokens, "{", "}");
    collect_if_block_candidates(
        expression,
        &tokens,
        &parens,
        &braces,
        expression_start_line.saturating_sub(1),
        source_offset + expression_start,
        blocks,
    );
    collect_template_if_block_candidates_with_offset(
        expression,
        source_offset + expression_start,
        expression_start_line.saturating_sub(1),
        blocks,
    );
}

fn template_expression_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut index = start;
    let mut brace_depth = 0_i32;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' => skip_quoted_source_no_line(bytes, &mut index),
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() {
                    if bytes[index] == b'*' && bytes[index + 1] == b'/' {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            b'/' if template_regex_literal_allowed_before(source, start, index) => {
                if let Some(next_index) = regex_literal_end(bytes, index) {
                    index = next_index;
                } else {
                    index += 1;
                }
            }
            b'`' => skip_template_literal_no_line(source, &mut index),
            b'{' => {
                brace_depth += 1;
                index += 1;
            }
            b'}' if brace_depth == 0 => return Some(index),
            b'}' => {
                brace_depth -= 1;
                index += 1;
            }
            _ => index += 1,
        }
    }
    None
}

fn template_regex_literal_allowed_before(
    source: &str,
    expression_start: usize,
    slash_index: usize,
) -> bool {
    let bytes = source.as_bytes();
    let mut cursor = slash_index;
    while cursor > expression_start {
        let previous = cursor - 1;
        if bytes[previous].is_ascii_whitespace() {
            cursor = previous;
            continue;
        }
        if bytes[previous] == b')' || bytes[previous] == b']' || bytes[previous] == b'}' {
            return false;
        }
        if matches!(bytes[previous], b'\'' | b'"' | b'`') {
            return false;
        }
        if bytes[previous].is_ascii_alphanumeric() || matches!(bytes[previous], b'_' | b'$') {
            let mut start = previous;
            while start > expression_start
                && (bytes[start - 1].is_ascii_alphanumeric()
                    || matches!(bytes[start - 1], b'_' | b'$'))
            {
                start -= 1;
            }
            return regex_literal_allowed_after(Some(&source[start..=previous]));
        }
        let token = match bytes[previous] {
            b'>' if previous > expression_start && bytes[previous - 1] == b'=' => "=>",
            b'&' if previous > expression_start && bytes[previous - 1] == b'&' => "&&",
            b'|' if previous > expression_start && bytes[previous - 1] == b'|' => "||",
            b'?' if previous > expression_start && bytes[previous - 1] == b'?' => "??",
            b'(' => "(",
            b'[' => "[",
            b'{' => "{",
            b'=' => "=",
            b',' => ",",
            b':' => ":",
            b';' => ";",
            b'!' => "!",
            b'?' => "?",
            _ => return false,
        };
        return regex_literal_allowed_after(Some(token));
    }
    true
}

fn skip_quoted_source(bytes: &[u8], index: &mut usize, line: &mut u32) {
    let quote = bytes[*index];
    *index += 1;
    while *index < bytes.len() {
        if bytes[*index] == b'\n' {
            *line += 1;
        }
        if bytes[*index] == b'\\' {
            *index = (*index + 2).min(bytes.len());
            continue;
        }
        if bytes[*index] == quote {
            *index += 1;
            break;
        }
        *index += 1;
    }
}

fn skip_quoted_source_no_line(bytes: &[u8], index: &mut usize) {
    let quote = bytes[*index];
    *index += 1;
    while *index < bytes.len() {
        if bytes[*index] == b'\\' {
            *index = (*index + 2).min(bytes.len());
            continue;
        }
        if bytes[*index] == quote {
            *index += 1;
            break;
        }
        *index += 1;
    }
}

fn skip_template_literal_no_line(source: &str, index: &mut usize) {
    let bytes = source.as_bytes();
    *index += 1;
    while *index < bytes.len() {
        match bytes[*index] {
            b'\\' => *index = (*index + 2).min(bytes.len()),
            b'`' => {
                *index += 1;
                break;
            }
            b'$' if bytes.get(*index + 1) == Some(&b'{') => {
                if let Some(expression_end) = template_expression_end(source, *index + 2) {
                    *index = expression_end + 1;
                } else {
                    *index += 2;
                }
            }
            _ => *index += 1,
        }
    }
}

fn skip_template_literal_source(source: &str, index: &mut usize, line: &mut u32) {
    let bytes = source.as_bytes();
    *index += 1;
    while *index < bytes.len() {
        match bytes[*index] {
            b'\n' => {
                *line += 1;
                *index += 1;
            }
            b'\\' => {
                if bytes.get(*index + 1) == Some(&b'\n') {
                    *line += 1;
                }
                *index = (*index + 2).min(bytes.len());
            }
            b'`' => {
                *index += 1;
                break;
            }
            b'$' if bytes.get(*index + 1) == Some(&b'{') => {
                if let Some(expression_end) = template_expression_end(source, *index + 2) {
                    *line += count_newlines(&source[*index..=expression_end]);
                    *index = expression_end + 1;
                } else {
                    *index += 2;
                }
            }
            _ => *index += 1,
        }
    }
}

fn unclosed_template_expression_resume_index(source: &str, template_start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut index = template_start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                index = (index + 2).min(bytes.len());
            }
            b'`' => return None,
            b'$' if bytes.get(index + 1) == Some(&b'{') => {
                let expression_start = index + 2;
                if template_expression_end(source, expression_start).is_none() {
                    return Some(expression_start);
                }
                index = template_expression_end(source, expression_start)? + 1;
            }
            _ => index += 1,
        }
    }
    None
}

fn count_newlines(source: &str) -> u32 {
    source
        .as_bytes()
        .iter()
        .filter(|byte| **byte == b'\n')
        .count() as u32
}

fn record_export_function(
    source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    function_index: usize,
    name_index: usize,
    exports: &mut Vec<ExportCandidate>,
) {
    let Some(export_index) = leading_export_index(tokens, function_index) else {
        return;
    };
    let name = if tokens
        .get(export_index + 1)
        .is_some_and(|token| token.text == "default")
    {
        "default".to_string()
    } else {
        tokens[name_index].text.clone()
    };
    let Some((start, end, complexity)) =
        declaration_range(source, tokens, parens, braces, function_index, name_index)
    else {
        return;
    };
    exports.push(ExportCandidate::new(
        name,
        start,
        end,
        complexity,
        function_index,
        true,
    ));
}

fn record_export_class(
    tokens: &[Token],
    class_index: usize,
    class_name: Option<&str>,
    start: u32,
    end: u32,
    complexity: u32,
    exports: &mut Vec<ExportCandidate>,
) {
    if let Some(export_index) = leading_export_index(tokens, class_index) {
        exports.push(ExportCandidate::new(
            if is_default_export(tokens, export_index, class_index) {
                "default".to_string()
            } else {
                let Some(class_name) = class_name else {
                    return;
                };
                class_name.to_string()
            },
            start,
            end,
            complexity,
            class_index,
            false,
        ));
    }
}

fn record_export_variable(
    tokens: &[Token],
    var_index: usize,
    declaration: ExportCandidate,
    exports: &mut Vec<ExportCandidate>,
) {
    if leading_export_index(tokens, var_index).is_some() {
        exports.push(declaration);
    }
}

fn declaration_range(
    _source: &str,
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    function_index: usize,
    name_index: usize,
) -> Option<(u32, u32, u32)> {
    let Some(body_open) = function_body_open(tokens, name_index, parens) else {
        let end = find_next_until(tokens, name_index + 1, ";", &[])
            .map(|index| tokens[index].line)
            .unwrap_or(tokens[name_index].line);
        return Some((tokens[function_index].line, end, 1));
    };
    let body_close = braces.get(body_open).and_then(|match_index| *match_index)?;
    let start_index = decorated_start_index(
        tokens,
        leading_export_index(tokens, function_index).unwrap_or(function_index),
    );
    Some((
        tokens[start_index].line,
        tokens[body_close].line,
        complexity_between(tokens, function_index, body_close, None),
    ))
}

fn function_name_index(tokens: &[Token], function_index: usize) -> Option<usize> {
    let next = tokens.get(function_index + 1)?;
    if next.text == "*" {
        return tokens
            .get(function_index + 2)
            .filter(|token| is_identifier(&token.text))
            .map(|_| function_index + 2);
    }
    if is_identifier(&next.text) {
        Some(function_index + 1)
    } else {
        None
    }
}

fn class_name_index(tokens: &[Token], class_index: usize) -> Option<usize> {
    tokens
        .get(class_index + 1)
        .filter(|token| is_identifier(&token.text))
        .map(|_| class_index + 1)
}

fn is_anonymous_default_class(tokens: &[Token], class_index: usize) -> bool {
    leading_export_index(tokens, class_index)
        .is_some_and(|export_index| is_default_export(tokens, export_index, class_index))
}

fn function_body_open(
    tokens: &[Token],
    name_index: usize,
    parens: &[Option<usize>],
) -> Option<usize> {
    let paren_open = find_next_text(tokens, name_index + 1, "(")?;
    let paren_close = parens
        .get(paren_open)
        .and_then(|match_index| *match_index)?;
    let mut cursor = paren_close + 1;
    while cursor < tokens.len() {
        match tokens[cursor].text.as_str() {
            "{" => return Some(cursor),
            ";" | "=" => return None,
            _ => cursor += 1,
        }
    }
    None
}

fn function_body_end_index(tokens: &[Token], braces: &[Option<usize>], body_open: usize) -> usize {
    braces
        .get(body_open)
        .and_then(|match_index| *match_index)
        .unwrap_or_else(|| tokens.len().saturating_sub(1).max(body_open))
}

fn variable_initializer_range(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    eq_index: usize,
    malformed_jsx_arrow: Option<(usize, u32)>,
) -> Option<(u32, u32, u32)> {
    let first = eq_index + 1;
    if let Some(name_index) = variable_function_expression_name_index(tokens, first) {
        let body_open = function_body_open(tokens, name_index, parens)?;
        let body_close = braces.get(body_open).and_then(|match_index| *match_index)?;
        return Some((
            tokens[first].line,
            tokens[body_close].line,
            complexity_between(tokens, first, body_close, None),
        ));
    }

    let arrow_index = variable_arrow_index(tokens, parens, first)?;
    let end_index = if tokens
        .get(arrow_index + 1)
        .is_some_and(|token| token.text == "{")
    {
        function_body_end_index(tokens, braces, arrow_index + 1)
    } else {
        variable_declaration_initializer_end_index(tokens, eq_index)
    };
    let end = malformed_jsx_arrow
        .filter(|(malformed_eq_index, _)| *malformed_eq_index == eq_index)
        .map(|(_, end_line)| end_line)
        .unwrap_or(tokens[end_index].line);
    Some((
        tokens[first].line,
        end,
        complexity_between(tokens, first, end_index, None),
    ))
}

fn variable_function_expression_name_index(tokens: &[Token], first: usize) -> Option<usize> {
    let function_index = if tokens
        .get(first)
        .is_some_and(|token| token.text == "function")
    {
        first
    } else if tokens.get(first).is_some_and(|token| token.text == "async")
        && tokens
            .get(first + 1)
            .is_some_and(|token| token.text == "function")
    {
        first + 1
    } else {
        return None;
    };
    Some(
        if tokens
            .get(function_index + 1)
            .is_some_and(|token| is_identifier(&token.text))
        {
            function_index + 1
        } else {
            function_index
        },
    )
}

fn variable_arrow_index(tokens: &[Token], parens: &[Option<usize>], first: usize) -> Option<usize> {
    let mut cursor = first;
    if tokens
        .get(cursor)
        .is_some_and(|token| token.text == "async")
    {
        cursor += 1;
    }
    variable_arrow_index_after_async(tokens, parens, cursor)
}

fn variable_arrow_index_after_async(
    tokens: &[Token],
    parens: &[Option<usize>],
    first: usize,
) -> Option<usize> {
    match tokens.get(first).map(|token| token.text.as_str()) {
        Some("<") => {
            let generic_close = generic_parameter_close_index(tokens, first)?;
            let paren_open = generic_close + 1;
            if tokens.get(paren_open).is_none_or(|token| token.text != "(") {
                return None;
            }
            arrow_after_parameter_list(tokens, parens, paren_open)
        }
        Some("(") => arrow_after_parameter_list(tokens, parens, first),
        Some(text) if is_identifier(text) => tokens
            .get(first + 1)
            .filter(|token| token.text == "=>")
            .map(|_| first + 1),
        _ => None,
    }
}

fn arrow_after_parameter_list(
    tokens: &[Token],
    parens: &[Option<usize>],
    paren_open: usize,
) -> Option<usize> {
    let paren_close = parens
        .get(paren_open)
        .and_then(|match_index| *match_index)?;
    let mut paren_depth = 0_i32;
    let mut brace_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    let mut index = paren_close + 1;
    while index < tokens.len() {
        match tokens[index].text.as_str() {
            "=>" if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 => {
                return Some(index);
            }
            "," | ";" | "as" | "satisfies"
                if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 =>
            {
                return None;
            }
            "(" => paren_depth += 1,
            ")" if paren_depth > 0 => paren_depth -= 1,
            "{" => brace_depth += 1,
            "}" if brace_depth > 0 => brace_depth -= 1,
            "[" => bracket_depth += 1,
            "]" if bracket_depth > 0 => bracket_depth -= 1,
            _ => {}
        }
        index += 1;
    }
    None
}

fn generic_parameter_close_index(tokens: &[Token], open_index: usize) -> Option<usize> {
    let mut paren_depth = 0_i32;
    let mut brace_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    let mut index = open_index + 1;
    while index < tokens.len() {
        match tokens[index].text.as_str() {
            "(" => paren_depth += 1,
            ")" if paren_depth > 0 => paren_depth -= 1,
            "{" => brace_depth += 1,
            "}" if brace_depth > 0 => brace_depth -= 1,
            "[" => bracket_depth += 1,
            "]" if bracket_depth > 0 => bracket_depth -= 1,
            ">" if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 => {
                return Some(index)
            }
            ";" | "=>" if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 => {
                return None
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn variable_declaration_end_line(tokens: &[Token], eq_index: usize) -> u32 {
    tokens
        .get(variable_declaration_initializer_end_index(tokens, eq_index))
        .map(|token| token.line)
        .unwrap_or_else(|| tokens.get(eq_index).map(|token| token.line).unwrap_or(1))
}

fn variable_declaration_initializer_end_index(tokens: &[Token], eq_index: usize) -> usize {
    let separator = variable_declaration_separator_after_initializer(tokens, eq_index);
    separator
        .and_then(|index| index.checked_sub(1))
        .unwrap_or_else(|| find_expression_end(tokens, eq_index + 1))
}

fn variable_declaration_after_initializer_next_cursor(tokens: &[Token], eq_index: usize) -> usize {
    match variable_declaration_separator_after_initializer(tokens, eq_index) {
        Some(index) if tokens[index].text == "," => index + 1,
        Some(_) => tokens.len(),
        None => eq_index + 1,
    }
}

fn variable_declaration_separator_after_initializer(
    tokens: &[Token],
    eq_index: usize,
) -> Option<usize> {
    let mut paren_depth = 0_i32;
    let mut brace_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    let generic_close = tokens
        .get(eq_index + 1)
        .filter(|token| token.text == "<")
        .and_then(|_| generic_parameter_close_index(tokens, eq_index + 1));
    for index in eq_index + 1..tokens.len() {
        if generic_close.is_some_and(|close_index| index <= close_index) {
            continue;
        }
        match tokens[index].text.as_str() {
            "(" => paren_depth += 1,
            ")" => paren_depth -= 1,
            "{" => brace_depth += 1,
            "}" => {
                if brace_depth == 0 {
                    return Some(index);
                }
                brace_depth -= 1;
            }
            "[" => bracket_depth += 1,
            "]" => bracket_depth -= 1,
            "," | ";" if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 => {
                return Some(index);
            }
            _ => {}
        }
    }
    None
}

fn variable_declaration_initializer_eq_index(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    start: usize,
    limit: usize,
) -> Option<usize> {
    let mut cursor = start;
    let limit = limit.min(tokens.len());
    while cursor < limit {
        match tokens[cursor].text.as_str() {
            "=" => return Some(cursor),
            "," | ";" | "}" => return None,
            "<" => {
                if let Some(close_index) = generic_parameter_close_index(tokens, cursor) {
                    cursor = close_index + 1;
                    continue;
                }
            }
            "(" | "{" | "[" => {
                if let Some(close_index) =
                    grouped_token_close_index(tokens, parens, braces, brackets, cursor)
                {
                    cursor = close_index + 1;
                    continue;
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn variable_declaration_without_initializer_end_index(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    brackets: &[Option<usize>],
    name_index: usize,
    limit: usize,
) -> usize {
    let mut cursor = name_index + 1;
    let mut last = name_index;
    let limit = limit.min(tokens.len());
    while cursor < limit {
        match tokens[cursor].text.as_str() {
            "," | ";" | "}" => return last,
            "<" => {
                if let Some(close_index) = generic_parameter_close_index(tokens, cursor) {
                    last = close_index;
                    cursor = close_index + 1;
                    continue;
                }
            }
            "(" | "{" | "[" => {
                if let Some(close_index) =
                    grouped_token_close_index(tokens, parens, braces, brackets, cursor)
                {
                    last = close_index;
                    cursor = close_index + 1;
                    continue;
                }
            }
            _ => {}
        }
        last = cursor;
        cursor += 1;
    }
    last
}

fn variable_declaration_without_initializer_next_cursor(
    tokens: &[Token],
    end_index: usize,
) -> usize {
    if tokens
        .get(end_index + 1)
        .is_some_and(|token| token.text == ",")
    {
        end_index + 2
    } else {
        tokens.len()
    }
}

fn method_signature_end(tokens: &[Token], paren_index: usize, body_close: usize) -> usize {
    find_next_until(tokens, paren_index + 1, ";", &["}"])
        .filter(|index| *index < body_close)
        .unwrap_or(paren_index)
}

fn type_like_declaration_end(
    tokens: &[Token],
    braces: &[Option<usize>],
    declaration_index: usize,
) -> usize {
    if let Some(open_index) = find_next_until(tokens, declaration_index + 1, "{", &[";"]) {
        if let Some(close_index) = braces.get(open_index).and_then(|match_index| *match_index) {
            return close_index;
        }
    }
    find_next_until(tokens, declaration_index + 1, ";", &[])
        .unwrap_or_else(|| find_expression_end(tokens, declaration_index))
}

fn anonymous_function_export(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    function_index: usize,
) -> Option<(u32, u32, u32)> {
    let paren_open = find_next_text(tokens, function_index + 1, "(")?;
    let paren_close = parens
        .get(paren_open)
        .and_then(|match_index| *match_index)?;
    let body_open = find_next_text(tokens, paren_close + 1, "{")?;
    let body_close = braces.get(body_open).and_then(|match_index| *match_index)?;
    Some((
        tokens[function_index].line,
        tokens[body_close].line,
        complexity_between(tokens, function_index, body_close, None),
    ))
}

fn type_like_default_export(
    tokens: &[Token],
    braces: &[Option<usize>],
    declaration_index: usize,
) -> Option<(u32, u32, u32)> {
    let end_index = type_like_declaration_end(tokens, braces, declaration_index);
    Some((tokens[declaration_index].line, tokens[end_index].line, 1))
}

fn default_expression_export(
    tokens: &[Token],
    braces: &[Option<usize>],
    expression_index: usize,
) -> Option<(u32, u32, u32)> {
    let end_index = if tokens
        .get(expression_index)
        .is_some_and(|token| token.text == "{")
    {
        braces
            .get(expression_index)
            .and_then(|match_index| *match_index)
            .unwrap_or(expression_index)
    } else {
        find_expression_end(tokens, expression_index)
    };
    Some((
        tokens.get(expression_index)?.line,
        tokens.get(end_index)?.line,
        complexity_between(tokens, expression_index, end_index, None),
    ))
}

fn decorated_start_index(tokens: &[Token], declaration_index: usize) -> usize {
    let mut start = declaration_index;
    while let Some(decorator_index) = previous_decorator_start(tokens, start) {
        start = decorator_index;
    }
    start
}

fn previous_decorator_start(tokens: &[Token], before_index: usize) -> Option<usize> {
    if before_index == 0 {
        return None;
    }

    let mut paren_depth = 0_i32;
    let mut brace_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    let mut index = before_index;
    while index > 0 {
        index -= 1;
        match tokens[index].text.as_str() {
            ")" => paren_depth += 1,
            "]" => bracket_depth += 1,
            "}" if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 => return None,
            "}" => brace_depth += 1,
            "(" if paren_depth > 0 => paren_depth -= 1,
            "[" if bracket_depth > 0 => bracket_depth -= 1,
            "{" if brace_depth > 0 => brace_depth -= 1,
            ";" if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 => return None,
            "@" if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 => {
                return Some(index)
            }
            "{" if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 => return None,
            _ => {}
        }
    }
    None
}

fn decorator_expression_end(tokens: &[Token], at_index: usize, limit: usize) -> usize {
    let mut paren_depth = 0_i32;
    let mut brace_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    let mut index = at_index + 1;
    let mut last = at_index;
    while index < limit {
        match tokens[index].text.as_str() {
            "(" => paren_depth += 1,
            ")" => paren_depth -= 1,
            "{" => brace_depth += 1,
            "}" if brace_depth > 0 => brace_depth -= 1,
            "[" => bracket_depth += 1,
            "]" if bracket_depth > 0 => bracket_depth -= 1,
            _ => {}
        }
        last = index;
        if paren_depth == 0
            && brace_depth == 0
            && bracket_depth == 0
            && tokens
                .get(index + 1)
                .is_none_or(|next| next.line > tokens[index].line)
        {
            break;
        }
        index += 1;
    }
    last
}

fn if_statement_end(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    body_start: usize,
) -> Option<usize> {
    let body_end = statement_end(tokens, parens, braces, body_start)?;
    let else_index = body_end + 1;
    if tokens
        .get(else_index)
        .is_none_or(|token| token.text != "else")
    {
        return Some(body_end);
    }

    statement_end(tokens, parens, braces, else_index + 1).or(Some(else_index))
}

fn statement_end(
    tokens: &[Token],
    parens: &[Option<usize>],
    braces: &[Option<usize>],
    body_start: usize,
) -> Option<usize> {
    let token = tokens.get(body_start)?;
    if tokens
        .get(body_start)
        .is_some_and(|token| token.text == "{")
    {
        return braces.get(body_start).and_then(|match_index| *match_index);
    }
    if token.text == "if" {
        let paren_open = find_next_text(tokens, body_start + 1, "(")?;
        let paren_close = parens
            .get(paren_open)
            .and_then(|match_index| *match_index)?;
        return if_statement_end(tokens, parens, braces, paren_close + 1);
    }
    Some(find_expression_end(tokens, body_start))
}

fn find_expression_end(tokens: &[Token], start: usize) -> usize {
    let line = tokens.get(start).map(|token| token.line).unwrap_or(1);
    let mut index = start;
    while index + 1 < tokens.len() {
        if tokens[index].text == ";" || tokens[index].line != line {
            break;
        }
        index += 1;
    }
    index
}

fn complexity_between(
    tokens: &[Token],
    start: usize,
    end: usize,
    skip_control_at: Option<usize>,
) -> u32 {
    let mut complexity = 1;
    let mut index = start;
    while index <= end && index < tokens.len() {
        if Some(index) != skip_control_at {
            match tokens[index].text.as_str() {
                "if" | "for" | "while" | "do" | "case" | "catch" | "&&" | "||" | "??" => {
                    complexity += 1;
                }
                "?" if tokens.get(index + 1).is_none_or(|next| next.text != ".")
                    && !is_typescript_type_question(tokens, index) =>
                {
                    complexity += 1;
                }
                text if text.starts_with('`') => {
                    complexity += template_interpolation_complexity(text);
                }
                _ => {}
            }
        }
        index += 1;
    }
    complexity
}

fn template_interpolation_complexity(template: &str) -> u32 {
    let bytes = template.as_bytes();
    if bytes.first() != Some(&b'`') {
        return 0;
    }
    let mut complexity = 0;
    let mut index = 1;
    while index + 1 < bytes.len() {
        if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
            continue;
        }
        if bytes[index] == b'$' && bytes[index + 1] == b'{' {
            let expression_start = index + 2;
            let Some(expression_end) = template_expression_end(template, expression_start) else {
                break;
            };
            let expression_tokens = tokenize(&template[expression_start..expression_end]);
            if !expression_tokens.is_empty() {
                complexity +=
                    complexity_between(&expression_tokens, 0, expression_tokens.len() - 1, None)
                        - 1;
            }
            index = expression_end + 1;
            continue;
        }
        index += 1;
    }
    complexity
}

fn is_typescript_type_question(tokens: &[Token], index: usize) -> bool {
    is_optional_type_marker(tokens, index) || is_conditional_type_question(tokens, index)
}

fn is_optional_type_marker(tokens: &[Token], index: usize) -> bool {
    if index == 0 {
        return false;
    }
    match tokens.get(index + 1).map(|token| token.text.as_str()) {
        Some(":") | Some(")") | Some(",") | Some(";") => true,
        Some("(") => is_optional_method_question(tokens, index),
        _ => false,
    }
}

fn is_optional_method_question(tokens: &[Token], index: usize) -> bool {
    let Some(previous) = index.checked_sub(1) else {
        return false;
    };
    let member_start = if tokens[previous].text == "]" {
        let brackets = matching_tokens(tokens, "[", "]");
        brackets
            .get(previous)
            .and_then(|match_index| *match_index)
            .unwrap_or(previous)
    } else if previous > 0 && tokens[previous - 1].text == "#" {
        previous - 1
    } else {
        previous
    };
    if member_start > 0 && tokens[member_start - 1].text == "." {
        return false;
    }
    let Some(before_member) = member_start
        .checked_sub(1)
        .and_then(|cursor| tokens.get(cursor))
    else {
        return false;
    };
    matches!(
        before_member.text.as_str(),
        "{" | "}" | ";" | "abstract" | "async" | "static" | "public" | "private" | "protected"
    )
}

fn is_conditional_type_question(tokens: &[Token], index: usize) -> bool {
    let mut paren_depth = 0_i32;
    let mut brace_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    let mut cursor = index;
    while let Some(previous) = cursor.checked_sub(1) {
        cursor = previous;
        match tokens[cursor].text.as_str() {
            ")" => paren_depth += 1,
            "(" => {
                if paren_depth == 0 {
                    return false;
                }
                paren_depth -= 1;
            }
            "}" => brace_depth += 1,
            "{" => {
                if brace_depth == 0 {
                    return false;
                }
                brace_depth -= 1;
            }
            "]" => bracket_depth += 1,
            "[" => {
                if bracket_depth == 0 {
                    return false;
                }
                bracket_depth -= 1;
            }
            "extends" if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 => {
                return true;
            }
            "," | ";" | "=>" if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 => {
                return false;
            }
            _ => {}
        }
    }
    false
}

fn normalize_if_condition(condition: &str) -> String {
    let mut output = String::new();
    let mut previous_was_replacement = false;
    for character in condition.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '$' | '.') {
            output.push(character);
            previous_was_replacement = false;
        } else if !previous_was_replacement {
            output.push('_');
            previous_was_replacement = true;
        }
    }
    let trimmed = output
        .trim_matches('_')
        .chars()
        .take(48)
        .collect::<String>();
    trimmed
}

fn leading_export_index(tokens: &[Token], declaration_index: usize) -> Option<usize> {
    let mut cursor = declaration_index;
    while cursor > 0 && is_leading_declaration_modifier(&tokens[cursor - 1].text) {
        cursor -= 1;
    }
    if cursor > 0 && tokens[cursor - 1].text == "export" {
        return Some(cursor - 1);
    }
    None
}

fn leading_export_index_for_type_like(tokens: &[Token], declaration_index: usize) -> Option<usize> {
    if tokens
        .get(declaration_index)
        .is_some_and(|token| token.text == "enum")
        && declaration_index > 0
        && tokens[declaration_index - 1].text == "const"
    {
        return leading_export_index(tokens, declaration_index - 1);
    }
    leading_export_index(tokens, declaration_index)
}

fn is_default_export(tokens: &[Token], export_index: usize, declaration_index: usize) -> bool {
    tokens[export_index + 1..declaration_index]
        .iter()
        .any(|token| token.text == "default")
}

fn is_declared_declaration(tokens: &[Token], declaration_index: usize) -> bool {
    tokens[..declaration_index]
        .iter()
        .rev()
        .take_while(|token| is_leading_declaration_modifier(&token.text) || token.text == "export")
        .any(|token| token.text == "declare")
}

fn is_leading_declaration_modifier(value: &str) -> bool {
    matches!(
        value,
        "default" | "async" | "await" | "declare" | "abstract" | "public" | "private" | "protected"
    )
}

fn is_variable_declaration_keyword(value: &str) -> bool {
    matches!(value, "const" | "let" | "var" | "using")
}

fn is_export_default_modifier(value: &str) -> bool {
    matches!(value, "async")
}

fn is_top_level(tokens: &[Token], index: usize) -> bool {
    let mut depth = 0_i32;
    for token in &tokens[..index] {
        match token.text.as_str() {
            "{" => depth += 1,
            "}" => depth -= 1,
            _ => {}
        }
    }
    depth == 0
        || leading_export_index(tokens, index).is_some_and(|export_index| {
            tokens[..export_index]
                .iter()
                .fold(0_i32, |depth, token| match token.text.as_str() {
                    "{" => depth + 1,
                    "}" => depth - 1,
                    _ => depth,
                })
                == 0
        })
}

fn find_next_text(tokens: &[Token], start: usize, text: &str) -> Option<usize> {
    tokens
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, token)| token.text == text)
        .map(|(index, _)| index)
}

fn find_next_until(tokens: &[Token], start: usize, text: &str, stops: &[&str]) -> Option<usize> {
    for (index, token) in tokens.iter().enumerate().skip(start) {
        if token.text == text {
            return Some(index);
        }
        if stops.iter().any(|stop| token.text == *stop) {
            return None;
        }
    }
    None
}

fn next_identifier(tokens: &[Token], start: usize) -> Option<usize> {
    tokens
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, token)| is_identifier(&token.text))
        .map(|(index, _)| index)
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    is_identifier_start_char(first) && chars.all(is_identifier_continue_char)
}

fn identifier_token(source: &str, start: usize) -> Option<(String, usize)> {
    let (first, mut index) = identifier_character_at(source, start)?;
    if !is_identifier_start_char(first) {
        return None;
    }
    let mut text = String::new();
    text.push(first);
    while index < source.len() {
        let Some((character, next_index)) = identifier_character_at(source, index) else {
            break;
        };
        if !is_identifier_continue_char(character) {
            break;
        }
        text.push(character);
        index = next_index;
    }
    Some((text, index))
}

fn identifier_character_at(source: &str, index: usize) -> Option<(char, usize)> {
    unicode_escape_at(source, index).or_else(|| {
        source[index..]
            .chars()
            .next()
            .map(|character| (character, index + character.len_utf8()))
    })
}

fn unicode_escape_at(source: &str, index: usize) -> Option<(char, usize)> {
    let bytes = source.as_bytes();
    if bytes.get(index) != Some(&b'\\') || bytes.get(index + 1) != Some(&b'u') {
        return None;
    }
    if bytes.get(index + 2) == Some(&b'{') {
        let mut cursor = index + 3;
        let mut value = 0_u32;
        let mut has_digit = false;
        while cursor < bytes.len() && bytes[cursor] != b'}' {
            let digit = hex_digit_value(bytes[cursor])?;
            has_digit = true;
            value = value.checked_mul(16)?.checked_add(digit)?;
            cursor += 1;
        }
        if !has_digit || bytes.get(cursor) != Some(&b'}') {
            return None;
        }
        return char::from_u32(value).map(|character| (character, cursor + 1));
    }
    let mut value = 0_u32;
    for offset in 0..4 {
        let digit = hex_digit_value(*bytes.get(index + 2 + offset)?)?;
        value = value * 16 + digit;
    }
    char::from_u32(value).map(|character| (character, index + 6))
}

fn hex_digit_value(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as u32),
        b'a'..=b'f' => Some((byte - b'a' + 10) as u32),
        b'A'..=b'F' => Some((byte - b'A' + 10) as u32),
        _ => None,
    }
}

fn is_literal_method_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    is_numeric_literal(value) || matches!(bytes.first(), Some(b'\'') | Some(b'"'))
}

fn is_string_literal(value: &str) -> bool {
    matches!(value.as_bytes().first(), Some(b'\'') | Some(b'"'))
}

fn is_numeric_literal(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_digit())
}

fn is_method_modifier(value: &str) -> bool {
    matches!(
        value,
        "public"
            | "private"
            | "protected"
            | "static"
            | "async"
            | "abstract"
            | "override"
            | "readonly"
            | "declare"
    )
}

fn matching_tokens(tokens: &[Token], open: &str, close: &str) -> Vec<Option<usize>> {
    let mut matches = vec![None; tokens.len()];
    let mut stack = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.text == open {
            stack.push(index);
        } else if token.text == close {
            if let Some(open_index) = stack.pop() {
                matches[open_index] = Some(index);
                matches[index] = Some(open_index);
            }
        }
    }
    matches
}

fn collect_delimiter_diagnostics(
    source: &str,
    tokens: &[Token],
    matches: &[Option<usize>],
    open: &str,
    close: &str,
    diagnostics: &mut Vec<AnchorDiagnostic>,
) {
    for (index, token) in tokens.iter().enumerate() {
        if matches
            .get(index)
            .and_then(|match_index| *match_index)
            .is_some()
        {
            continue;
        }
        if token.text == open {
            push_token_diagnostic(
                source,
                token,
                &format!(
                    "unmatched opening delimiter `{open}` prevented complete anchor extraction"
                ),
                diagnostics,
            );
        } else if token.text == close {
            push_token_diagnostic(
                source,
                token,
                &format!(
                    "unmatched closing delimiter `{close}` prevented complete anchor extraction"
                ),
                diagnostics,
            );
        }
    }
}

fn push_token_diagnostic(
    source: &str,
    token: &Token,
    message: &str,
    diagnostics: &mut Vec<AnchorDiagnostic>,
) {
    push_byte_diagnostic(source, token.start, message, diagnostics);
}

fn push_byte_diagnostic(
    source: &str,
    byte_index: usize,
    message: &str,
    diagnostics: &mut Vec<AnchorDiagnostic>,
) {
    let (line, column) = source_line_column(source, byte_index);
    diagnostics.push(AnchorDiagnostic {
        severity: AnchorDiagnosticSeverity::Error,
        line,
        column,
        message: message.to_string(),
    });
}

fn source_line_column(source: &str, byte_index: usize) -> (u32, u32) {
    let mut limit = byte_index.min(source.len());
    while limit > 0 && !source.is_char_boundary(limit) {
        limit -= 1;
    }
    let prefix = &source[..limit];
    let line = count_newlines(prefix) + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let column = prefix[line_start..].chars().count() as u32 + 1;
    (line, column)
}

fn tokenize(source: &str) -> Vec<Token> {
    tokenize_with_diagnostics(source).tokens
}

fn tokenize_with_diagnostics(source: &str) -> Tokenization {
    let bytes = source.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut diagnostics = Vec::<AnchorDiagnostic>::new();
    let mut index = 0;
    let mut line = 1_u32;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\n' {
            line += 1;
            index += 1;
            continue;
        }
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            let start = index;
            index += 2;
            let mut closed = false;
            while index + 1 < bytes.len() {
                if bytes[index] == b'\n' {
                    line += 1;
                }
                if bytes[index] == b'*' && bytes[index + 1] == b'/' {
                    index += 2;
                    closed = true;
                    break;
                }
                index += 1;
            }
            if !closed {
                push_byte_diagnostic(
                    source,
                    start,
                    "unterminated block comment prevented complete anchor extraction",
                    &mut diagnostics,
                );
                index = bytes.len();
            }
            continue;
        }
        if byte == b'/'
            && regex_literal_allowed_after(tokens.last().map(|token| token.text.as_str()))
        {
            if let Some(next_index) = regex_literal_end(bytes, index) {
                index = next_index;
                continue;
            }
        }
        if matches!(byte, b'\'' | b'"') {
            let quote = byte;
            let start = index;
            let start_line = line;
            let mut closed = false;
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\n' {
                    line += 1;
                }
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                    continue;
                }
                if bytes[index] == quote {
                    index += 1;
                    closed = true;
                    break;
                }
                index += 1;
            }
            tokens.push(Token {
                text: source[start..index].to_string(),
                line: start_line,
                start,
                end: index,
            });
            if !closed {
                push_byte_diagnostic(
                    source,
                    start,
                    "unterminated string literal prevented complete anchor extraction",
                    &mut diagnostics,
                );
            }
            continue;
        }
        if byte == b'`' {
            let start = index;
            let start_line = line;
            if let Some(resume_index) = unclosed_template_expression_resume_index(source, start) {
                index = resume_index;
                line = start_line + count_newlines(&source[start..resume_index]);
            } else {
                skip_template_literal_source(source, &mut index, &mut line);
            }
            tokens.push(Token {
                text: source[start..index].to_string(),
                line: start_line,
                start,
                end: index,
            });
            if source.as_bytes().get(index.saturating_sub(1)) != Some(&b'`') {
                push_byte_diagnostic(
                    source,
                    start,
                    "unterminated template literal prevented complete anchor extraction",
                    &mut diagnostics,
                );
            }
            continue;
        }
        if byte.is_ascii_digit() {
            let start = index;
            index = numeric_literal_end(bytes, index);
            tokens.push(Token {
                text: source[start..index].to_string(),
                line,
                start,
                end: index,
            });
            continue;
        }
        if let Some((text, end)) = identifier_token(source, index) {
            let start = index;
            index = end;
            tokens.push(Token {
                text,
                line,
                start,
                end: index,
            });
            continue;
        }
        let start = index;
        let text = match (byte, bytes.get(index + 1).copied()) {
            (b'=', Some(b'>')) => {
                index += 2;
                "=>".to_string()
            }
            (b'&', Some(b'&')) => {
                index += 2;
                "&&".to_string()
            }
            (b'|', Some(b'|')) => {
                index += 2;
                "||".to_string()
            }
            (b'?', Some(b'?')) => {
                index += 2;
                "??".to_string()
            }
            _ => {
                let character = source[index..].chars().next().unwrap_or('\0');
                index += character.len_utf8();
                source[start..index].to_string()
            }
        };
        tokens.push(Token {
            text,
            line,
            start,
            end: index,
        });
    }

    Tokenization {
        tokens,
        diagnostics,
    }
}

fn numeric_literal_end(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    if bytes.get(index) == Some(&b'0') {
        if matches!(bytes.get(index + 1), Some(b'x') | Some(b'X')) {
            index += 2;
            while index < bytes.len() && (bytes[index].is_ascii_hexdigit() || bytes[index] == b'_')
            {
                index += 1;
            }
            return index;
        }
        if matches!(bytes.get(index + 1), Some(b'b') | Some(b'B')) {
            index += 2;
            while index < bytes.len() && matches!(bytes[index], b'0' | b'1' | b'_') {
                index += 1;
            }
            return index;
        }
        if matches!(bytes.get(index + 1), Some(b'o') | Some(b'O')) {
            index += 2;
            while index < bytes.len()
                && (matches!(bytes[index], b'0'..=b'7') || bytes[index] == b'_')
            {
                index += 1;
            }
            return index;
        }
    }

    while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'_') {
        index += 1;
    }
    if bytes.get(index) == Some(&b'.')
        && bytes
            .get(index + 1)
            .is_some_and(|byte| byte.is_ascii_digit())
    {
        index += 1;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'_') {
            index += 1;
        }
    }
    if matches!(bytes.get(index), Some(b'e') | Some(b'E')) {
        let exponent_start = index;
        index += 1;
        if matches!(bytes.get(index), Some(b'+') | Some(b'-')) {
            index += 1;
        }
        let digit_start = index;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'_') {
            index += 1;
        }
        if index == digit_start {
            return exponent_start;
        }
    }
    index
}

fn regex_literal_allowed_after(previous: Option<&str>) -> bool {
    match previous {
        None => true,
        Some(previous) => matches!(
            previous,
            "(" | "["
                | "{"
                | "="
                | "=>"
                | ","
                | ":"
                | ";"
                | "!"
                | "?"
                | "&&"
                | "||"
                | "??"
                | "return"
                | "throw"
                | "case"
                | "delete"
                | "typeof"
                | "void"
                | "yield"
                | "await"
        ),
    }
}

fn regex_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    let mut in_character_class = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' | b'\r' => return None,
            b'\\' => {
                index = (index + 2).min(bytes.len());
            }
            b'[' => {
                in_character_class = true;
                index += 1;
            }
            b']' if in_character_class => {
                in_character_class = false;
                index += 1;
            }
            b'/' if !in_character_class => {
                index += 1;
                while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                    index += 1;
                }
                return Some(index);
            }
            _ => index += 1,
        }
    }
    None
}

fn is_identifier_start_char(character: char) -> bool {
    character == '_' || character == '$' || character.is_alphabetic()
}

fn is_identifier_continue_char(character: char) -> bool {
    is_identifier_start_char(character)
        || character.is_numeric()
        || matches!(character, '\u{200C}' | '\u{200D}')
        || is_unicode_combining_mark(character)
}

fn is_unicode_combining_mark(character: char) -> bool {
    matches!(
        character as u32,
        0x0300..=0x036F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F
    )
}

#[cfg(test)]
mod tests {
    use super::{anchor_exists, assert_anchor_exists, extract_anchors, AnchorKind};
    use crate::core::paths::RelativePath;

    #[test]
    fn extracts_current_core_fixture_like_typescript_contract() {
        let file = RelativePath::new("src/example.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"
export function handle() {
  if (a && b || c) return 1;
  return 2;
}
function duplicate() {}
function duplicate() {}
class Store {
  save() {
    return true;
  }
}
export const run = () => true;
"#,
        );

        assert!(extraction.complete);
        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:handle",
                "fn:duplicate",
                "fn:duplicate#2",
                "class:Store",
                "fn:Store.save",
                "fn:run",
                "export:handle",
                "export:run",
                "block:if_a_b_c",
            ]
        );
        let handle = extraction.anchors.get_str("fn:handle").unwrap();
        assert_eq!((handle.start, handle.end, handle.complexity), (2, 5, 4));
        assert_eq!(handle.kind, AnchorKind::Function);
        let block = extraction.anchors.get_str("block:if_a_b_c").unwrap();
        assert_eq!((block.start, block.end, block.complexity), (3, 3, 3));
        assert!(anchor_exists(&file, "function exists() {}", "fn:exists"));
    }

    #[test]
    fn marks_unmatched_delimiters_incomplete_while_preserving_recovered_anchors() {
        let file = RelativePath::new("src/incomplete.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            "function before() { return 1; }\nfunction broken() {\n  return 2;\n",
        );

        assert!(!extraction.complete);
        assert!(!extraction.diagnostics.is_empty());
        assert!(extraction.anchors.get_str("fn:before").is_some());

        let rust_file = RelativePath::new("src/incomplete.rs").unwrap();
        let rust = extract_anchors(
            &rust_file,
            "pub fn before() -> u32 { 1 }\npub fn broken() -> u32 {\n  2\n",
        );

        assert!(!rust.complete);
        assert!(!rust.diagnostics.is_empty());
        assert!(rust.anchors.get_str("fn:before").is_some());
    }

    #[test]
    fn extracts_rust_function_and_impl_anchors_without_ts_collectors() {
        let file = RelativePath::new("src/lib.rs").unwrap();
        let extraction = extract_anchors(
            &file,
            r##"
pub fn top_level<T>(value: T) -> T {
    if ready() && check() {
        return value;
    }
    value
}

async unsafe fn r#type() {
    loop {
        break;
    }
}

impl<T> Store<T> {
    pub fn new(value: T) -> Self {
        Self { value }
    }

    pub(crate) const fn get(&self) -> &T {
        &self.value
    }
}

impl<T> Display for Store<T> {
    fn fmt(&self) -> String {
        if self.value.is_ready() {
            return "ready".to_string();
        }
        "pending".to_string()
    }
}
"##,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:top_level",
                "fn:type",
                "impl:Store",
                "fn:Store.new",
                "fn:Store.get",
                "impl:Store.Display",
                "fn:Store.Display.fmt",
                "export:top_level",
                "export:Store.new",
                "export:Store.get",
                "block:if_ready_check",
            ]
        );
        assert_eq!(
            extraction.anchors.get_str("impl:Store").unwrap().kind,
            AnchorKind::Impl
        );
        assert_eq!(
            extraction.anchors.get_str("fn:Store.new").unwrap().kind,
            AnchorKind::Method
        );
        assert_eq!(
            extraction
                .anchors
                .get_str("fn:top_level")
                .unwrap()
                .complexity,
            3
        );
        assert_eq!(
            extraction
                .anchors
                .get_str("fn:Store.Display.fmt")
                .unwrap()
                .complexity,
            2
        );
        let block = extraction.anchors.get_str("block:if_ready_check").unwrap();
        assert_eq!((block.start, block.end, block.complexity), (3, 5, 3));
        assert_eq!(block.kind, AnchorKind::Block);
    }

    #[test]
    fn extracts_rust_structural_anchors_and_trait_default_methods() {
        let file = RelativePath::new("src/lib.rs").unwrap();
        let extraction = extract_anchors(
            &file,
            r##"#[derive(Clone)]
pub struct Store<T>
where
    T: Clone,
{
    value: T,
}

pub(crate) struct Tuple<T>(pub T)
where
    T: Copy;

struct Unit;

pub enum Event<T> {
    Created(T),
    Deleted,
}

pub trait Service<T>
where
    T: Clone,
{
    fn required(&self);
    fn ready(&self, input: T) -> bool {
        if check() || fallback() {
            return true;
        }
        false
    }
}

pub mod nested {
    pub fn inside() {}
}

mod external;
"##,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "struct:Store",
                "struct:Tuple",
                "struct:Unit",
                "enum:Event",
                "trait:Service",
                "fn:Service.required",
                "fn:Service.ready",
                "mod:nested",
                "fn:nested.inside",
                "mod:external",
                "export:Store",
                "export:Tuple",
                "export:Event",
                "export:Service",
                "export:nested",
                "export:nested.inside",
                "block:if_check_fallback",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("struct:Store").unwrap().start,
                extraction.anchors.get_str("struct:Store").unwrap().end,
                extraction.anchors.get_str("struct:Store").unwrap().kind,
            ),
            (1, 7, AnchorKind::Struct)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("struct:Tuple").unwrap().start,
                extraction.anchors.get_str("struct:Tuple").unwrap().end,
            ),
            (9, 11)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("struct:Unit").unwrap().start,
                extraction.anchors.get_str("struct:Unit").unwrap().end,
            ),
            (13, 13)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("enum:Event").unwrap().start,
                extraction.anchors.get_str("enum:Event").unwrap().end,
                extraction.anchors.get_str("enum:Event").unwrap().kind,
            ),
            (15, 18, AnchorKind::Enum)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("trait:Service").unwrap().start,
                extraction.anchors.get_str("trait:Service").unwrap().end,
                extraction.anchors.get_str("trait:Service").unwrap().kind,
            ),
            (20, 31, AnchorKind::Trait)
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("fn:Service.ready")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("fn:Service.ready").unwrap().end,
                extraction
                    .anchors
                    .get_str("fn:Service.ready")
                    .unwrap()
                    .complexity,
            ),
            (25, 30, 3)
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("block:if_check_fallback")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("block:if_check_fallback")
                    .unwrap()
                    .end,
                extraction
                    .anchors
                    .get_str("block:if_check_fallback")
                    .unwrap()
                    .complexity,
            ),
            (26, 28, 3)
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("fn:Service.required")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("fn:Service.required")
                    .unwrap()
                    .end,
                extraction
                    .anchors
                    .get_str("fn:Service.required")
                    .unwrap()
                    .complexity,
            ),
            (24, 24, 1)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("mod:nested").unwrap().start,
                extraction.anchors.get_str("mod:nested").unwrap().end,
                extraction.anchors.get_str("mod:nested").unwrap().kind,
            ),
            (33, 35, AnchorKind::Module)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("mod:external").unwrap().start,
                extraction.anchors.get_str("mod:external").unwrap().end,
            ),
            (37, 37)
        );
        assert_eq!(
            extraction.anchors.get_str("export:Service").unwrap().kind,
            AnchorKind::Export
        );
        assert!(extraction.anchors.get_str("fn:inside").is_none());
    }

    #[test]
    fn includes_rust_outer_attributes_and_docs_in_anchor_ranges() {
        let file = RelativePath::new("src/attrs.rs").unwrap();
        let extraction = extract_anchors(
            &file,
            r##"#[cfg(feature = "api")]
/// Builds store.
pub fn build() -> bool {
    true
}

#[derive(Clone)]
#[cfg_attr(
    feature = "serde",
    derive(Debug)
)]
pub struct Store {
    value: bool,
}

/**
 * Default service.
 */
pub trait Service {
    /// Required hook.
    fn required(&self);
    #[cfg(feature = "ready")]
    fn ready(&self) -> bool {
        true
    }
}

#[allow(dead_code)]
impl Store {
    /// Creates a store.
    pub fn new() -> Self {
        Self { value: true }
    }
    #[cfg(feature = "check")]
    fn check(&self) -> bool {
        true
    }
}
"##,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:build",
                "struct:Store",
                "trait:Service",
                "fn:Service.required",
                "fn:Service.ready",
                "impl:Store",
                "fn:Store.new",
                "fn:Store.check",
                "export:build",
                "export:Store",
                "export:Service",
                "export:Store.new",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:build").unwrap().start,
                extraction.anchors.get_str("fn:build").unwrap().end,
                extraction.anchors.get_str("export:build").unwrap().start,
            ),
            (1, 5, 1)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("struct:Store").unwrap().start,
                extraction.anchors.get_str("struct:Store").unwrap().end,
                extraction.anchors.get_str("export:Store").unwrap().start,
            ),
            (7, 14, 7)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("trait:Service").unwrap().start,
                extraction.anchors.get_str("trait:Service").unwrap().end,
                extraction
                    .anchors
                    .get_str("fn:Service.required")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("fn:Service.ready")
                    .unwrap()
                    .start,
            ),
            (16, 26, 20, 22)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("impl:Store").unwrap().start,
                extraction.anchors.get_str("impl:Store").unwrap().end,
                extraction.anchors.get_str("fn:Store.new").unwrap().start,
                extraction
                    .anchors
                    .get_str("export:Store.new")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("fn:Store.check").unwrap().start,
            ),
            (28, 38, 30, 30, 34)
        );
    }

    #[test]
    fn extracts_rust_function_local_items_without_macro_or_extern_ghosts() {
        let file = RelativePath::new("src/local.rs").unwrap();
        let extraction = extract_anchors(
            &file,
            r##"fn outer() {
    fn helper() -> bool {
        true
    }

    struct Local;

    impl Local {
        pub fn make() -> Self {
            Local
        }
    }

    mod local_mod {
        pub fn published() {}
    }

    if enabled() {
        fn branch_helper() {}
        struct Branch;
    }

    macro_rules! define_fake {
        () => {
            fn fake_macro_rules() {}
        };
    }

    invoke! {
        fn fake_invocation() {}
    }

    unsafe extern "C" {
        fn c_api();
    }
}

fn after() {}
"##,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:outer",
                "fn:outer.helper",
                "struct:outer.Local",
                "impl:outer.Local",
                "fn:outer.Local.make",
                "mod:outer.local_mod",
                "fn:outer.local_mod.published",
                "fn:outer.branch_helper",
                "struct:outer.Branch",
                "fn:after",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:outer.helper").unwrap().start,
                extraction.anchors.get_str("fn:outer.helper").unwrap().end,
                extraction
                    .anchors
                    .get_str("fn:outer.Local.make")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("fn:outer.local_mod.published")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("fn:outer.branch_helper")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("fn:after").unwrap().start,
            ),
            (2, 4, 9, 15, 19, 38)
        );
        assert!(extraction
            .anchors
            .get_str("fn:outer.fake_macro_rules")
            .is_none());
        assert!(extraction
            .anchors
            .get_str("fn:outer.fake_invocation")
            .is_none());
        assert!(extraction.anchors.get_str("fn:outer.c_api").is_none());
        assert!(extraction
            .anchors
            .get_str("export:outer.Local.make")
            .is_none());
        assert!(extraction
            .anchors
            .get_str("export:outer.local_mod.published")
            .is_none());
    }

    #[test]
    fn counts_rust_complexity_from_executable_bodies() {
        let file = RelativePath::new("src/complexity.rs").unwrap();
        let extraction = extract_anchors(
            &file,
            r##"fn from_header<T: ?Sized>() -> Result<u8, Error> {
    let loaded = value()?;
    match mode() {
        Mode::A => 1,
        Mode::B => 2,
        _ => 3,
    };
    Ok(loaded)
}

fn outer() {
    fn hidden() {
        if hidden_check() {}
    }

    macro_rules! branchy {
        () => {
            if fake() && fake_two() {}
        };
    }

    if visible() {}
}
"##,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:from_header", "fn:outer", "fn:outer.hidden"]
        );
        assert_eq!(
            extraction
                .anchors
                .get_str("fn:from_header")
                .unwrap()
                .complexity,
            5
        );
        assert_eq!(
            extraction.anchors.get_str("fn:outer").unwrap().complexity,
            2
        );
        assert_eq!(
            extraction
                .anchors
                .get_str("fn:outer.hidden")
                .unwrap()
                .complexity,
            2
        );
        assert!(extraction
            .anchors
            .get_str("block:if_fake_fake_two")
            .is_none());
    }

    #[test]
    fn extracts_rust_significant_if_blocks_with_duplicate_suffixes() {
        let file = RelativePath::new("src/blocks.rs").unwrap();
        let extraction = extract_anchors(
            &file,
            r##"fn run() {
    if user.active && (user.admin || account.enabled) {
        approve();
    } else if fallback && retry && ready {
        retry();
    }
    if user.active && (user.admin || account.enabled) {
        audit();
    }
}
"##,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:run",
                "block:if_user.active_user.admin_account.enabled",
                "block:if_fallback_retry_ready",
                "block:if_user.active_user.admin_account.enabled#2",
            ]
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("block:if_user.active_user.admin_account.enabled")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("block:if_user.active_user.admin_account.enabled")
                    .unwrap()
                    .end,
                extraction
                    .anchors
                    .get_str("block:if_user.active_user.admin_account.enabled")
                    .unwrap()
                    .complexity,
            ),
            (2, 4, 4)
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("block:if_fallback_retry_ready")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("block:if_fallback_retry_ready")
                    .unwrap()
                    .end,
                extraction
                    .anchors
                    .get_str("block:if_fallback_retry_ready")
                    .unwrap()
                    .complexity,
            ),
            (4, 6, 4)
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("block:if_user.active_user.admin_account.enabled#2")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("block:if_user.active_user.admin_account.enabled#2")
                    .unwrap()
                    .end,
            ),
            (7, 9)
        );
    }

    #[test]
    fn extracts_rust_public_export_anchors_for_use_and_named_items() {
        let file = RelativePath::new("src/exports.rs").unwrap();
        let extraction = extract_anchors(
            &file,
            r##"pub type PublicAlias<T> = Option<T>;
pub const LIMIT: usize = { 4 };
pub static mut COUNTER: usize = 0;
pub use crate::inner::{Thing, Other as Renamed, nested::Deep};
pub(crate) use self::local::r#type as r#trait;
mod private {
    pub type InnerAlias = u8;
    pub const INNER: usize = 1;
    pub use super::PublicAlias as AliasAgain;
}
"##,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "mod:private",
                "export:PublicAlias",
                "export:LIMIT",
                "export:COUNTER",
                "export:Thing",
                "export:Renamed",
                "export:Deep",
                "export:trait",
                "export:private.InnerAlias",
                "export:private.INNER",
                "export:private.AliasAgain",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("export:LIMIT").unwrap().start,
                extraction.anchors.get_str("export:LIMIT").unwrap().end,
                extraction.anchors.get_str("export:LIMIT").unwrap().kind,
            ),
            (2, 2, AnchorKind::Export)
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("export:private.InnerAlias")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:private.InnerAlias")
                    .unwrap()
                    .end,
            ),
            (7, 7)
        );
    }

    #[test]
    fn qualifies_rust_module_items_and_skips_extern_block_declarations() {
        let file = RelativePath::new("src/lib.rs").unwrap();
        let extraction = extract_anchors(
            &file,
            r##"mod api {
    pub struct State;
    pub fn run() {}
    impl State {
        pub fn new() -> Self {
            State
        }
    }
}

mod worker {
    pub fn run() {}
}

unsafe extern "C" {
    pub fn c_api(input: *const u8);
}

pub unsafe extern "C" fn callback() {}

pub struct r#type;
pub enum r#match { A }
pub trait r#async {
    fn r#await(&self);
}
pub mod r#crate {}
"##,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "mod:api",
                "struct:api.State",
                "fn:api.run",
                "impl:api.State",
                "fn:api.State.new",
                "mod:worker",
                "fn:worker.run",
                "fn:callback",
                "struct:type",
                "enum:match",
                "trait:async",
                "fn:async.await",
                "mod:crate",
                "export:api.State",
                "export:api.run",
                "export:api.State.new",
                "export:worker.run",
                "export:callback",
                "export:type",
                "export:match",
                "export:async",
                "export:crate",
            ]
        );
        assert!(extraction.anchors.get_str("fn:c_api").is_none());
        assert!(extraction.anchors.get_str("export:c_api").is_none());
        assert!(extraction.anchors.get_str("fn:run").is_none());
        assert!(extraction.anchors.get_str("fn:run#2").is_none());
        assert_eq!(
            extraction.anchors.get_str("fn:api.State.new").unwrap().kind,
            AnchorKind::Method
        );
    }

    #[test]
    fn names_rust_trait_impl_subjects_after_the_implemented_type() {
        let file = RelativePath::new("src/impls.rs").unwrap();
        let extraction = extract_anchors(
            &file,
            r##"trait Render {}
trait Poll {}
trait Debug {}
struct Store;
trait Reader {}

impl Render for &mut Store {
    fn render(&self) {}
}

impl Poll for dyn Reader {
    fn poll(&self) {}
}

impl Debug for crate::store::Store {
    fn fmt(&self) {}
}
"##,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "trait:Render",
                "trait:Poll",
                "trait:Debug",
                "struct:Store",
                "trait:Reader",
                "impl:Store.Render",
                "fn:Store.Render.render",
                "impl:Reader.Poll",
                "fn:Reader.Poll.poll",
                "impl:Store.Debug",
                "fn:Store.Debug.fmt",
            ]
        );
    }

    #[test]
    fn skips_rust_strings_comments_lifetimes_and_nested_macro_functions() {
        let file = RelativePath::new("src/lib.rs").unwrap();
        let extraction = extract_anchors(
            &file,
            r##"
// fn fake_comment() {}
/* nested /* fn fake_block() {} */ still comment */
const TEXT: &str = "fn fake_string() {}";
const RAW: &str = r#"fn fake_raw() {}"#;

macro_rules! define_fake {
    () => {
        fn fake_macro() {}
    };
}

pub fn borrow<'a>(value: &'a str) -> &'a str {
    value
}

#[cfg(feature = "a")]
fn duplicate() {}
#[cfg(feature = "b")]
fn duplicate() {}
"##,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:borrow",
                "fn:duplicate",
                "fn:duplicate#2",
                "export:borrow"
            ]
        );
    }

    #[test]
    fn matches_else_if_block_chain_edges() {
        let file = RelativePath::new("src/else-if-blocks.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            "function run() {\n  if (a && b && c) { return 1; }\n  else if (d && e && f) { return 2; }\n}\n",
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:run", "block:if_a_b_c", "block:if_d_e_f"]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:run").unwrap().start,
                extraction.anchors.get_str("fn:run").unwrap().end,
                extraction.anchors.get_str("fn:run").unwrap().complexity,
            ),
            (1, 4, 7)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("block:if_a_b_c").unwrap().start,
                extraction.anchors.get_str("block:if_a_b_c").unwrap().end,
                extraction
                    .anchors
                    .get_str("block:if_a_b_c")
                    .unwrap()
                    .complexity,
            ),
            (2, 3, 6)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("block:if_d_e_f").unwrap().start,
                extraction.anchors.get_str("block:if_d_e_f").unwrap().end,
                extraction
                    .anchors
                    .get_str("block:if_d_e_f")
                    .unwrap()
                    .complexity,
            ),
            (3, 3, 3)
        );
    }

    #[test]
    fn matches_frozen_typescript_edge_case_order() {
        let file = RelativePath::new("src/edge.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"
export default function namedDefault() { return 1; }
export function overloaded(x: string): string;
export function overloaded(x: number): number;
export function overloaded(x) { return x; }
class Store {
  constructor() {}
  private save() { return true; }
  static load() { return true; }
  get value() { return 1; }
  field = () => true;
}
const obj = { method() { return true; } };
function outer() { function inner() { return true; } return inner(); }
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:namedDefault",
                "fn:overloaded",
                "fn:outer",
                "class:Store",
                "fn:Store.save",
                "fn:Store.load",
                "export:default",
                "export:overloaded",
            ]
        );
        assert_eq!(
            extraction
                .anchors
                .get_str("export:overloaded")
                .unwrap()
                .start,
            3
        );
        assert!(extraction.anchors.get_str("fn:Store.value").is_none());
        assert!(extraction.anchors.get_str("fn:inner").is_none());
    }

    #[test]
    fn matches_modern_typescript_export_and_method_edges() {
        let file = RelativePath::new("src/modern.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"
export async function fetchData() { await run(); }
export default function() { return 1; }
export default () => true;
export const value = 1;
const local = () => true;
export { local as renamed };
class C {
  async save() { return true; }
  *items() { yield 1; }
  [computed]() { return 1; }
  #secret() { return 2; }
  declare only(): void;
}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:fetchData",
                "class:C",
                "fn:C.save",
                "fn:C.items",
                "fn:C.[computed]",
                "fn:C.#secret",
                "fn:local",
                "export:fetchData",
                "export:default",
                "export:value",
                "export:renamed",
            ]
        );
        assert_eq!(
            extraction.anchors.get_str("export:default").unwrap().start,
            3
        );
        assert_eq!(extraction.anchors.get_str("export:value").unwrap().start, 5);
        assert_eq!(
            extraction.anchors.get_str("export:renamed").unwrap().start,
            6
        );
        assert!(extraction.anchors.get_str("fn:C.only").is_none());
    }

    #[test]
    fn matches_typescript_type_like_declare_and_export_order_edges() {
        let file = RelativePath::new("src/exports.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"
export const run = () => true;
export function handle() {}
export class Box { open() {} }
export interface I {}
export type T = string;
export enum E { A }
export const enum CE { A }
export namespace N { export function inside() {} }
export declare function declared(x: string): string;
export declare const declaredValue: number;
export declare class Declared { run(): void; }
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:handle",
                "fn:declared",
                "class:Box",
                "fn:Box.open",
                "class:Declared",
                "fn:Declared.run",
                "fn:run",
                "export:handle",
                "export:declared",
                "export:run",
                "export:Box",
                "export:I",
                "export:T",
                "export:E",
                "export:CE",
                "export:N",
                "export:declaredValue",
                "export:Declared",
            ]
        );
        assert_eq!(extraction.anchors.get_str("export:I").unwrap().start, 5);
        assert_eq!(extraction.anchors.get_str("export:T").unwrap().start, 6);
        assert_eq!(extraction.anchors.get_str("export:E").unwrap().start, 7);
        assert_eq!(extraction.anchors.get_str("export:CE").unwrap().start, 8);
        assert_eq!(extraction.anchors.get_str("export:N").unwrap().start, 9);
        assert_eq!(
            extraction.anchors.get_str("fn:Declared.run").unwrap().start,
            12
        );
        assert!(extraction.anchors.get_str("fn:inside").is_none());
    }

    #[test]
    fn matches_default_type_like_export_edges() {
        let extraction = extract_anchors(
            &RelativePath::new("src/default-type-like.ts").unwrap(),
            r#"export default interface I { x: number }
export { I as NamedI };
export default enum E { A }
export { E as NamedE };
export default type T = string;
export { T as NamedT };
export default namespace N { export const x = 1; }
export { N as NamedN };
function after() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:after",
                "export:default",
                "export:NamedI",
                "export:NamedE"
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("export:default").unwrap().start,
                extraction.anchors.get_str("export:NamedI").unwrap().start,
                extraction.anchors.get_str("export:NamedE").unwrap().start,
                extraction.anchors.get_str("fn:after").unwrap().start,
            ),
            (1, 1, 3, 9)
        );
        for ghost in [
            "export:I",
            "export:E",
            "export:T",
            "export:N",
            "export:NamedT",
            "export:NamedN",
        ] {
            assert!(extraction.anchors.get_str(ghost).is_none());
        }
    }

    #[test]
    fn matches_decorator_line_ranges_for_classes_exports_and_methods() {
        let file = RelativePath::new("src/decorators.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"@sealed({
  role: "api"
})
export class Service {
  @logged({
    level: "info"
  })
  run() {}
}
class Local {
  @logged
  [compute]() {}
  @secret
  #run() {}
}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "class:Service",
                "fn:Service.run",
                "class:Local",
                "fn:Local.[compute]",
                "fn:Local.#run",
                "export:Service",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("class:Service").unwrap().start,
                extraction.anchors.get_str("class:Service").unwrap().end,
            ),
            (1, 9)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:Service.run").unwrap().start,
                extraction.anchors.get_str("fn:Service.run").unwrap().end,
            ),
            (5, 8)
        );
        assert_eq!(
            extraction.anchors.get_str("export:Service").unwrap().start,
            1
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("fn:Local.[compute]")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("fn:Local.#run").unwrap().start,
            ),
            (11, 13)
        );
    }

    #[test]
    fn suppresses_class_field_initializer_function_anchors() {
        let extraction = extract_anchors(
            &RelativePath::new("src/class-fields.ts").unwrap(),
            r#"class C {
  field = () => true;
  static staticField = function hidden() { return 1; };
  #privateField = function privateHidden() { return 2; };
  [fieldName] = function hiddenComputed() { return 3; };
  "quotedField" = function hiddenQuoted() { return 4; };
  accessor auto = 1;
  method() {}
  #method() {}
  [methodName]() {}
  "quotedMethod"() {}
}
function after() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:after",
                "class:C",
                "fn:C.method",
                "fn:C.#method",
                "fn:C.[methodName]",
                "fn:C.\"quotedMethod\"",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("class:C").unwrap().start,
                extraction.anchors.get_str("class:C").unwrap().end,
                extraction.anchors.get_str("fn:C.method").unwrap().start,
                extraction.anchors.get_str("fn:C.#method").unwrap().start,
                extraction
                    .anchors
                    .get_str("fn:C.[methodName]")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("fn:C.\"quotedMethod\"")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("fn:after").unwrap().start,
            ),
            (1, 12, 8, 9, 10, 11, 13)
        );
        for ghost in [
            "fn:C.field",
            "fn:C.staticField",
            "fn:C.hidden",
            "fn:C.privateHidden",
            "fn:C.hiddenComputed",
            "fn:C.hiddenQuoted",
            "fn:C.auto",
        ] {
            assert!(extraction.anchors.get_str(ghost).is_none());
        }
    }

    #[test]
    fn matches_semicolonless_class_field_recovery_edges() {
        let semicolonless = extract_anchors(
            &RelativePath::new("src/semicolonless-class-fields.ts").unwrap(),
            r#"class C {
  field = () => true
  method() { return 1; }
  other = function hidden() { return 2; }
  #privateField = () => true
  #method() { return 3; }
  [computed] = function hiddenComputed() { return 4; }
  [methodName]() { return 5; }
}
function after() {}
"#,
        );

        assert_eq!(
            semicolonless
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "class:C", "fn:C.method", "fn:C.#method"]
        );
        assert_eq!(
            (
                semicolonless.anchors.get_str("class:C").unwrap().start,
                semicolonless.anchors.get_str("class:C").unwrap().end,
                semicolonless.anchors.get_str("fn:C.method").unwrap().start,
                semicolonless.anchors.get_str("fn:C.#method").unwrap().start,
                semicolonless.anchors.get_str("fn:after").unwrap().start,
            ),
            (1, 8, 3, 6, 10)
        );
        for ghost in [
            "fn:C.field",
            "fn:C.hidden",
            "fn:C.privateField",
            "fn:C.hiddenComputed",
            "fn:C.[methodName]",
        ] {
            assert!(semicolonless.anchors.get_str(ghost).is_none());
        }

        let multiline = extract_anchors(
            &RelativePath::new("src/multiline-class-fields.ts").unwrap(),
            r#"class C {
  field =
    () => true
  method() { return 1; }
  data = {
    run() { return 2; }
  }
  afterData() { return 3; }
}
function after() {}
"#,
        );

        assert_eq!(
            multiline
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "class:C", "fn:C.method", "fn:C.afterData"]
        );
        assert_eq!(
            (
                multiline.anchors.get_str("class:C").unwrap().start,
                multiline.anchors.get_str("class:C").unwrap().end,
                multiline.anchors.get_str("fn:C.method").unwrap().start,
                multiline.anchors.get_str("fn:C.afterData").unwrap().start,
                multiline.anchors.get_str("fn:after").unwrap().start,
            ),
            (1, 9, 4, 8, 10)
        );
        assert!(multiline.anchors.get_str("fn:C.run").is_none());
        assert!(multiline.anchors.get_str("fn:C.field").is_none());
    }

    #[test]
    fn matches_semicolonless_field_generator_recovery_edges() {
        let semicolonless = extract_anchors(
            &RelativePath::new("src/field-followed-by-generator.ts").unwrap(),
            r#"class C {
  field = 1
  *items() { yield 1; }
  other = 2
  async save() { return 1; }
}
function after() {}
"#,
        );

        assert_eq!(
            semicolonless
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "class:C"]
        );
        assert_eq!(
            (
                semicolonless.anchors.get_str("class:C").unwrap().start,
                semicolonless.anchors.get_str("class:C").unwrap().end,
                semicolonless.anchors.get_str("fn:after").unwrap().start,
            ),
            (1, 3, 7)
        );
        assert!(semicolonless.anchors.get_str("fn:C.items").is_none());
        assert!(semicolonless.anchors.get_str("fn:C.save").is_none());

        let semicolon = extract_anchors(
            &RelativePath::new("src/field-followed-by-generator-semicolon.ts").unwrap(),
            r#"class C {
  field = 1;
  *items() { yield 1; }
}
function after() {}
"#,
        );

        assert_eq!(
            semicolon
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "class:C", "fn:C.items"]
        );
        assert_eq!(
            (
                semicolon.anchors.get_str("class:C").unwrap().start,
                semicolon.anchors.get_str("class:C").unwrap().end,
                semicolon.anchors.get_str("fn:C.items").unwrap().start,
                semicolon.anchors.get_str("fn:after").unwrap().start,
            ),
            (1, 4, 3, 5)
        );
    }

    #[test]
    fn matches_anonymous_default_class_method_anchors() {
        let file = RelativePath::new("src/anon-default.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"@sealed
export default class {
  @logged
  open() {}
  *items() { yield 1; }
  [compute]() {}
  #secret() {}
  get value() { return 1; }
  field = () => true;
}
class Later { m() {} }
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:open",
                "fn:items",
                "fn:[compute]",
                "fn:#secret",
                "class:Later",
                "fn:Later.m",
                "export:default",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:open").unwrap().start,
                extraction.anchors.get_str("fn:open").unwrap().end,
            ),
            (3, 4)
        );
        assert_eq!(
            extraction.anchors.get_str("export:default").unwrap().start,
            1
        );
        assert_eq!(
            extraction.anchors.get_str("export:default").unwrap().end,
            10
        );
        assert!(extraction.anchors.get_str("fn:value").is_none());
        assert!(extraction.anchors.get_str("fn:field").is_none());
    }

    #[test]
    fn suppresses_class_expression_parser_ghosts() {
        let extraction = extract_anchors(
            &RelativePath::new("src/class-expressions.ts").unwrap(),
            r#"const Plain = class { method() {} };
const Named = class Inner { method() {} };
export const Exported = class ExportedInner { method() {} };
const Nested = foo(class NestedInner { method() {} });
class Real { method() {} }
function later() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:later",
                "class:Real",
                "fn:Real.method",
                "export:Exported"
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("export:Exported").unwrap().start,
                extraction.anchors.get_str("class:Real").unwrap().start,
                extraction.anchors.get_str("fn:later").unwrap().start,
            ),
            (3, 5, 6)
        );
        for ghost in [
            "class:Inner",
            "fn:Inner.method",
            "class:ExportedInner",
            "fn:ExportedInner.method",
            "class:NestedInner",
            "fn:NestedInner.method",
            "fn:Plain.method",
        ] {
            assert!(extraction.anchors.get_str(ghost).is_none());
        }
    }

    #[test]
    fn matches_tsx_jsx_anchor_ranges_and_complexity_edges() {
        let file = RelativePath::new("src/view.tsx").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"
export function View(props: { ok: boolean }) {
  return <div>{props.ok ? <span/> : null}</div>;
}
export const Panel = (props: { ok: boolean }) => (
  <section>{props.ok && <span>{props.ok ? "yes" : "no"}</span>}</section>
);
const FragmentView = () => <>
  <span/>
  <span/>
</>;
export class Screen {
  render() {
    return <main>{items.map((item) => <span key={item.id}>{item.name}</span>)}</main>;
  }
}
function WithBlock(props) {
  if (props.ok && props.ready || props.admin) {
    return <div>{props.ok ? <span/> : null}</div>;
  }
  return null;
}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:View",
                "fn:WithBlock",
                "class:Screen",
                "fn:Screen.render",
                "fn:Panel",
                "fn:FragmentView",
                "export:View",
                "export:Panel",
                "export:Screen",
                "block:if_props.ok_props.ready_props.admin",
            ]
        );
        let view = extraction.anchors.get_str("fn:View").unwrap();
        assert_eq!((view.start, view.end, view.complexity), (2, 4, 2));
        let panel = extraction.anchors.get_str("fn:Panel").unwrap();
        assert_eq!((panel.start, panel.end, panel.complexity), (5, 7, 3));
        let fragment = extraction.anchors.get_str("fn:FragmentView").unwrap();
        assert_eq!(
            (fragment.start, fragment.end, fragment.complexity),
            (8, 11, 1)
        );
        let render = extraction.anchors.get_str("fn:Screen.render").unwrap();
        assert_eq!((render.start, render.end, render.complexity), (13, 15, 1));
        let with_block = extraction.anchors.get_str("fn:WithBlock").unwrap();
        assert_eq!(
            (with_block.start, with_block.end, with_block.complexity),
            (17, 22, 5)
        );
        let block = extraction
            .anchors
            .get_str("block:if_props.ok_props.ready_props.admin")
            .unwrap();
        assert_eq!((block.start, block.end, block.complexity), (18, 20, 4));
    }

    #[test]
    fn matches_import_reexport_and_regex_literal_anchor_edges() {
        let file = RelativePath::new("src/imports-regex.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"interface LocalType {}
type LocalAlias = string;
const local = () => true;
export type { LocalType, LocalAlias };
export { local as remoteName } from "./dep";
export { local as renamed };
export * from "./remote";
export * as remoteNs from "./remote";
const regex = /function fake() { return true; }/;
function real() {
  return /if (fake && other || third) { return true; }/.test("x");
}
const tpl = `class Imaginary { run() {} }`;
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "fn:local",
                "export:LocalType",
                "export:LocalAlias",
                "export:renamed",
            ]
        );
        let real = extraction.anchors.get_str("fn:real").unwrap();
        assert_eq!((real.start, real.end, real.complexity), (10, 12, 1));
        let local = extraction.anchors.get_str("fn:local").unwrap();
        assert_eq!((local.start, local.end), (3, 3));
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("export:LocalType")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:LocalAlias")
                    .unwrap()
                    .start,
            ),
            (1, 2)
        );
        assert!(extraction.anchors.get_str("export:remoteName").is_none());
        assert!(extraction.anchors.get_str("fn:fake").is_none());
        assert!(extraction
            .anchors
            .get_str("block:if_fake_other_third")
            .is_none());

        let import_equals = extract_anchors(
            &RelativePath::new("src/import-equals.ts").unwrap(),
            r#"import Alias = require("dep");
import Other = ns.Other;
import { "dash-name" as dashName, regular as regularName } from "./dep";
export { Alias, Other as ExportedOther, dashName, regularName };
export type { Alias as AliasType };
function real() {}
"#,
        );
        assert_eq!(
            import_equals
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "export:Alias",
                "export:ExportedOther",
                "export:AliasType",
            ]
        );
        assert_eq!(
            (
                import_equals.anchors.get_str("export:Alias").unwrap().start,
                import_equals
                    .anchors
                    .get_str("export:ExportedOther")
                    .unwrap()
                    .start,
                import_equals
                    .anchors
                    .get_str("export:AliasType")
                    .unwrap()
                    .start,
            ),
            (1, 2, 1)
        );
        assert!(import_equals.anchors.get_str("export:dashName").is_none());
        assert!(import_equals
            .anchors
            .get_str("export:regularName")
            .is_none());

        let import_type_equals = extract_anchors(
            &RelativePath::new("src/import-type-equals.ts").unwrap(),
            r#"import Alias = require("dep");
import Other = ns.Other;
import type TypeAlias = require("types");
import type BareType;
import type ImportedDefault from "dep";
import type { ImportedNamed } from "dep";
export { Alias, Other as ExportedOther, TypeAlias, BareType as BareAgain, ImportedDefault as DefaultAgain, ImportedNamed as NamedAgain };
export type { Alias as AliasType, TypeAlias as TypeAliasType };
function real() {}
"#,
        );
        assert_eq!(
            import_type_equals
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "export:Alias",
                "export:ExportedOther",
                "export:TypeAlias",
                "export:BareAgain",
                "export:AliasType",
                "export:TypeAliasType",
            ]
        );
        assert_eq!(
            (
                import_type_equals
                    .anchors
                    .get_str("export:TypeAlias")
                    .unwrap()
                    .start,
                import_type_equals
                    .anchors
                    .get_str("export:BareAgain")
                    .unwrap()
                    .start,
                import_type_equals
                    .anchors
                    .get_str("export:TypeAliasType")
                    .unwrap()
                    .start,
            ),
            (3, 4, 3)
        );
        assert!(import_type_equals
            .anchors
            .get_str("export:DefaultAgain")
            .is_none());
        assert!(import_type_equals
            .anchors
            .get_str("export:NamedAgain")
            .is_none());

        let malformed_import_alias = extract_anchors(
            &RelativePath::new("src/import-malformed-alias.ts").unwrap(),
            r#"import Broken Local;
import Bare;
import FromLike from "dep";
import Comma, { Other } from "dep";
export { Broken as BrokenAgain, Bare as BareAgain, FromLike as FromAgain, Comma as CommaAgain };
function real() {}
"#,
        );
        assert_eq!(
            malformed_import_alias
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:BrokenAgain", "export:BareAgain"]
        );
        assert_eq!(
            (
                malformed_import_alias
                    .anchors
                    .get_str("export:BrokenAgain")
                    .unwrap()
                    .start,
                malformed_import_alias
                    .anchors
                    .get_str("export:BareAgain")
                    .unwrap()
                    .start,
            ),
            (1, 2)
        );
        assert!(malformed_import_alias
            .anchors
            .get_str("export:FromAgain")
            .is_none());
        assert!(malformed_import_alias
            .anchors
            .get_str("export:CommaAgain")
            .is_none());

        let import_attributes = extract_anchors(
            &RelativePath::new("src/import-attrs.ts").unwrap(),
            r#"const local = () => true;
export { local as remoteName } from "./dep" with { type: "json" };
import { value as importedValue } from "./dep" with { type: "json" };
export { importedValue };
export { local as renamed };
function real() {}
"#,
        );
        assert_eq!(
            import_attributes
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "fn:local", "export:renamed"]
        );
        assert_eq!(
            (
                import_attributes.anchors.get_str("fn:real").unwrap().start,
                import_attributes.anchors.get_str("fn:local").unwrap().start,
                import_attributes
                    .anchors
                    .get_str("export:renamed")
                    .unwrap()
                    .start,
            ),
            (6, 1, 1)
        );
        assert!(import_attributes
            .anchors
            .get_str("export:remoteName")
            .is_none());
        assert!(import_attributes
            .anchors
            .get_str("export:importedValue")
            .is_none());
    }

    #[test]
    fn matches_export_import_and_export_assignment_namespace_member_edges() {
        let file = RelativePath::new("src/export-variants.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"namespace Local {
  export const value = () => true;
  export function run() {}
  export class Box { open() {} }
}
export import ExportedAlias = Local;
export import Other = Local.Box;
export { ExportedAlias as AliasAgain };
export type { Other as OtherType };
export import Req = require(
  "dep"
);
export { Req, Req as ReqAgain };
export type { Req as ReqType };
export import Multi = Local
  .Box;
export { Multi as MultiAgain };
export import Duplicate = require("a");
export import Duplicate = require("b");
export { Duplicate as DuplicateAgain };
export = Local;
export as namespace Archiva;
function real() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "export:run",
                "export:value",
                "export:Box",
                "export:ExportedAlias",
                "export:Other",
                "export:AliasAgain",
                "export:OtherType",
                "export:Req",
                "export:ReqAgain",
                "export:ReqType",
                "export:Multi",
                "export:MultiAgain",
                "export:Duplicate",
                "export:DuplicateAgain",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("export:run").unwrap().start,
                extraction.anchors.get_str("export:value").unwrap().start,
                extraction.anchors.get_str("export:Box").unwrap().start,
            ),
            (3, 2, 4)
        );
        assert_eq!(
            vec![
                extraction
                    .anchors
                    .get_str("export:ExportedAlias")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("export:Other").unwrap().start,
                extraction
                    .anchors
                    .get_str("export:AliasAgain")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:OtherType")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("export:Req").unwrap().start,
                extraction.anchors.get_str("export:Req").unwrap().end,
                extraction.anchors.get_str("export:ReqAgain").unwrap().start,
                extraction.anchors.get_str("export:ReqType").unwrap().start,
                extraction.anchors.get_str("export:Multi").unwrap().start,
                extraction.anchors.get_str("export:Multi").unwrap().end,
                extraction
                    .anchors
                    .get_str("export:MultiAgain")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:Duplicate")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:DuplicateAgain")
                    .unwrap()
                    .start,
            ],
            vec![6, 7, 6, 7, 10, 12, 10, 10, 15, 16, 15, 18, 18]
        );
        assert!(extraction.anchors.get_str("export:Archiva").is_none());
        assert!(extraction.anchors.get_str("fn:Local.run").is_none());

        let export_import_type = extract_anchors(
            &RelativePath::new("src/export-import-type.ts").unwrap(),
            r#"export import type Alias = require("types");
export import type Bare;
export import type Qualified = NS.Sub;
export import type Multi = require(
  "dep"
);
export import type = require("weird");
function real() {}
"#,
        );
        assert_eq!(
            export_import_type
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "export:Alias",
                "export:Bare",
                "export:Qualified",
                "export:Multi",
                "export:type",
            ]
        );
        assert_eq!(
            (
                export_import_type
                    .anchors
                    .get_str("export:Alias")
                    .unwrap()
                    .start,
                export_import_type
                    .anchors
                    .get_str("export:Bare")
                    .unwrap()
                    .start,
                export_import_type
                    .anchors
                    .get_str("export:Qualified")
                    .unwrap()
                    .start,
                export_import_type
                    .anchors
                    .get_str("export:Multi")
                    .unwrap()
                    .start,
                export_import_type
                    .anchors
                    .get_str("export:Multi")
                    .unwrap()
                    .end,
                export_import_type
                    .anchors
                    .get_str("export:type")
                    .unwrap()
                    .start,
            ),
            (1, 2, 3, 4, 6, 7)
        );

        let malformed_export_import = extract_anchors(
            &RelativePath::new("src/export-import-malformed.ts").unwrap(),
            r#"export import Broken Local;
export import Bare;
export import FromLike from "dep";
export import Comma, Other = require("dep");
export { Broken as BrokenAgain, Bare as BareAgain, FromLike as FromAgain, Comma as CommaAgain };
function real() {}
"#,
        );
        assert_eq!(
            malformed_export_import
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "export:Broken",
                "export:Bare",
                "export:BrokenAgain",
                "export:BareAgain",
            ]
        );
        assert_eq!(
            vec![
                malformed_export_import
                    .anchors
                    .get_str("export:Broken")
                    .unwrap()
                    .start,
                malformed_export_import
                    .anchors
                    .get_str("export:Bare")
                    .unwrap()
                    .start,
                malformed_export_import
                    .anchors
                    .get_str("export:BrokenAgain")
                    .unwrap()
                    .start,
                malformed_export_import
                    .anchors
                    .get_str("export:BareAgain")
                    .unwrap()
                    .start,
            ],
            vec![1, 2, 1, 2]
        );
        for ghost in [
            "export:FromLike",
            "export:Comma",
            "export:FromAgain",
            "export:CommaAgain",
        ] {
            assert!(malformed_export_import.anchors.get_str(ghost).is_none());
        }

        let enum_export_assignment = extract_anchors(
            &RelativePath::new("src/enum-export-assignment.ts").unwrap(),
            r#"enum Local {
  A = 1 +
    2,
  B,
  "dash-name" = "x",
  1 = 1,
  [key] = 3
}
namespace Local { export function run() {} }
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            enum_export_assignment
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "export:A",
                "export:B",
                "export:dash-name",
                "export:1",
                "export:run",
            ]
        );
        assert_eq!(
            (
                enum_export_assignment
                    .anchors
                    .get_str("export:A")
                    .unwrap()
                    .start,
                enum_export_assignment
                    .anchors
                    .get_str("export:A")
                    .unwrap()
                    .end,
                enum_export_assignment
                    .anchors
                    .get_str("export:B")
                    .unwrap()
                    .start,
                enum_export_assignment
                    .anchors
                    .get_str("export:dash-name")
                    .unwrap()
                    .start,
                enum_export_assignment
                    .anchors
                    .get_str("export:1")
                    .unwrap()
                    .start,
                enum_export_assignment
                    .anchors
                    .get_str("export:run")
                    .unwrap()
                    .start,
            ),
            (2, 3, 4, 5, 6, 9)
        );
        assert!(enum_export_assignment
            .anchors
            .get_str("export:key")
            .is_none());

        let exported_enum_assignment = extract_anchors(
            &RelativePath::new("src/exported-enum-assignment.ts").unwrap(),
            "export enum Local { A }\nexport = Local;\nfunction real() {}\n",
        );
        assert_eq!(
            exported_enum_assignment
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:A", "export:Local"]
        );

        let export_assignment_order = extract_anchors(
            &RelativePath::new("src/export-assignment-order.ts").unwrap(),
            r#"export const other = 1;
namespace Local { export const value = 1; }
export = Local;
function after() {}
"#,
        );
        assert_eq!(
            export_assignment_order
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "export:value", "export:other"]
        );

        let exported_enum_merge_assignment = extract_anchors(
            &RelativePath::new("src/exported-enum-merge-assignment.ts").unwrap(),
            r#"export enum Local { A }
namespace Local { export const value = 1; }
export = Local;
function after() {}
"#,
        );
        assert_eq!(
            exported_enum_merge_assignment
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "export:value", "export:Local"]
        );
        assert!(exported_enum_merge_assignment
            .anchors
            .get_str("export:A")
            .is_none());

        let exported_namespace_merge_assignment = extract_anchors(
            &RelativePath::new("src/exported-namespace-merge-assignment.ts").unwrap(),
            r#"export namespace Local { export const self = 1; }
namespace Local { export const value = 1; }
export = Local;
function after() {}
"#,
        );
        assert_eq!(
            exported_namespace_merge_assignment
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "export:value", "export:Local"]
        );
        assert!(exported_namespace_merge_assignment
            .anchors
            .get_str("export:self")
            .is_none());

        let export_namespace_only_assignment = extract_anchors(
            &RelativePath::new("src/export-namespace-only-assignment.ts").unwrap(),
            r#"export namespace Local { export const self = 1; }
export = Local;
function after() {}
"#,
        );
        assert_eq!(
            export_namespace_only_assignment
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "export:self", "export:Local"]
        );

        let plain_enum_export_namespace_assignment = extract_anchors(
            &RelativePath::new("src/plain-enum-export-namespace-assignment.ts").unwrap(),
            r#"enum Local { A }
export namespace Local { export const value = 1; }
export = Local;
function after() {}
"#,
        );
        assert_eq!(
            plain_enum_export_namespace_assignment
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "export:A", "export:Local"]
        );
        assert!(plain_enum_export_namespace_assignment
            .anchors
            .get_str("export:value")
            .is_none());

        let exported_function_namespace_assignment = extract_anchors(
            &RelativePath::new("src/exported-function-namespace-assignment.ts").unwrap(),
            r#"export function Local() { return 1; }
namespace Local { export const value = 1; }
export = Local;
function after() {}
"#,
        );
        assert_eq!(
            exported_function_namespace_assignment
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:Local", "fn:after", "export:value", "export:Local"]
        );
        assert!(exported_function_namespace_assignment
            .anchors
            .get_str("export:default")
            .is_none());

        let exported_class_namespace_assignment = extract_anchors(
            &RelativePath::new("src/exported-class-namespace-assignment.ts").unwrap(),
            r#"export class Local { method() {} }
namespace Local { export const value = 1; }
export = Local;
function after() {}
"#,
        );
        assert_eq!(
            exported_class_namespace_assignment
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:after",
                "class:Local",
                "fn:Local.method",
                "export:value",
                "export:Local",
            ]
        );
        assert!(exported_class_namespace_assignment
            .anchors
            .get_str("export:default")
            .is_none());

        let default_class_namespace_assignment = extract_anchors(
            &RelativePath::new("src/default-class-namespace-assignment.ts").unwrap(),
            r#"export default class Local { method() {} }
namespace Local { export const value = 1; }
export = Local;
function after() {}
"#,
        );
        assert_eq!(
            default_class_namespace_assignment
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:after",
                "class:Local",
                "fn:Local.method",
                "export:value",
                "export:default",
            ]
        );
    }

    #[test]
    fn matches_dotted_namespace_and_namespace_export_alias_edges() {
        let file = RelativePath::new("src/ns-combined.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"namespace Local.Inner.Deep {
  export function deep() {}
}
namespace Local {
  const hidden = () => true;
  function secret() { if (a && b || c) return 1; }
  export { hidden, secret as revealed };
  export function run() {}
}
export = Local;
function real() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "export:Inner",
                "export:run",
                "export:hidden",
                "export:revealed",
                "block:if_a_b_c",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("export:Inner").unwrap().start,
                extraction.anchors.get_str("export:Inner").unwrap().end,
                extraction.anchors.get_str("export:run").unwrap().start,
                extraction.anchors.get_str("export:hidden").unwrap().start,
                extraction.anchors.get_str("export:revealed").unwrap().start,
                extraction
                    .anchors
                    .get_str("export:revealed")
                    .unwrap()
                    .complexity,
            ),
            (1, 3, 8, 5, 6, 4)
        );
        let block = extraction.anchors.get_str("block:if_a_b_c").unwrap();
        assert_eq!((block.start, block.end, block.complexity), (6, 6, 3));
        assert!(extraction.anchors.get_str("export:deep").is_none());
        assert!(extraction.anchors.get_str("fn:secret").is_none());
    }

    #[test]
    fn matches_merged_namespace_export_alias_edges() {
        let backward = extract_anchors(
            &RelativePath::new("src/ns-merge-exported-backward.ts").unwrap(),
            r#"namespace N {
  export class Box { open() {} }
}
namespace N {
  export { Box as Crate };
}
export = N;
function after() {}
"#,
        );
        assert_eq!(
            backward
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "export:Box", "export:Crate"]
        );
        assert_eq!(
            (
                backward.anchors.get_str("fn:after").unwrap().start,
                backward.anchors.get_str("export:Box").unwrap().start,
                backward.anchors.get_str("export:Crate").unwrap().start,
            ),
            (8, 2, 2)
        );

        let forward = extract_anchors(
            &RelativePath::new("src/ns-merge-exported-forward.ts").unwrap(),
            r#"namespace N {
  export { Box as Crate };
}
namespace N {
  export class Box { open() {} }
}
export = N;
function after() {}
"#,
        );
        assert_eq!(
            forward
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "export:Crate", "export:Box"]
        );
        assert_eq!(
            (
                forward.anchors.get_str("fn:after").unwrap().start,
                forward.anchors.get_str("export:Crate").unwrap().start,
                forward.anchors.get_str("export:Box").unwrap().start,
            ),
            (8, 5, 5)
        );

        let mixed = extract_anchors(
            &RelativePath::new("src/ns-merge-mixed.ts").unwrap(),
            r#"namespace N {
  class Hidden { method() {} }
  export class Box { open() {} }
  export const value = () => true;
}
namespace N {
  export { Hidden as Seen, Box as Crate, value as aliasValue };
}
export = N;
function after() {}
"#,
        );
        assert_eq!(
            mixed
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:after",
                "export:Box",
                "export:value",
                "export:Crate",
                "export:aliasValue",
            ]
        );
        assert_eq!(
            (
                mixed.anchors.get_str("fn:after").unwrap().start,
                mixed.anchors.get_str("export:Box").unwrap().start,
                mixed.anchors.get_str("export:value").unwrap().start,
                mixed.anchors.get_str("export:Crate").unwrap().start,
                mixed.anchors.get_str("export:aliasValue").unwrap().start,
            ),
            (10, 3, 4, 3, 4)
        );
        assert!(mixed.anchors.get_str("export:Seen").is_none());
        assert!(mixed.anchors.get_str("export:Hidden").is_none());
    }

    #[test]
    fn matches_namespace_alias_to_alias_export_edges() {
        let same_block = extract_anchors(
            &RelativePath::new("src/ns-alias-chain.ts").unwrap(),
            r#"namespace N {
  const value = 1;
  export { value as first };
  export { first as second };
}
export = N;
function after() {}
"#,
        );
        assert_eq!(
            same_block
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "export:first", "export:second"]
        );
        assert_eq!(
            (
                same_block.anchors.get_str("fn:after").unwrap().start,
                same_block.anchors.get_str("export:first").unwrap().start,
                same_block.anchors.get_str("export:second").unwrap().start,
            ),
            (7, 2, 2)
        );

        let forward = extract_anchors(
            &RelativePath::new("src/ns-merge-alias-forward.ts").unwrap(),
            r#"namespace N {
  export { first as second };
}
namespace N {
  export { value as first };
}
namespace N {
  export const value = 1;
}
export = N;
function after() {}
"#,
        );
        assert_eq!(
            forward
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "export:second", "export:first", "export:value"]
        );
        assert_eq!(
            (
                forward.anchors.get_str("fn:after").unwrap().start,
                forward.anchors.get_str("export:second").unwrap().start,
                forward.anchors.get_str("export:first").unwrap().start,
                forward.anchors.get_str("export:value").unwrap().start,
            ),
            (11, 8, 8, 8)
        );

        let direct_and_chain = extract_anchors(
            &RelativePath::new("src/ns-alias-direct-and-chain.ts").unwrap(),
            r#"namespace N {
  export const value = 1;
  export { value as first };
  export { value as second, first as third };
}
export = N;
function after() {}
"#,
        );
        assert_eq!(
            direct_and_chain
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:after",
                "export:value",
                "export:first",
                "export:second",
                "export:third",
            ]
        );
        assert_eq!(
            (
                direct_and_chain.anchors.get_str("fn:after").unwrap().start,
                direct_and_chain
                    .anchors
                    .get_str("export:value")
                    .unwrap()
                    .start,
                direct_and_chain
                    .anchors
                    .get_str("export:first")
                    .unwrap()
                    .start,
                direct_and_chain
                    .anchors
                    .get_str("export:second")
                    .unwrap()
                    .start,
                direct_and_chain
                    .anchors
                    .get_str("export:third")
                    .unwrap()
                    .start,
            ),
            (7, 2, 2, 2, 2)
        );

        let type_only_forward = extract_anchors(
            &RelativePath::new("src/ns-type-alias-forward.ts").unwrap(),
            r#"namespace N {
  export type { value as PublicValue };
}
namespace N {
  export const value = 1;
}
export = N;
function after() {}
"#,
        );
        assert_eq!(
            type_only_forward
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "export:PublicValue", "export:value"]
        );
        assert_eq!(
            (
                type_only_forward.anchors.get_str("fn:after").unwrap().start,
                type_only_forward
                    .anchors
                    .get_str("export:PublicValue")
                    .unwrap()
                    .start,
                type_only_forward
                    .anchors
                    .get_str("export:value")
                    .unwrap()
                    .start,
            ),
            (8, 5, 5)
        );
        assert!(type_only_forward.anchors.get_str("export:type").is_none());
    }

    #[test]
    fn matches_ambient_namespace_export_assignment_edges() {
        let implicit = extract_anchors(
            &RelativePath::new("src/ns-ambient.ts").unwrap(),
            r#"declare namespace Ambient {
  const value: number;
  function run(): void;
  class Box { open(): void; }
  interface Face { y: number }
  type Shape = { x: number };
}
export = Ambient;
function real() {}
"#,
        );
        assert_eq!(
            implicit
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "export:run",
                "export:value",
                "export:Box",
                "export:Face",
                "export:Shape",
            ]
        );
        assert_eq!(
            vec![
                implicit.anchors.get_str("fn:real").unwrap().start,
                implicit.anchors.get_str("export:run").unwrap().start,
                implicit.anchors.get_str("export:value").unwrap().start,
                implicit.anchors.get_str("export:Box").unwrap().start,
                implicit.anchors.get_str("export:Face").unwrap().start,
                implicit.anchors.get_str("export:Shape").unwrap().start,
            ],
            vec![9, 3, 2, 4, 5, 6]
        );

        let listed = extract_anchors(
            &RelativePath::new("src/ns-ambient-listed.ts").unwrap(),
            r#"declare namespace Listed {
  function hiddenRun(): void;
  const hidden: number;
  export { hidden as Hidden };
  export function explicit(): void;
}
export = Listed;
function real() {}
"#,
        );
        assert_eq!(
            listed
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:explicit", "export:Hidden"]
        );
        assert_eq!(
            (
                listed.anchors.get_str("fn:real").unwrap().start,
                listed.anchors.get_str("export:explicit").unwrap().start,
                listed.anchors.get_str("export:Hidden").unwrap().start,
            ),
            (8, 5, 3)
        );
        assert!(listed.anchors.get_str("export:hiddenRun").is_none());
        assert!(listed.anchors.get_str("export:hidden").is_none());

        let nested = extract_anchors(
            &RelativePath::new("src/ns-ambient-nested.ts").unwrap(),
            r#"declare namespace Local {
  namespace Inner { export function deep(): void; }
  namespace Plain { function hidden(): void; }
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            nested
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:Inner", "export:Plain"]
        );
        assert_eq!(
            vec![
                nested.anchors.get_str("fn:real").unwrap().start,
                nested.anchors.get_str("export:Inner").unwrap().start,
                nested.anchors.get_str("export:Plain").unwrap().start,
            ],
            vec![6, 2, 3]
        );
        assert!(nested.anchors.get_str("export:deep").is_none());
        assert!(nested.anchors.get_str("export:hidden").is_none());

        let nested_listed = extract_anchors(
            &RelativePath::new("src/ns-ambient-nested-listed.ts").unwrap(),
            r#"declare namespace Local {
  namespace Inner {
    const hidden: number;
    export { hidden as Hidden };
  }
  function run(): void;
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            nested_listed
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:run", "export:Inner"]
        );
        assert_eq!(
            vec![
                nested_listed.anchors.get_str("fn:real").unwrap().start,
                nested_listed.anchors.get_str("export:run").unwrap().start,
                nested_listed.anchors.get_str("export:Inner").unwrap().start,
                nested_listed.anchors.get_str("export:Inner").unwrap().end,
            ],
            vec![9, 6, 2, 5]
        );
        assert!(nested_listed.anchors.get_str("export:Hidden").is_none());

        let module = extract_anchors(
            &RelativePath::new("src/ns-ambient-module.ts").unwrap(),
            r#"declare module "pkg" {
  export function ghost(): void;
  export const ghostValue: number;
}
declare module Local {
  export function run(): void;
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            module
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:run"]
        );
        assert_eq!(
            (
                module.anchors.get_str("fn:real").unwrap().start,
                module.anchors.get_str("export:run").unwrap().start,
            ),
            (9, 6)
        );
        assert!(module.anchors.get_str("export:ghost").is_none());
        assert!(module.anchors.get_str("export:ghostValue").is_none());

        let merge_backward = extract_anchors(
            &RelativePath::new("src/ns-ambient-merge-backward.ts").unwrap(),
            r#"declare namespace Local {
  const hidden: number;
}
declare namespace Local {
  export { hidden as Hidden };
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            merge_backward
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:hidden", "export:Hidden"]
        );
        assert_eq!(
            vec![
                merge_backward.anchors.get_str("fn:real").unwrap().start,
                merge_backward
                    .anchors
                    .get_str("export:hidden")
                    .unwrap()
                    .start,
                merge_backward
                    .anchors
                    .get_str("export:Hidden")
                    .unwrap()
                    .start,
            ],
            vec![8, 2, 2]
        );

        let merge_forward = extract_anchors(
            &RelativePath::new("src/ns-ambient-merge-forward.ts").unwrap(),
            r#"declare namespace Local {
  export { hidden as Hidden };
}
declare namespace Local {
  const hidden: number;
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            merge_forward
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:Hidden", "export:hidden"]
        );
        assert_eq!(
            vec![
                merge_forward.anchors.get_str("fn:real").unwrap().start,
                merge_forward
                    .anchors
                    .get_str("export:Hidden")
                    .unwrap()
                    .start,
                merge_forward
                    .anchors
                    .get_str("export:hidden")
                    .unwrap()
                    .start,
            ],
            vec![8, 5, 5]
        );

        let type_only_merge_forward = extract_anchors(
            &RelativePath::new("src/ns-ambient-type-alias-forward.ts").unwrap(),
            r#"declare namespace Local {
  export type { Shape as PublicShape };
}
declare namespace Local {
  interface Shape { x: number }
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            type_only_merge_forward
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:PublicShape", "export:Shape"]
        );
        assert_eq!(
            (
                type_only_merge_forward
                    .anchors
                    .get_str("fn:real")
                    .unwrap()
                    .start,
                type_only_merge_forward
                    .anchors
                    .get_str("export:PublicShape")
                    .unwrap()
                    .start,
                type_only_merge_forward
                    .anchors
                    .get_str("export:Shape")
                    .unwrap()
                    .start,
            ),
            (8, 5, 5)
        );
        assert!(type_only_merge_forward
            .anchors
            .get_str("export:type")
            .is_none());
    }

    #[test]
    fn matches_namespace_multi_variable_and_type_only_export_alias_edges() {
        let file = RelativePath::new("src/ns-multivar.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"namespace Local {
  const first = () => true, second = () => false;
  let count = 1, total = 2;
  type Shape = { x: number };
  interface Face { y: number }
  export { first, second as renamedSecond, count, total as renamedTotal, type Shape, Face as RenamedFace };
  export const direct = () => true, other = () => false;
  export { direct as aliasDirect, other as aliasOther };
}
export = Local;
function real() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "export:first",
                "export:renamedSecond",
                "export:count",
                "export:renamedTotal",
                "export:Shape",
                "export:RenamedFace",
                "export:direct",
                "export:other",
                "export:aliasDirect",
                "export:aliasOther",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("export:first").unwrap().start,
                extraction
                    .anchors
                    .get_str("export:renamedSecond")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("export:count").unwrap().start,
                extraction
                    .anchors
                    .get_str("export:renamedTotal")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("export:Shape").unwrap().start,
                extraction
                    .anchors
                    .get_str("export:RenamedFace")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("export:direct").unwrap().start,
                extraction.anchors.get_str("export:other").unwrap().start,
                extraction
                    .anchors
                    .get_str("export:aliasDirect")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:aliasOther")
                    .unwrap()
                    .start,
            ),
            (2, 2, 3, 3, 4, 5, 7, 7, 7, 7)
        );
        assert!(extraction.anchors.get_str("export:second").is_none());
        assert!(extraction.anchors.get_str("export:total").is_none());
        assert!(extraction.anchors.get_str("export:Face").is_none());
    }

    #[test]
    fn matches_namespace_destructuring_export_assignment_edges() {
        let file = RelativePath::new("src/ns-destructure.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"namespace Local {
  const { first, second: renamedLocal } = source;
  const [head, tail] = values;
  export const { direct, alias: directAlias } = source;
  export const [directHead, directTail] = values;
  const { outer: { inner }, ...rest } = source;
  const [nestedHead, ...others] = values;
  export { first, renamedLocal as exportedSecond, head, tail as exportedTail, inner, rest, nestedHead, others };
}
export = Local;
function real() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "export:direct",
                "export:directAlias",
                "export:directHead",
                "export:directTail",
                "export:first",
                "export:exportedSecond",
                "export:head",
                "export:exportedTail",
                "export:inner",
                "export:rest",
                "export:nestedHead",
                "export:others",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("export:direct").unwrap().start,
                extraction
                    .anchors
                    .get_str("export:directAlias")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:directHead")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:directTail")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("export:first").unwrap().start,
                extraction
                    .anchors
                    .get_str("export:exportedSecond")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("export:head").unwrap().start,
                extraction
                    .anchors
                    .get_str("export:exportedTail")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("export:inner").unwrap().start,
                extraction.anchors.get_str("export:rest").unwrap().start,
                extraction
                    .anchors
                    .get_str("export:nestedHead")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("export:others").unwrap().start,
            ),
            (4, 4, 5, 5, 2, 2, 3, 3, 6, 6, 7, 7)
        );
        assert!(extraction.anchors.get_str("export:renamedLocal").is_none());
        assert!(extraction.anchors.get_str("export:tail").is_none());
        assert!(extraction.anchors.get_str("export:alias").is_none());
    }

    #[test]
    fn matches_namespace_binding_default_initializer_edges() {
        let file = RelativePath::new("src/ns-binding-defaults.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"namespace Local {
  const { [dynamicKey]: computedValue = fallback(a, b), plain = make(c, d), nested: { inner = other(e, f) } = defaults } = source;
  const [head = pair(g, h), , tail = makeTail(i, j)] = values;
  export const { directComputed = call(k, l), directPlain = make(m, n) } = source;
  export const [directHead = pair(o, p), directTail = makeTail(q, r)] = values;
  export { computedValue, plain, inner, head, tail };
}
export = Local;
function real() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "export:directComputed",
                "export:directPlain",
                "export:directHead",
                "export:directTail",
                "export:computedValue",
                "export:plain",
                "export:inner",
                "export:head",
                "export:tail",
            ]
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("export:directComputed")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:directPlain")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:directHead")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:directTail")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:computedValue")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("export:plain").unwrap().start,
                extraction.anchors.get_str("export:inner").unwrap().start,
                extraction.anchors.get_str("export:head").unwrap().start,
                extraction.anchors.get_str("export:tail").unwrap().start,
            ),
            (4, 4, 5, 5, 2, 2, 2, 3, 3)
        );
        for ghost in [
            "export:b", "export:d", "export:f", "export:h", "export:j", "export:l", "export:n",
            "export:p", "export:r",
        ] {
            assert!(extraction.anchors.get_str(ghost).is_none());
        }
    }

    #[test]
    fn matches_quoted_export_alias_suppression_edges() {
        let namespace_file = RelativePath::new("src/ns-quoted-alias.ts").unwrap();
        let namespace = extract_anchors(
            &namespace_file,
            r#"namespace Local {
  const value = 1;
  const keep = 2;
  function run() {}
  type Shape = { x: number };
  export { keep as kept, value as "dash-name", run as "call-run", type Shape as "shape-type" };
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            namespace
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real"]
        );
        for ghost in [
            "export:value",
            "export:keep",
            "export:kept",
            "export:run",
            "export:Shape",
            "export:dash-name",
            "export:call-run",
            "export:shape-type",
        ] {
            assert!(namespace.anchors.get_str(ghost).is_none());
        }

        let top_level_file = RelativePath::new("src/top-quoted-alias.ts").unwrap();
        let top_level = extract_anchors(
            &top_level_file,
            r#"const value = 1;
const keep = 2;
function run() {}
export { keep as kept, value as "dash-name" };
"#,
        );
        assert_eq!(
            top_level
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:run"]
        );
        assert!(top_level.anchors.get_str("export:kept").is_none());
        assert!(top_level.anchors.get_str("export:value").is_none());
    }

    #[test]
    fn matches_missing_export_alias_target_edges() {
        let top_level = extract_anchors(
            &RelativePath::new("src/top-missing-alias.ts").unwrap(),
            r#"const keep = 1;
const other = 2;
export { keep as, other as valid };
function real() {}
"#,
        );
        assert_eq!(
            top_level
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:", "export:valid"]
        );
        assert_eq!(
            vec![
                top_level.anchors.get_str("fn:real").unwrap().start,
                top_level.anchors.get_str("export:").unwrap().start,
                top_level.anchors.get_str("export:valid").unwrap().start,
            ],
            vec![4, 1, 2]
        );
        assert!(top_level.anchors.get_str("export:keep").is_none());
        assert!(top_level.anchors.get_str("export:other").is_none());

        let namespace = extract_anchors(
            &RelativePath::new("src/ns-missing-alias.ts").unwrap(),
            r#"namespace Local {
  const keep = 1;
  const other = 2;
  export { keep as, other as valid };
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            namespace
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:", "export:valid"]
        );
        assert_eq!(
            vec![
                namespace.anchors.get_str("fn:real").unwrap().start,
                namespace.anchors.get_str("export:").unwrap().start,
                namespace.anchors.get_str("export:valid").unwrap().start,
            ],
            vec![7, 2, 3]
        );
        assert!(namespace.anchors.get_str("export:keep").is_none());
        assert!(namespace.anchors.get_str("export:other").is_none());

        let comma_hole = extract_anchors(
            &RelativePath::new("src/ns-alias-comma-hole.ts").unwrap(),
            r#"namespace Local {
  const keep = 1;
  const other = 2;
  export { , keep, other as valid };
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            comma_hole
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:keep", "export:valid"]
        );
        assert!(comma_hole.anchors.get_str("export:").is_none());

        let numeric_target = extract_anchors(
            &RelativePath::new("src/ns-numeric-alias.ts").unwrap(),
            r#"namespace Local {
  const keep = 1;
  const other = 2;
  export { keep as 123, other as valid };
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            numeric_target
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:"]
        );
        assert_eq!(
            (
                numeric_target.anchors.get_str("fn:real").unwrap().start,
                numeric_target.anchors.get_str("export:").unwrap().start,
            ),
            (7, 2)
        );
        assert!(numeric_target.anchors.get_str("export:valid").is_none());

        let numeric_after_valid = extract_anchors(
            &RelativePath::new("src/top-numeric-alias-after-valid.ts").unwrap(),
            r#"const keep = 1;
const other = 2;
export { other as valid, keep as 123 };
function real() {}
"#,
        );
        assert_eq!(
            numeric_after_valid
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:valid", "export:"]
        );

        let template_target = extract_anchors(
            &RelativePath::new("src/top-template-alias.ts").unwrap(),
            r#"const keep = 1;
const other = 2;
export { keep as `dash`, other as valid };
function real() {}
"#,
        );
        assert_eq!(
            template_target
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:"]
        );
        assert!(template_target.anchors.get_str("export:valid").is_none());

        let dot_target = extract_anchors(
            &RelativePath::new("src/ns-dot-alias.ts").unwrap(),
            r#"namespace Local {
  const keep = 1;
  const other = 2;
  export { keep as ., other as valid };
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            dot_target
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:", "export:valid"]
        );

        let punctuation_targets = extract_anchors(
            &RelativePath::new("src/top-punctuation-alias.ts").unwrap(),
            r#"const q = 1;
const c = 2;
const p = 3;
const b = 4;
const other = 5;
export { q as ?, c as :, p as ), b as ], other as valid };
function real() {}
"#,
        );
        assert_eq!(
            punctuation_targets
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:", "export:valid"]
        );
        assert_eq!(
            (
                punctuation_targets
                    .anchors
                    .get_str("export:")
                    .unwrap()
                    .start,
                punctuation_targets
                    .anchors
                    .get_str("export:valid")
                    .unwrap()
                    .start,
            ),
            (1, 5)
        );

        let namespace_punctuation_targets = extract_anchors(
            &RelativePath::new("src/ns-punctuation-alias.ts").unwrap(),
            r#"namespace Local {
  const q = 1;
  const c = 2;
  const p = 3;
  const b = 4;
  const other = 5;
  export { q as ?, c as :, p as ), b as ], other as valid };
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            namespace_punctuation_targets
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:", "export:valid"]
        );
        assert_eq!(
            (
                namespace_punctuation_targets
                    .anchors
                    .get_str("export:")
                    .unwrap()
                    .start,
                namespace_punctuation_targets
                    .anchors
                    .get_str("export:valid")
                    .unwrap()
                    .start,
            ),
            (2, 6)
        );

        let private_name_target = extract_anchors(
            &RelativePath::new("src/top-private-name-alias.ts").unwrap(),
            r#"const keep = 1;
const other = 2;
export { keep as #foo, other as Valid };
function real() {}
"#,
        );
        assert_eq!(
            private_name_target
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:#foo", "export:Valid"]
        );
        assert_eq!(
            (
                private_name_target
                    .anchors
                    .get_str("export:#foo")
                    .unwrap()
                    .start,
                private_name_target
                    .anchors
                    .get_str("export:Valid")
                    .unwrap()
                    .start,
            ),
            (1, 2)
        );

        let namespace_private_name_target = extract_anchors(
            &RelativePath::new("src/ns-private-name-alias.ts").unwrap(),
            r#"namespace Local {
  const keep = 1;
  const other = 2;
  export { keep as #foo, other as Valid };
}
export = Local;
function real() {}
"#,
        );
        assert_eq!(
            namespace_private_name_target
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:#foo", "export:Valid"]
        );
        assert_eq!(
            (
                namespace_private_name_target
                    .anchors
                    .get_str("export:#foo")
                    .unwrap()
                    .start,
                namespace_private_name_target
                    .anchors
                    .get_str("export:Valid")
                    .unwrap()
                    .start,
            ),
            (2, 3)
        );
    }

    #[test]
    fn matches_literal_method_names_and_type_only_export_aliases() {
        let file = RelativePath::new("src/literal-methods.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"interface Foo {}
type Bar = string;
export type { Foo as RenamedFoo, Bar };
export { type Foo as TypeFoo };
class Names {
  "quoted"() { return 1; }
  42() { return 2; }
  static "static-name"() { return 3; }
  get "value"() { return 4; }
  [computed]() { return 5; }
}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "class:Names",
                "fn:Names.\"quoted\"",
                "fn:Names.42",
                "fn:Names.\"static-name\"",
                "fn:Names.[computed]",
                "export:RenamedFoo",
                "export:Bar",
                "export:TypeFoo",
            ]
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("fn:Names.\"quoted\"")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("fn:Names.42").unwrap().start,
            ),
            (6, 7)
        );
        assert_eq!(
            extraction
                .anchors
                .get_str("fn:Names.\"static-name\"")
                .unwrap()
                .start,
            8
        );
        assert!(extraction.anchors.get_str("fn:Names.\"value\"").is_none());
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("export:RenamedFoo")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("export:Bar").unwrap().start,
                extraction.anchors.get_str("export:TypeFoo").unwrap().start,
            ),
            (1, 2, 1)
        );
    }

    #[test]
    fn matches_escaped_numeric_and_computed_method_name_edges() {
        let file = RelativePath::new("src/literal-edge-methods.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"class Numeric {
  "with\\slash"() {}
  "with\"quote"() {}
  3.14() {}
  1_000() {}
  0x10() {}
  1e3() {}
}
class Computed {
  ["literal"]() {}
  [1 + 2]() {}
  [Symbol.iterator]() {}
  [bad ? { x: 1 } : key]() {}
}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "class:Numeric",
                "fn:Numeric.\"with\\\\slash\"",
                "fn:Numeric.\"with\\\"quote\"",
                "fn:Numeric.3.14",
                "fn:Numeric.1_000",
                "fn:Numeric.0x10",
                "fn:Numeric.1e3",
                "class:Computed",
                "fn:Computed.[\"literal\"]",
                "fn:Computed.[1 + 2]",
                "fn:Computed.[Symbol.iterator]",
                "fn:Computed.[bad ? { x: 1 } : key]",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:Numeric.3.14").unwrap().start,
                extraction
                    .anchors
                    .get_str("fn:Numeric.1_000")
                    .unwrap()
                    .start,
            ),
            (4, 5)
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:Numeric.0x10").unwrap().start,
                extraction.anchors.get_str("fn:Numeric.1e3").unwrap().start,
            ),
            (6, 7)
        );
        assert_eq!(
            extraction
                .anchors
                .get_str("fn:Computed.[1 + 2]")
                .unwrap()
                .start,
            11
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("fn:Computed.[bad ? { x: 1 } : key]")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("fn:Computed.[bad ? { x: 1 } : key]")
                    .unwrap()
                    .complexity,
            ),
            (13, 2)
        );
    }

    #[test]
    fn matches_malformed_bigint_like_class_element_recovery() {
        let file = RelativePath::new("src/bigint-recovery.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"class Broken {
  before() {}
  10n() {}
  after() {}
}
function later() { return 1; }
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:later", "class:Broken", "fn:Broken.before"]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("class:Broken").unwrap().start,
                extraction.anchors.get_str("class:Broken").unwrap().end,
            ),
            (1, 2)
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("fn:Broken.before")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("fn:Broken.before").unwrap().end,
            ),
            (2, 2)
        );
        assert!(extraction.anchors.get_str("fn:Broken.n").is_none());
        assert!(extraction.anchors.get_str("fn:Broken.after").is_none());

        let computed = extract_anchors(
            &RelativePath::new("src/computed-recovery.ts").unwrap(),
            r#"class Broken {
  before() {}
  [bad() {}
  after() {}
}
function later() {}
"#,
        );
        assert_eq!(
            computed
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:later", "class:Broken", "fn:Broken.before"]
        );
        assert_eq!(
            (
                computed.anchors.get_str("class:Broken").unwrap().start,
                computed.anchors.get_str("class:Broken").unwrap().end,
                computed.anchors.get_str("fn:Broken.before").unwrap().start,
            ),
            (1, 3, 2)
        );
        assert!(computed.anchors.get_str("fn:Broken.bad").is_none());
        assert!(computed.anchors.get_str("fn:Broken.after").is_none());

        let method_params = extract_anchors(
            &RelativePath::new("src/method-params-recovery.ts").unwrap(),
            r#"class Broken {
  before() {}
  broken(a, b {
    return 1;
  }
  after() {}
}
function later() {}
"#,
        );
        assert_eq!(
            method_params
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:later",
                "class:Broken",
                "fn:Broken.before",
                "fn:Broken.broken",
            ]
        );
        assert_eq!(
            (
                method_params.anchors.get_str("class:Broken").unwrap().start,
                method_params.anchors.get_str("class:Broken").unwrap().end,
                method_params
                    .anchors
                    .get_str("fn:Broken.broken")
                    .unwrap()
                    .start,
                method_params
                    .anchors
                    .get_str("fn:Broken.broken")
                    .unwrap()
                    .end,
            ),
            (1, 6, 3, 6)
        );
        assert!(method_params.anchors.get_str("fn:Broken.after").is_none());
    }

    #[test]
    fn matches_unclosed_function_body_recovery() {
        let function_file = RelativePath::new("src/unclosed-function.ts").unwrap();
        let function = extract_anchors(
            &function_file,
            r#"function broken() {
  if (a && b) return 1;
function later() { return 2; }
"#,
        );

        assert_eq!(
            function
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:broken"]
        );
        let broken = function.anchors.get_str("fn:broken").unwrap();
        assert_eq!((broken.start, broken.end, broken.complexity), (1, 3, 3));
        assert!(function.anchors.get_str("fn:later").is_none());

        let arrow_file = RelativePath::new("src/unclosed-arrow.ts").unwrap();
        let arrow = extract_anchors(
            &arrow_file,
            r#"const broken = () => {
  if (a && b) return 1;
function later() { return 2; }
"#,
        );

        assert_eq!(
            arrow
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:broken"]
        );
        let broken = arrow.anchors.get_str("fn:broken").unwrap();
        assert_eq!((broken.start, broken.end, broken.complexity), (1, 3, 3));
        assert!(arrow.anchors.get_str("fn:later").is_none());
    }

    #[test]
    fn suppresses_nested_arrow_initializer_parser_ghosts() {
        for source in [
            "const x = foo(() => true);\nfunction after() {}\n",
            "const x = (() => true)();\nfunction after() {}\n",
            "const x = { run: () => true };\nfunction after() {}\n",
        ] {
            let extraction =
                extract_anchors(&RelativePath::new("src/nested-arrow.ts").unwrap(), source);
            assert_eq!(
                extraction
                    .anchors
                    .iter()
                    .map(|(anchor, _)| anchor.as_str())
                    .collect::<Vec<_>>(),
                vec!["fn:after"]
            );
            assert!(extraction.anchors.get_str("fn:x").is_none());
        }
    }

    #[test]
    fn matches_optional_class_method_edges() {
        let extraction = extract_anchors(
            &RelativePath::new("src/optional-methods.ts").unwrap(),
            r#"class C {
  optional?() { return 1; }
  required() { return 2; }
  "quoted"?() { return 3; }
  42?() { return 4; }
  [computed]?() { return 5; }
  #secret?() { return 6; }
}
abstract class AbstractC { abstract optional?(): void; abstract required(): void; concrete() {} }
declare class DeclaredC { optional?(): void; required(): void; }
class SignatureOnly { optional?(): void; required(): void; concrete() {} }
function after() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:after",
                "class:C",
                "fn:C.optional",
                "fn:C.required",
                "fn:C.\"quoted\"",
                "fn:C.42",
                "fn:C.[computed]",
                "fn:C.#secret",
                "class:AbstractC",
                "fn:AbstractC.optional",
                "fn:AbstractC.required",
                "fn:AbstractC.concrete",
                "class:DeclaredC",
                "fn:DeclaredC.optional",
                "fn:DeclaredC.required",
                "class:SignatureOnly",
                "fn:SignatureOnly.concrete",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:after").unwrap().start,
                extraction.anchors.get_str("class:C").unwrap().start,
                extraction.anchors.get_str("class:C").unwrap().end,
                extraction.anchors.get_str("fn:C.optional").unwrap().start,
                extraction.anchors.get_str("fn:C.[computed]").unwrap().start,
                extraction
                    .anchors
                    .get_str("fn:AbstractC.optional")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("fn:DeclaredC.optional")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("fn:SignatureOnly.concrete")
                    .unwrap()
                    .start,
            ),
            (12, 1, 8, 2, 6, 9, 10, 11)
        );
        assert!(extraction
            .anchors
            .get_str("fn:SignatureOnly.optional")
            .is_none());
        assert!(extraction
            .anchors
            .get_str("fn:SignatureOnly.required")
            .is_none());
    }

    #[test]
    fn matches_function_expression_variable_initializer_edges() {
        let extraction = extract_anchors(
            &RelativePath::new("src/function-expressions.ts").unwrap(),
            r#"const plain = function() { return 1; };
const named = function hidden() { return 2; };
const asyncNamed = async function hiddenAsync() { await run(); };
export const exported = async function exportedHidden() { await run(); };
const paren = (function parenHidden() { return 3; });
const call = foo(function callHidden() { return 4; });
function real() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "fn:plain",
                "fn:named",
                "fn:asyncNamed",
                "fn:exported",
                "export:exported",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:plain").unwrap().start,
                extraction.anchors.get_str("fn:named").unwrap().start,
                extraction.anchors.get_str("fn:asyncNamed").unwrap().start,
                extraction.anchors.get_str("fn:exported").unwrap().start,
                extraction.anchors.get_str("export:exported").unwrap().start,
            ),
            (1, 2, 3, 4, 4)
        );
        for ghost in [
            "fn:hidden",
            "fn:hiddenAsync",
            "fn:exportedHidden",
            "fn:paren",
            "fn:parenHidden",
            "fn:call",
            "fn:callHidden",
        ] {
            assert!(extraction.anchors.get_str(ghost).is_none());
        }
    }

    #[test]
    fn suppresses_type_operator_wrapped_arrow_initializer_anchors() {
        let extraction = extract_anchors(
            &RelativePath::new("src/type-operator-wrapped-arrows.ts").unwrap(),
            r#"const satisfiesRun = ((x: number) => x) satisfies (x: number) => number;
const asRun = ((x: number) => x) as (x: number) => number;
export const exported = ((x: number) => x) satisfies (x: number) => number;
const direct = (x: number) => x satisfies number;
function after() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "fn:direct", "export:exported"]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:direct").unwrap().start,
                extraction.anchors.get_str("export:exported").unwrap().start,
                extraction.anchors.get_str("fn:after").unwrap().start,
            ),
            (4, 3, 5)
        );
        for ghost in ["fn:satisfiesRun", "fn:asRun", "fn:exported"] {
            assert!(extraction.anchors.get_str(ghost).is_none());
        }
    }

    #[test]
    fn matches_typed_variable_initializer_edges() {
        let top_level = extract_anchors(
            &RelativePath::new("src/typed-initializers.ts").unwrap(),
            r#"const objectTyped: { a: number, b: number } = () => true;
const tupleTyped: [number, string] = function hiddenTuple() { return [1, "x"]; };
export const genericTyped: Promise<string | number> = async () => "x";
const unionTyped: (() => number) | null = () => 1;
const plain = () => true;
function after() {}
"#,
        );

        assert_eq!(
            top_level
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:after",
                "fn:objectTyped",
                "fn:tupleTyped",
                "fn:genericTyped",
                "fn:unionTyped",
                "fn:plain",
                "export:genericTyped",
            ]
        );
        assert_eq!(
            (
                top_level.anchors.get_str("fn:objectTyped").unwrap().start,
                top_level.anchors.get_str("fn:tupleTyped").unwrap().start,
                top_level.anchors.get_str("fn:genericTyped").unwrap().start,
                top_level
                    .anchors
                    .get_str("export:genericTyped")
                    .unwrap()
                    .start,
                top_level.anchors.get_str("fn:unionTyped").unwrap().start,
                top_level.anchors.get_str("fn:plain").unwrap().start,
                top_level.anchors.get_str("fn:after").unwrap().start,
            ),
            (1, 2, 3, 3, 4, 5, 6)
        );
        for ghost in ["fn:b", "fn:string", "fn:hiddenTuple"] {
            assert!(top_level.anchors.get_str(ghost).is_none());
        }

        let namespace = extract_anchors(
            &RelativePath::new("src/ns-typed-initializers.ts").unwrap(),
            r#"namespace Local {
  const objectTyped: { a: number, b: number } = () => true;
  const tupleTyped: [number, string] = function hiddenTuple() { return [1, "x"]; };
  export { objectTyped, tupleTyped };
}
export = Local;
function after() {}
"#,
        );

        assert_eq!(
            namespace
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "export:objectTyped", "export:tupleTyped"]
        );
        assert_eq!(
            (
                namespace.anchors.get_str("fn:after").unwrap().start,
                namespace
                    .anchors
                    .get_str("export:objectTyped")
                    .unwrap()
                    .start,
                namespace
                    .anchors
                    .get_str("export:tupleTyped")
                    .unwrap()
                    .start,
            ),
            (7, 2, 3)
        );
        for ghost in ["export:b", "export:string", "fn:hiddenTuple"] {
            assert!(namespace.anchors.get_str(ghost).is_none());
        }
    }

    #[test]
    fn matches_type_question_complexity_edges() {
        let extraction = extract_anchors(
            &RelativePath::new("src/type-question-complexity.ts").unwrap(),
            r#"function optionalParam(x?: string) { return x; }
const optionalArrow = (x?: string) => x;
function conditionalType(x: T extends string ? A : B) { return x; }
const conditionalArrow = (x: T extends string ? A : B) => x;
function defaultParam(x = flag ? 1 : 2) { return x; }
const defaultArrow = (x = flag ? 1 : 2) => x;
class C { optional?() { return 1; } method(x?: T, y: T extends string ? A : B) { return y; } }
class Computed { [bad ? { x: 1 } : key]() {} }
function optionalChain(x: any) { return x?.value; }
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:optionalParam",
                "fn:conditionalType",
                "fn:defaultParam",
                "fn:optionalChain",
                "class:C",
                "fn:C.optional",
                "fn:C.method",
                "class:Computed",
                "fn:Computed.[bad ? { x: 1 } : key]",
                "fn:optionalArrow",
                "fn:conditionalArrow",
                "fn:defaultArrow",
            ]
        );
        assert_eq!(
            (
                extraction
                    .anchors
                    .get_str("fn:optionalParam")
                    .unwrap()
                    .complexity,
                extraction
                    .anchors
                    .get_str("fn:optionalArrow")
                    .unwrap()
                    .complexity,
                extraction
                    .anchors
                    .get_str("fn:conditionalType")
                    .unwrap()
                    .complexity,
                extraction
                    .anchors
                    .get_str("fn:conditionalArrow")
                    .unwrap()
                    .complexity,
                extraction.anchors.get_str("class:C").unwrap().complexity,
                extraction
                    .anchors
                    .get_str("fn:C.method")
                    .unwrap()
                    .complexity,
                extraction
                    .anchors
                    .get_str("fn:defaultParam")
                    .unwrap()
                    .complexity,
                extraction
                    .anchors
                    .get_str("fn:defaultArrow")
                    .unwrap()
                    .complexity,
                extraction
                    .anchors
                    .get_str("fn:Computed.[bad ? { x: 1 } : key]")
                    .unwrap()
                    .complexity,
                extraction
                    .anchors
                    .get_str("fn:optionalChain")
                    .unwrap()
                    .complexity,
            ),
            (1, 1, 1, 1, 1, 1, 2, 2, 2, 1)
        );
    }

    #[test]
    fn matches_using_declaration_function_initializer_edges() {
        let extraction = extract_anchors(
            &RelativePath::new("src/using-declarations.ts").unwrap(),
            r#"using disposable = () => true;
await using asyncDisposable = async () => true;
export using exportedDisposable = function hidden() { return true; };
export await using exportedAsyncDisposable = async function hiddenAsync() { return true; };
using resource = acquire();
function after() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:after",
                "fn:disposable",
                "fn:asyncDisposable",
                "fn:exportedDisposable",
                "fn:exportedAsyncDisposable",
                "export:exportedDisposable",
                "export:exportedAsyncDisposable",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:disposable").unwrap().start,
                extraction
                    .anchors
                    .get_str("fn:asyncDisposable")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("fn:exportedDisposable")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("fn:exportedAsyncDisposable")
                    .unwrap()
                    .start,
                extraction
                    .anchors
                    .get_str("export:exportedAsyncDisposable")
                    .unwrap()
                    .start,
                extraction.anchors.get_str("fn:after").unwrap().start,
            ),
            (1, 2, 3, 4, 4, 6)
        );
        for ghost in [
            "fn:hidden",
            "fn:hiddenAsync",
            "fn:resource",
            "export:resource",
        ] {
            assert!(extraction.anchors.get_str(ghost).is_none());
        }
    }

    #[test]
    fn suppresses_destructuring_function_initializer_anchors() {
        let exported = extract_anchors(
            &RelativePath::new("src/destructured-functions.ts").unwrap(),
            r#"export const {
  a = function hidden() {
    if (x) return 1;
    return 0;
  },
  b = () => {
    if (y && z) return 2;
    return 0;
  },
  c,
  source: alias,
  nested: { deep = () => true },
  ...rest
} = obj;
export const [first = function hiddenArray() {}, second = () => true, third] = arr;
function real() {}
"#,
        );
        assert_eq!(
            exported
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:real",
                "export:a",
                "export:b",
                "export:c",
                "export:alias",
                "export:deep",
                "export:rest",
                "export:first",
                "export:second",
                "export:third",
            ]
        );
        assert_eq!(
            (
                exported.anchors.get_str("export:a").unwrap().start,
                exported.anchors.get_str("export:a").unwrap().end,
                exported.anchors.get_str("export:a").unwrap().complexity,
                exported.anchors.get_str("export:b").unwrap().start,
                exported.anchors.get_str("export:b").unwrap().end,
                exported.anchors.get_str("export:b").unwrap().complexity,
            ),
            (2, 5, 2, 6, 9, 3)
        );
        for ghost in [
            "fn:a",
            "fn:b",
            "fn:first",
            "fn:second",
            "fn:hidden",
            "fn:hiddenArray",
            "fn:deep",
            "fn:nested",
        ] {
            assert!(exported.anchors.get_str(ghost).is_none());
        }

        let local = extract_anchors(
            &RelativePath::new("src/local-destructured-functions.ts").unwrap(),
            "const { a = function hidden() {}, b = () => true } = obj;\nconst [c = function hiddenArray() {}, d = () => true] = arr;\nfunction real() {}\n",
        );
        assert_eq!(
            local
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real"]
        );

        let aliases = extract_anchors(
            &RelativePath::new("src/destructured-aliases.ts").unwrap(),
            "const { a, b = () => true } = obj;\nconst [c, d = function hidden() {}] = arr;\nexport { a, b as bee, c, d as dee };\n",
        );
        assert_eq!(
            aliases
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["export:a", "export:bee", "export:c", "export:dee"]
        );
    }

    #[test]
    fn matches_unicode_identifier_anchor_edges() {
        let extraction = extract_anchors(
            &RelativePath::new("src/unicode-identifiers.ts").unwrap(),
            "export function café() { return 1; }\nconst π = () => true;\nexport const 名前 = () => true;\nclass 店 { 開く() { return true; } }\nexport { π as piAlias };\nfunction after() {}\n",
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:café",
                "fn:after",
                "class:店",
                "fn:店.開く",
                "fn:π",
                "fn:名前",
                "export:café",
                "export:名前",
                "export:piAlias",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:café").unwrap().start,
                extraction.anchors.get_str("fn:π").unwrap().start,
                extraction.anchors.get_str("fn:名前").unwrap().start,
                extraction.anchors.get_str("class:店").unwrap().start,
                extraction.anchors.get_str("fn:店.開く").unwrap().start,
                extraction.anchors.get_str("export:piAlias").unwrap().start,
            ),
            (1, 2, 3, 4, 4, 2)
        );
    }

    #[test]
    fn matches_escaped_unicode_identifier_anchor_edges() {
        let extraction = extract_anchors(
            &RelativePath::new("src/unicode-escapes.ts").unwrap(),
            r#"export function caf\u00e9() { return 1; }
const \u03c0 = () => true;
const \u{03c0}Brace = () => true;
export const \u540d\u524d = () => true;
class \u5e97 { \u958b\u304f() { return true; } }
export { \u03c0 as piAlias };
function after() {}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "fn:café",
                "fn:after",
                "class:店",
                "fn:店.開く",
                "fn:π",
                "fn:πBrace",
                "fn:名前",
                "export:café",
                "export:名前",
                "export:piAlias",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:café").unwrap().start,
                extraction.anchors.get_str("fn:π").unwrap().start,
                extraction.anchors.get_str("fn:πBrace").unwrap().start,
                extraction.anchors.get_str("fn:名前").unwrap().start,
                extraction.anchors.get_str("class:店").unwrap().start,
                extraction.anchors.get_str("fn:店.開く").unwrap().start,
                extraction.anchors.get_str("export:piAlias").unwrap().start,
            ),
            (1, 2, 3, 4, 5, 5, 2)
        );
        for ghost in [
            "fn:caf",
            "export:caf",
            "fn:u03c0",
            "fn:u540d",
            "class:u5e97",
            "fn:u958b",
        ] {
            assert!(extraction.anchors.get_str(ghost).is_none());
        }
    }

    #[test]
    fn matches_default_generator_function_edges() {
        let anonymous = extract_anchors(
            &RelativePath::new("src/default-generator.ts").unwrap(),
            "export default function*() { yield 1; }\nfunction real() {}\n",
        );
        assert_eq!(
            anonymous
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "export:default"]
        );
        assert_eq!(
            (
                anonymous.anchors.get_str("export:default").unwrap().start,
                anonymous.anchors.get_str("fn:real").unwrap().start,
            ),
            (1, 2)
        );
        assert!(anonymous.anchors.get_str("fn:yield").is_none());

        let named = extract_anchors(
            &RelativePath::new("src/named-default-generator.ts").unwrap(),
            "export default function* named() { yield 1; }\nfunction real() {}\n",
        );
        assert_eq!(
            named
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:named", "fn:real", "export:default"]
        );
        assert_eq!(
            (
                named.anchors.get_str("fn:named").unwrap().start,
                named.anchors.get_str("fn:real").unwrap().start,
                named.anchors.get_str("export:default").unwrap().start,
            ),
            (1, 2, 1)
        );
    }

    #[test]
    fn matches_malformed_jsx_recovery_boundaries() {
        let unclosed_file = RelativePath::new("src/malformed-jsx.tsx").unwrap();
        let unclosed = extract_anchors(
            &unclosed_file,
            "function before() { return 0; }\nconst Broken = () => <div>\nfunction after() { return 1; }\n",
        );
        let mismatched_file = RelativePath::new("src/malformed-jsx-close.tsx").unwrap();
        let mismatched_close = extract_anchors(
            &mismatched_file,
            "function before() { return 0; }\nconst Broken = () => <div>\n</span>;\nfunction after() { return 1; }\n",
        );

        assert_eq!(
            unclosed
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:before", "fn:Broken"]
        );
        assert_eq!(
            (
                unclosed.anchors.get_str("fn:Broken").unwrap().start,
                unclosed.anchors.get_str("fn:Broken").unwrap().end,
            ),
            (2, 4)
        );
        assert!(unclosed.anchors.get_str("fn:after").is_none());
        assert!(!unclosed.complete);
        assert!(!unclosed.diagnostics.is_empty());
        assert_eq!(
            mismatched_close
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:before", "fn:after", "fn:Broken"]
        );
        assert_eq!(
            (
                mismatched_close.anchors.get_str("fn:Broken").unwrap().start,
                mismatched_close.anchors.get_str("fn:Broken").unwrap().end,
            ),
            (2, 3)
        );
        assert_eq!(
            mismatched_close.anchors.get_str("fn:after").unwrap().start,
            4
        );
        assert!(!mismatched_close.complete);
        assert!(!mismatched_close.diagnostics.is_empty());
    }

    #[test]
    fn matches_significant_if_blocks_inside_template_literal_expressions() {
        let file = RelativePath::new("src/template-block.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            "const tpl = `line one\n${(() => { if (a && b || c) { return 1; } return 0; })()}\nline three`;\nfunction real() { return 1; }\nconst after = () => true;\n",
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "fn:after", "block:if_a_b_c",]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:real").unwrap().start,
                extraction.anchors.get_str("fn:after").unwrap().start,
            ),
            (4, 5)
        );
        let block = extraction.anchors.get_str("block:if_a_b_c").unwrap();
        assert_eq!((block.start, block.end, block.complexity), (2, 2, 3));
        assert!(extraction.anchors.get_str("fn:tpl").is_none());

        let nested_tagged = extract_anchors(
            &file,
            "const tpl = tag`outer ${(() => tag2`inner ${(() => { if (a && b || c) { return 1; } return 0; })()}`)()} done`;\nfunction real() { return 1; }\n",
        );
        assert_eq!(
            nested_tagged
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:real", "block:if_a_b_c"]
        );
        let nested_block = nested_tagged.anchors.get_str("block:if_a_b_c").unwrap();
        assert_eq!(
            (
                nested_block.start,
                nested_block.end,
                nested_block.complexity
            ),
            (1, 1, 3)
        );
        assert!(nested_tagged.anchors.get_str("fn:tpl").is_none());

        let regex_in_expression = extract_anchors(
            &RelativePath::new("src/template-regex-block.ts").unwrap(),
            "const tpl = `x ${/}/.test(s) ? (() => { if (a && b || c) { return 1; } return 0; })() : 0}`;\nfunction after() {}\n",
        );
        assert_eq!(
            regex_in_expression
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "block:if_a_b_c"]
        );
        let regex_block = regex_in_expression
            .anchors
            .get_str("block:if_a_b_c")
            .unwrap();
        assert_eq!(
            (
                regex_in_expression
                    .anchors
                    .get_str("fn:after")
                    .unwrap()
                    .start,
                regex_block.start,
                regex_block.end,
                regex_block.complexity,
            ),
            (2, 1, 1, 3)
        );

        let division_like_expression = extract_anchors(
            &RelativePath::new("src/template-division-block.ts").unwrap(),
            "const tpl = `x ${a / } / b ? (() => { if (a && b || c) { return 1; } return 0; })() : 0}`;\nfunction after() {}\n",
        );
        assert_eq!(
            division_like_expression
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after"]
        );
        assert!(division_like_expression
            .anchors
            .get_str("block:if_a_b_c")
            .is_none());

        let comment_in_expression = extract_anchors(
            &RelativePath::new("src/template-comment-block.ts").unwrap(),
            "const tpl = `x ${/* comment } */ (() => { if (a && b || c) { return 1; } return 0; })()}`;\nfunction after() {}\n",
        );
        assert_eq!(
            comment_in_expression
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "block:if_a_b_c"]
        );

        let multiline_nested = extract_anchors(
            &RelativePath::new("src/template-multiline-nested.ts").unwrap(),
            "const tpl = `outer ${tag`inner ${(() => {\n  if (a && b || c) { return 1; }\n  return 0;\n})()}`}`;\nfunction after() {}\n",
        );
        assert_eq!(
            multiline_nested
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "block:if_a_b_c"]
        );
        let multiline_block = multiline_nested.anchors.get_str("block:if_a_b_c").unwrap();
        assert_eq!(
            (
                multiline_nested.anchors.get_str("fn:after").unwrap().start,
                multiline_block.start,
                multiline_block.end,
                multiline_block.complexity,
            ),
            (5, 2, 2, 3)
        );

        let nested_unclosed_inner = extract_anchors(
            &RelativePath::new("src/template-nested-unclosed-inner.ts").unwrap(),
            "const tpl = `outer ${tag`inner ${(() => { if (a && b || c) { return 1; } return 0; })()} done`;\nfunction after() {}\n",
        );
        assert_eq!(
            nested_unclosed_inner
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "block:if_a_b_c"]
        );
        let nested_unclosed_block = nested_unclosed_inner
            .anchors
            .get_str("block:if_a_b_c")
            .unwrap();
        assert_eq!(
            (
                nested_unclosed_inner
                    .anchors
                    .get_str("fn:after")
                    .unwrap()
                    .start,
                nested_unclosed_block.start,
                nested_unclosed_block.end,
                nested_unclosed_block.complexity,
            ),
            (2, 1, 1, 3)
        );

        let nested_open_expression = extract_anchors(
            &RelativePath::new("src/template-nested-open-expression.ts").unwrap(),
            "const tpl = `outer ${tag`inner ${(() => { if (a && b || c) { return 1; } return 0; })()}\nfunction after() {}\n",
        );
        assert_eq!(
            nested_open_expression
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["block:if_a_b_c"]
        );
        let nested_open_block = nested_open_expression
            .anchors
            .get_str("block:if_a_b_c")
            .unwrap();
        assert_eq!(
            (
                nested_open_block.start,
                nested_open_block.end,
                nested_open_block.complexity,
            ),
            (1, 1, 3)
        );
        assert!(nested_open_expression.anchors.get_str("fn:after").is_none());

        let escaped_backtick = extract_anchors(
            &RelativePath::new("src/template-escaped-backtick.ts").unwrap(),
            "const tpl = `line \\` still template ${(() => { if (a && b || c) { return 1; } return 0; })()}`;\nfunction after() {}\n",
        );
        assert_eq!(
            escaped_backtick
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "block:if_a_b_c"]
        );

        let escaped_dollar = extract_anchors(
            &RelativePath::new("src/template-escaped-dollar.ts").unwrap(),
            "const tpl = `literal \\${notExpression}`;\nfunction after() {}\n",
        );
        assert_eq!(
            escaped_dollar
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after"]
        );

        let unterminated_raw = extract_anchors(
            &RelativePath::new("src/template-unterminated-raw.ts").unwrap(),
            "const tpl = `function hidden() {}\nfunction after() {}\n",
        );
        assert!(unterminated_raw.anchors.is_empty());
        assert!(!unterminated_raw.complete);
        assert!(!unterminated_raw.diagnostics.is_empty());

        let unterminated_closed_expression = extract_anchors(
            &RelativePath::new("src/template-unterminated-closed-expression.ts").unwrap(),
            "const tpl = `x ${(() => { if (a && b || c) { return 1; } return 0; })()}\nfunction after() {}\n",
        );
        assert_eq!(
            unterminated_closed_expression
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["block:if_a_b_c"]
        );
        assert!(unterminated_closed_expression
            .anchors
            .get_str("fn:after")
            .is_none());
        assert!(!unterminated_closed_expression.complete);
        assert!(!unterminated_closed_expression.diagnostics.is_empty());

        let unterminated_open_expression = extract_anchors(
            &RelativePath::new("src/template-unterminated-open-expression.ts").unwrap(),
            "const tpl = `x ${(() => { if (a && b || c) { return 1; } return 0; })()\nfunction after() {}\n",
        );
        assert_eq!(
            unterminated_open_expression
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:after", "block:if_a_b_c"]
        );
        let recovered_block = unterminated_open_expression
            .anchors
            .get_str("block:if_a_b_c")
            .unwrap();
        assert_eq!((recovered_block.start, recovered_block.end), (1, 1));
        assert!(unterminated_open_expression
            .anchors
            .get_str("fn:tpl")
            .is_none());
        assert!(!unterminated_open_expression.complete);
        assert!(!unterminated_open_expression.diagnostics.is_empty());
    }

    #[test]
    fn matches_tsx_generic_arrow_ambiguity_and_ts_generic_arrow_parity() {
        let source = "function before() { return 0; }\nconst id = <T>(value: T) => value;\nfunction after<T>(value: T) { return value; }\n";
        let ts_file = RelativePath::new("src/ambig-generic.ts").unwrap();
        let tsx_file = RelativePath::new("src/ambig-generic.tsx").unwrap();
        let valid_tsx_file = RelativePath::new("src/valid-generic.tsx").unwrap();
        let ts = extract_anchors(&ts_file, source);
        let tsx = extract_anchors(&tsx_file, source);
        let valid_tsx = extract_anchors(
            &valid_tsx_file,
            "function before() { return 0; }\nconst id = <T,>(value: T) => value;\nfunction after<T>(value: T) { return value; }\n",
        );

        assert_eq!(
            ts.anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:before", "fn:after", "fn:id"]
        );
        assert_eq!(
            tsx.anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:before"]
        );
        assert_eq!(
            valid_tsx
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:before", "fn:after", "fn:id"]
        );
        assert_eq!(ts.anchors.get_str("fn:id").unwrap().start, 2);
        assert!(tsx.anchors.get_str("fn:id").is_none());
        assert!(tsx.anchors.get_str("fn:after").is_none());
        assert_eq!(valid_tsx.anchors.get_str("fn:id").unwrap().start, 2);
    }

    #[test]
    fn extracts_significant_if_blocks_with_normalized_conditions() {
        let file = RelativePath::new("src/block.ts").unwrap();
        let extraction = extract_anchors(
            &file,
            r#"
function complex() {
  if (user && (user.active || user.admin) && account?.enabled) {
    return true;
  }
}
"#,
        );

        let function = extraction.anchors.get_str("fn:complex").unwrap();
        assert_eq!(
            (function.start, function.end, function.complexity),
            (2, 6, 5)
        );
        let block = extraction
            .anchors
            .get_str("block:if_user_user.active_user.admin_account_.enabled")
            .unwrap();
        assert_eq!((block.start, block.end, block.complexity), (3, 5, 4));
        assert_eq!(block.kind.as_str(), "block");
    }

    #[test]
    fn extracts_c_family_functions_types_methods_and_blocks() {
        let extraction = extract_anchors(
            &RelativePath::new("src/driver.c").unwrap(),
            r#"typedef struct device {
    int id;
} device_t;

static int probe(struct device *dev)
{
    if (dev && dev->id > 0 || fallback(dev)) {
        return dev->id;
    }
    return 0;
}

enum state {
    STATE_READY,
};
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "struct:device",
                "enum:state",
                "fn:probe",
                "block:if_dev_dev_id_0_fallback_dev",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("struct:device").unwrap().start,
                extraction.anchors.get_str("struct:device").unwrap().end,
                extraction.anchors.get_str("fn:probe").unwrap().start,
                extraction.anchors.get_str("fn:probe").unwrap().end,
            ),
            (1, 3, 5, 11)
        );
    }

    #[test]
    fn extracts_cpp_inline_and_qualified_methods() {
        let extraction = extract_anchors(
            &RelativePath::new("src/store.cpp").unwrap(),
            r#"class Store {
public:
    int get() const {
        if (ready_ && value_ > 0 || fallback()) {
            return value_;
        }
        return 0;
    }
private:
    bool ready_;
    int value_;
};

int Store::set(int value)
{
    value_ = value;
    return value_;
}
"#,
        );

        assert_eq!(
            extraction
                .anchors
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec![
                "class:Store",
                "fn:Store.get",
                "fn:Store.set",
                "block:if_ready__value__0_fallback",
            ]
        );
        assert_eq!(
            (
                extraction.anchors.get_str("fn:Store.get").unwrap().start,
                extraction.anchors.get_str("fn:Store.get").unwrap().end,
                extraction.anchors.get_str("fn:Store.get").unwrap().kind,
                extraction.anchors.get_str("fn:Store.set").unwrap().start,
                extraction.anchors.get_str("fn:Store.set").unwrap().kind,
            ),
            (3, 8, AnchorKind::Method, 14, AnchorKind::Method)
        );
    }

    #[test]
    fn asserts_anchor_existence_with_current_write_decision_message() {
        let file = RelativePath::new("src/session.ts").unwrap();
        let source = "function processCheckout() {}\nfunction approveOrder() {}\n";
        assert!(assert_anchor_exists(&file, source, "fn:processCheckout").is_ok());

        let error = assert_anchor_exists(&file, source, "fn:missing")
            .unwrap_err()
            .user_message();
        assert_eq!(
            error,
            "Anchor \"fn:missing\" does not exist in src/session.ts. A decision recorded against a missing anchor is an immediate orphan. Available anchors in src/session.ts: fn:approveOrder, fn:processCheckout."
        );

        let no_anchor = assert_anchor_exists(&file, "const value = 1;\n", "fn:missing")
            .unwrap_err()
            .user_message();
        assert_eq!(
            no_anchor,
            "Anchor \"fn:missing\" does not exist in src/session.ts. A decision recorded against a missing anchor is an immediate orphan. No anchors were found in src/session.ts."
        );
    }
}
