use crate::core::diff::{apply_line_changes_to_range, diff_lines, LineChange};
use crate::core::dlog::LineRange;
use crate::core::dmap::{parse_dmap, render_dmap, DecisionStatus, DmapEntry};
use crate::core::json::{
    parse as parse_json, stringify_compact, stringify_pretty, JsonObject, JsonValue,
};
use crate::core::yaml::{parse_yaml, render_yaml, YamlObject, YamlValue};

const DEFAULT_CASES: usize = 128;

#[derive(Clone, Debug)]
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.state = value;
        value
    }

    fn usize(&mut self, upper_exclusive: usize) -> usize {
        if upper_exclusive == 0 {
            0
        } else {
            (self.next_u64() as usize) % upper_exclusive
        }
    }

    fn bool(&mut self) -> bool {
        self.usize(2) == 0
    }
}

#[test]
fn property_json_roundtrips_compact_and_pretty() {
    let mut rng = Rng::new(0xA11C_E5ED);
    for _ in 0..DEFAULT_CASES {
        let value = json_value(&mut rng, 0);

        let compact = stringify_compact(&value);
        assert_eq!(parse_json(&compact).unwrap(), value);

        let pretty = stringify_pretty(&value, 2);
        assert_eq!(parse_json(&pretty).unwrap(), value);
    }
}

#[test]
fn property_yaml_roundtrips_rendered_values() {
    let mut rng = Rng::new(0xA11C_5EED);
    for _ in 0..DEFAULT_CASES {
        let value = yaml_document(&mut rng);
        let rendered = render_yaml(&value);
        let parsed = match parse_yaml(&rendered) {
            Ok(parsed) => parsed,
            Err(error) => {
                panic!("parse failed: {error:?}\n--- rendered ---\n{rendered}\n--- end ---")
            }
        };
        assert_eq!(
            parsed, value,
            "round-trip failed\n--- rendered ---\n{rendered}\n--- end ---"
        );
    }
}

#[test]
fn property_dmap_render_parse_is_idempotent() {
    let mut rng = Rng::new(0xD00D_5EED);
    for _ in 0..DEFAULT_CASES {
        let entries = dmap_entries(&mut rng);
        let rendered = render_dmap(&entries);
        let parsed = parse_dmap(&rendered).unwrap();
        assert_eq!(render_dmap(&parsed), rendered);
    }
}

#[test]
fn property_dmap_random_malformed_inputs_do_not_panic() {
    let mut rng = Rng::new(0xBAD_D0C5);
    for _ in 0..DEFAULT_CASES {
        let input = random_ascii_text(&mut rng, 80);
        if let Ok(entries) = parse_dmap(&input) {
            let rendered = render_dmap(&entries);
            assert_eq!(render_dmap(&parse_dmap(&rendered).unwrap()), rendered);
        }
    }
}

#[test]
fn property_json_yaml_random_malformed_inputs_do_not_panic() {
    let mut rng = Rng::new(0x5EDE_5EED);
    for _ in 0..DEFAULT_CASES {
        let input = random_parser_hostile_text(&mut rng, 160);

        if let Ok(value) = parse_json(&input) {
            let rendered = stringify_compact(&value);
            assert_eq!(parse_json(&rendered).unwrap(), value);
        }

        if let Ok(value) = parse_yaml(&input) {
            if matches!(
                &value,
                YamlValue::Object(object) if !object.entries().is_empty()
            ) || matches!(&value, YamlValue::Array(values) if !values.is_empty())
            {
                let rendered = render_yaml(&value);
                assert_eq!(parse_yaml(&rendered).unwrap(), value);
            }
        }
    }
}

#[test]
fn property_diff_counts_match_random_edit_scripts() {
    let mut rng = Rng::new(0xD1FF_5EED);
    for _ in 0..DEFAULT_CASES {
        let old_lines = random_lines(&mut rng, 30);
        let new_lines = mutate_lines(&mut rng, &old_lines);
        let old_content = lines_to_source(&old_lines);
        let new_content = lines_to_source(&new_lines);
        let changes = diff_lines(&old_content, &new_content);

        let (old_count, new_count) = change_counts(&changes);
        assert_eq!(old_count, old_lines.len() as u32);
        assert_eq!(new_count, new_lines.len() as u32);

        if !old_lines.is_empty() {
            let start = rng.usize(old_lines.len()) as u32 + 1;
            let end = start + rng.usize(old_lines.len() - start as usize + 1) as u32;
            let shifted = apply_line_changes_to_range(&changes, LineRange { start, end });
            assert!(shifted.start >= 1);
            assert!(shifted.end >= shifted.start);
        }
    }
}

#[test]
fn property_parser_malformed_sources_do_not_panic() {
    let mut rng = Rng::new(0xA4C0_5EED);
    for _ in 0..DEFAULT_CASES {
        for kind in [SourceKind::Ts, SourceKind::Tsx, SourceKind::Rust] {
            let file = fuzz_file(kind);
            let source = random_malformed_source(&mut rng, kind, 48);
            let extraction = crate::core::anchor::extract_anchors(&file, &source);
            assert_anchor_extraction_invariants(&extraction);
        }
    }
}

#[test]
#[ignore]
fn property_extended_serialization_and_diff() {
    let mut rng = Rng::new(0xE17E_5EED);
    for _ in 0..4096 {
        let json = json_value(&mut rng, 0);
        assert_eq!(parse_json(&stringify_compact(&json)).unwrap(), json);

        let yaml = yaml_document(&mut rng);
        assert_eq!(parse_yaml(&render_yaml(&yaml)).unwrap(), yaml);

        let entries = dmap_entries(&mut rng);
        let rendered = render_dmap(&entries);
        assert_eq!(render_dmap(&parse_dmap(&rendered).unwrap()), rendered);

        let old_lines = random_lines(&mut rng, 80);
        let new_lines = mutate_lines(&mut rng, &old_lines);
        let changes = diff_lines(&lines_to_source(&old_lines), &lines_to_source(&new_lines));
        let (old_count, new_count) = change_counts(&changes);
        assert_eq!(old_count, old_lines.len() as u32);
        assert_eq!(new_count, new_lines.len() as u32);

        let kind = match rng.usize(3) {
            0 => SourceKind::Ts,
            1 => SourceKind::Tsx,
            _ => SourceKind::Rust,
        };
        let file = fuzz_file(kind);
        let source = random_malformed_source(&mut rng, kind, 96);
        assert_anchor_extraction_invariants(&crate::core::anchor::extract_anchors(&file, &source));
    }
}

#[derive(Clone, Copy)]
enum SourceKind {
    Ts,
    Tsx,
    Rust,
}

fn json_value(rng: &mut Rng, depth: usize) -> JsonValue {
    match if depth >= 3 {
        rng.usize(4)
    } else {
        rng.usize(6)
    } {
        0 => JsonValue::Null,
        1 => JsonValue::Bool(rng.bool()),
        2 => JsonValue::Number((rng.usize(10_000) as f64) - 5_000.0),
        3 => JsonValue::String(random_text(rng, 18)),
        4 => JsonValue::Array(
            (0..rng.usize(4))
                .map(|_| json_value(rng, depth + 1))
                .collect(),
        ),
        _ => {
            let mut entries = Vec::new();
            for index in 0..rng.usize(4) {
                entries.push((
                    format!("k{index}_{}", random_identifier(rng)),
                    json_value(rng, depth + 1),
                ));
            }
            JsonValue::Object(JsonObject::from_entries(entries))
        }
    }
}

fn yaml_document(rng: &mut Rng) -> YamlValue {
    if rng.bool() {
        YamlValue::Object(yaml_object(rng, 0, 1))
    } else {
        YamlValue::Array(yaml_array(rng, 0, 1))
    }
}

fn yaml_value(rng: &mut Rng, depth: usize) -> YamlValue {
    match if depth >= 3 {
        rng.usize(4)
    } else {
        rng.usize(6)
    } {
        0 => YamlValue::Null,
        1 => YamlValue::Bool(rng.bool()),
        2 => YamlValue::Number(rng.usize(10_000) as i64 - 5_000),
        3 => YamlValue::String(random_yaml_string(rng, 18)),
        4 => YamlValue::Array(yaml_array(rng, depth + 1, 0)),
        _ => YamlValue::Object(yaml_object(rng, depth + 1, 0)),
    }
}

fn yaml_array(rng: &mut Rng, depth: usize, min_len: usize) -> Vec<YamlValue> {
    let len = min_len + rng.usize(4);
    (0..len)
        .map(|_| yaml_sequence_item(rng, depth + 1))
        .collect()
}

fn yaml_sequence_item(rng: &mut Rng, depth: usize) -> YamlValue {
    match if depth >= 3 {
        rng.usize(5)
    } else {
        rng.usize(7)
    } {
        0 => YamlValue::Null,
        1 => YamlValue::Bool(rng.bool()),
        2 => YamlValue::Number(rng.usize(10_000) as i64 - 5_000),
        3 => YamlValue::String(random_yaml_string(rng, 18)),
        4 => YamlValue::Array(Vec::new()),
        _ => YamlValue::Object(yaml_object(rng, depth + 1, 1)),
    }
}

fn yaml_object(rng: &mut Rng, depth: usize, min_len: usize) -> YamlObject {
    let mut entries = Vec::new();
    for index in 0..(min_len + rng.usize(4)) {
        entries.push((
            format!("k{index}_{}", random_identifier(rng)),
            yaml_value(rng, depth + 1),
        ));
    }
    YamlObject::from_entries(entries)
}

fn dmap_entries(rng: &mut Rng) -> Vec<DmapEntry> {
    (0..rng.usize(12))
        .map(|index| {
            let start = rng.usize(60) as i64;
            DmapEntry {
                start_line: start,
                end_line: start + rng.usize(6) as i64,
                anchor: format!("fn:{}_{}", random_identifier(rng), index),
                status: match rng.usize(4) {
                    0 => Some(DecisionStatus::Undecided),
                    1 => Some(DecisionStatus::Stale),
                    2 => Some(DecisionStatus::Orphan),
                    _ => None,
                },
            }
        })
        .collect()
}

fn random_lines(rng: &mut Rng, max_lines: usize) -> Vec<String> {
    (0..rng.usize(max_lines + 1))
        .map(|_| random_identifier(rng))
        .collect()
}

fn mutate_lines(rng: &mut Rng, old_lines: &[String]) -> Vec<String> {
    let mut output = Vec::new();
    for line in old_lines {
        append_mutated_line(rng, line, &mut output);
    }
    if rng.usize(3) == 0 {
        output.push(random_identifier(rng));
    }
    output
}

fn append_mutated_line(rng: &mut Rng, line: &str, output: &mut Vec<String>) {
    let action = rng.usize(5);
    if action == 0 {
        return;
    }
    push_prefix_insert(rng, action, output);
    output.push(line_for_action(rng, action, line));
    push_suffix_insert(rng, action, output);
}

fn push_prefix_insert(rng: &mut Rng, action: usize, output: &mut Vec<String>) {
    if action == 1 {
        output.push(random_identifier(rng));
    }
}

fn push_suffix_insert(rng: &mut Rng, action: usize, output: &mut Vec<String>) {
    if action == 3 {
        output.push(random_identifier(rng));
    }
}

fn line_for_action(rng: &mut Rng, action: usize, line: &str) -> String {
    if action == 2 {
        random_identifier(rng)
    } else {
        line.to_string()
    }
}

fn lines_to_source(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn change_counts(changes: &[LineChange]) -> (u32, u32) {
    let mut old_count = 0_u32;
    let mut new_count = 0_u32;
    for change in changes {
        match *change {
            LineChange::Equal { count } => {
                old_count += count;
                new_count += count;
            }
            LineChange::Added { count } => new_count += count,
            LineChange::Removed { count } => old_count += count,
        }
    }
    (old_count, new_count)
}

fn random_identifier(rng: &mut Rng) -> String {
    let len = rng.usize(8) + 1;
    let mut output = String::new();
    for _ in 0..len {
        output.push((b'a' + rng.usize(26) as u8) as char);
    }
    output
}

fn random_text(rng: &mut Rng, max_len: usize) -> String {
    let len = rng.usize(max_len + 1);
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 _-.,";
    let mut output = String::new();
    for _ in 0..len {
        output.push(alphabet[rng.usize(alphabet.len())] as char);
    }
    output
}

fn random_yaml_string(rng: &mut Rng, max_len: usize) -> String {
    // Occasionally emit strings that exercise the block-scalar / folding /
    // quoting paths of the renderer: long (>100) values, internal whitespace
    // runs, tabs, CR/CRLF, embedded newlines, and leading/trailing spaces.
    // These categories previously round-tripped incorrectly (data corruption),
    // so the round-trip property must cover them.
    match rng.usize(8) {
        0 => {
            // Long, word-wrapped prose with occasional double spaces.
            let words = 20 + rng.usize(40);
            let mut output = String::new();
            for index in 0..words {
                if index > 0 {
                    output.push(' ');
                    if rng.usize(5) == 0 {
                        output.push(' ');
                    }
                }
                for _ in 0..(1 + rng.usize(6)) {
                    output.push(random_yaml_edge_char(rng));
                }
            }
            output
        }
        1 => {
            // Multiline content (literal-block candidate).
            let lines = 2 + rng.usize(4);
            let mut parts = Vec::new();
            for _ in 0..lines {
                let len = rng.usize(30);
                let mut line = String::new();
                for _ in 0..len {
                    line.push(random_yaml_inner_char(rng));
                }
                parts.push(line);
            }
            parts.join("\n")
        }
        2 => {
            // Whitespace-sensitive: tabs, CR, CRLF, leading/trailing spaces.
            let choices = [
                "a\tb",
                "a\rb",
                "a\r\nb",
                "  leading",
                "trailing  ",
                "\n",
                "a\nb\n",
            ];
            let base = choices[rng.usize(choices.len())].to_string();
            if rng.bool() {
                let mut long = base.clone();
                while long.len() < 120 {
                    long.push('z');
                }
                long
            } else {
                base
            }
        }
        _ => {
            let len = rng.usize(max_len + 1);
            match len {
                0 => String::new(),
                1 => random_yaml_edge_char(rng).to_string(),
                _ => {
                    let mut output = String::new();
                    output.push(random_yaml_edge_char(rng));
                    for _ in 0..(len - 2) {
                        output.push(random_yaml_inner_char(rng));
                    }
                    output.push(random_yaml_edge_char(rng));
                    output
                }
            }
        }
    }
}

fn random_yaml_edge_char(rng: &mut Rng) -> char {
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-.,";
    alphabet[rng.usize(alphabet.len())] as char
}

fn random_yaml_inner_char(rng: &mut Rng) -> char {
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 _-.,";
    alphabet[rng.usize(alphabet.len())] as char
}

fn random_ascii_text(rng: &mut Rng, max_len: usize) -> String {
    let len = rng.usize(max_len + 1);
    let alphabet = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ:-_\n\r ";
    let mut output = String::new();
    for _ in 0..len {
        output.push(alphabet[rng.usize(alphabet.len())] as char);
    }
    output
}

fn random_parser_hostile_text(rng: &mut Rng, max_len: usize) -> String {
    let fragments = [
        "{",
        "}",
        "[",
        "]",
        ":",
        ",",
        "\"",
        "'",
        "\\",
        "\n",
        "\r\n",
        "\t",
        " ",
        "# comment\n",
        "- ",
        "? ",
        "|",
        ">",
        "!!str ",
        "null",
        "true",
        "false",
        "123",
        "-4.5e+6",
        "\"unterminated",
        "'unterminated",
        "\\u{",
        "[}",
        "{]",
        "key: value\n",
        "  nested: [1, 2\n",
        "- item\n",
    ];
    let mut output = String::new();
    while output.len() < max_len && rng.usize(6) != 0 {
        output.push_str(fragments[rng.usize(fragments.len())]);
        if rng.usize(4) == 0 {
            output.push_str(&random_text(rng, 8));
        }
    }
    output
}

fn fuzz_file(kind: SourceKind) -> crate::core::paths::RelativePath {
    crate::core::paths::RelativePath::new(match kind {
        SourceKind::Ts => "src/fuzz.ts",
        SourceKind::Tsx => "src/fuzz.tsx",
        SourceKind::Rust => "src/fuzz.rs",
    })
    .unwrap()
}

fn random_malformed_source(rng: &mut Rng, kind: SourceKind, max_fragments: usize) -> String {
    let mut output = String::new();
    for _ in 0..(rng.usize(max_fragments) + 1) {
        append_random_source_fragment(rng, fragments_for(kind), &mut output);
    }
    output
}

fn append_random_source_fragment(rng: &mut Rng, fragments: &[&str], output: &mut String) {
    output.push_str(fragments[rng.usize(fragments.len())]);
    if rng.bool() {
        output.push('\n');
    }
}

fn fragments_for(kind: SourceKind) -> &'static [&'static str] {
    match kind {
        SourceKind::Ts => &[
            "export function f",
            "(a",
            ") {",
            "if (a &&",
            "return `unterminated ${",
            "/* open",
            "class C { m(",
            "const x = /[/",
            "}",
        ],
        SourceKind::Tsx => &[
            "export const View = () =>",
            "<div",
            "</span>",
            "{props.ok &&",
            "<Button value={",
            "return <",
            "/* jsx",
            "`template ${",
            "}",
        ],
        SourceKind::Rust => &[
            "pub fn f",
            "(x:",
            ") {",
            "let value = r#\"",
            "/* block",
            "match value {",
            "'unterminated",
            "impl T { fn m(",
            "}",
        ],
    }
}

fn assert_anchor_extraction_invariants(extraction: &crate::core::anchor::AnchorExtraction) {
    assert_eq!(extraction.complete, extraction.diagnostics.is_empty());
    for (_, anchor) in extraction.anchors.iter() {
        assert!(!anchor.anchor.is_empty());
        assert!(anchor.start >= 1);
        assert!(anchor.end >= anchor.start);
    }
    for diagnostic in &extraction.diagnostics {
        assert!(diagnostic.line >= 1);
        assert!(diagnostic.column >= 1);
        assert!(!diagnostic.message.is_empty());
    }
}
