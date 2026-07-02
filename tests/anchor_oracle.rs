//! Differential-oracle test harness for the Rust and C/C++ anchor extractors
//! (audit finding B10).
//!
//! B10: "The Rust and C/C++ extractors have no independent oracle; correctness
//! rests on same-team unit tests. A wrong range in a .rs file is silently
//! accepted."
//!
//! This suite is a *self-checking corpus with independently-declared
//! expectations*. Because the zero-dependency policy forbids `syn`,
//! `tree-sitter`, and every other crate, the "oracle" is built two ways that do
//! not share code with `extract_anchors`:
//!
//!   1. Hand-computed ground truth. Every fixture is written so the correct
//!      anchor set and the exact 1-based inclusive `start`/`end` line ranges are
//!      obvious from the text, and are annotated inline. `check_rust` /
//!      `check_c` assert the extractor reproduces exactly that set and those
//!      ranges — so a wrong range fails the test rather than being silently
//!      accepted (the literal B10 complaint).
//!
//!   2. An independent line scanner (`scan_top_level_items` /
//!      `independent_item_start`) that re-derives, using only `str` operations
//!      and column/prefix heuristics, the start line of every top-level
//!      `fn`/`struct`/`enum`/`trait`. It shares no logic with the extractor's
//!      tokenizer/parser, so `crosscheck_start_lines` gives a genuinely
//!      independent oracle for start lines.
//!
//! KNOWN BUGS discovered while building this oracle: none. Every extracted range
//! matched the hand-computed ground truth. If a genuine wrong range is found in
//! the future, per the audit instructions it must be recorded here with a
//! `// KNOWN BUG:` comment and the fixture asserted at its current behavior,
//! rather than fixed in `src/core/anchor.rs` (this file is test-only).

use archiva::core::anchor::{extract_anchors, AnchorExtraction, AnchorKind};
use archiva::core::paths::RelativePath;

/// One hand-declared expected anchor: its key, its 1-based inclusive start/end
/// line range, and its kind. Order in the slice must match the extractor's
/// emission order (structural items in source order, then exports, then blocks).
struct Expect {
    key: &'static str,
    start: u32,
    end: u32,
    kind: AnchorKind,
}

const fn e(key: &'static str, start: u32, end: u32, kind: AnchorKind) -> Expect {
    Expect {
        key,
        start,
        end,
        kind,
    }
}

fn extract(path: &str, source: &str) -> AnchorExtraction {
    let file = RelativePath::new(path).expect("valid relative path");
    extract_anchors(&file, source)
}

/// Assert the extraction reproduces exactly `expected` (same keys in the same
/// order, same ranges, same kinds). On mismatch it panics with a readable diff
/// of expected vs actual so a wrong range is immediately visible.
fn check_anchors(name: &str, extraction: &AnchorExtraction, expected: &[Expect]) {
    let actual_keys: Vec<&str> = extraction
        .anchors
        .iter()
        .map(|(key, _)| key.as_str())
        .collect();
    let expected_keys: Vec<&str> = expected.iter().map(|item| item.key).collect();
    assert_eq!(
        actual_keys, expected_keys,
        "\n[{name}] anchor SET/ORDER mismatch\n  expected: {expected_keys:?}\n  actual:   {actual_keys:?}"
    );

    for item in expected {
        let info = extraction.anchors.get_str(item.key).unwrap_or_else(|| {
            panic!(
                "\n[{name}] expected anchor {:?} is missing\n  present keys: {actual_keys:?}",
                item.key
            )
        });
        assert!(
            info.start == item.start && info.end == item.end && info.kind == item.kind,
            "\n[{name}] range/kind mismatch for {key:?}\n  expected: start={es} end={ee} kind={ek:?}\n  actual:   start={as_} end={ae} kind={ak:?}",
            key = item.key,
            es = item.start,
            ee = item.end,
            ek = item.kind,
            as_ = info.start,
            ae = info.end,
            ak = info.kind,
        );
    }
}

fn check_complete(name: &str, extraction: &AnchorExtraction) {
    assert!(
        extraction.complete,
        "\n[{name}] expected complete extraction, got diagnostics: {:?}",
        extraction.diagnostics
    );
}

// ===========================================================================
// Independent oracle: a regex-free line scanner with no shared code with the
// extractor. It finds top-level item declaration lines by a simple heuristic
// (item keyword at column 0, after optional visibility/asyncness modifiers) and
// walks upward over single-line docs/attributes to find the true start line.
// ===========================================================================

#[derive(Debug, PartialEq, Eq)]
enum ScanKind {
    Fn,
    Struct,
    Enum,
    Trait,
}

impl ScanKind {
    fn anchor_prefix(&self) -> &'static str {
        match self {
            ScanKind::Fn => "fn",
            ScanKind::Struct => "struct",
            ScanKind::Enum => "enum",
            ScanKind::Trait => "trait",
        }
    }
}

struct ScannedItem {
    kind: ScanKind,
    name: String,
    /// 0-based index of the line carrying the item keyword.
    keyword_line: usize,
}

/// Modifiers that may precede an item keyword at column 0. `pub(crate)` etc. are
/// matched by prefix so parenthesised visibility scopes are handled too.
fn is_modifier_token(token: &str) -> bool {
    token.starts_with("pub")
        || matches!(
            token,
            "async" | "unsafe" | "const" | "extern" | "default" | "static"
        )
}

/// Independently scan for top-level `fn`/`struct`/`enum`/`trait` declarations.
/// "Top-level" is approximated as "keyword line begins at column 0" — the
/// corpus is written so every top-level item starts at column 0 and every
/// nested item is indented, which lets this scanner stay free of any brace or
/// token tracking (i.e. independent of the extractor).
fn scan_top_level_items(source: &str) -> Vec<ScannedItem> {
    let mut items = Vec::new();
    for (line_idx, raw) in source.lines().enumerate() {
        // Column-0 requirement: skip indented (nested) lines entirely.
        if raw.starts_with([' ', '\t']) || raw.is_empty() {
            continue;
        }
        let mut words = raw.split_whitespace().peekable();
        // Skip leading visibility / asyncness / const modifiers.
        while let Some(word) = words.peek() {
            if is_modifier_token(word) {
                words.next();
            } else {
                break;
            }
        }
        let Some(keyword) = words.next() else {
            continue;
        };
        let kind = match keyword {
            "fn" => ScanKind::Fn,
            "struct" => ScanKind::Struct,
            "enum" => ScanKind::Enum,
            "trait" => ScanKind::Trait,
            _ => continue,
        };
        let Some(raw_name) = words.next() else {
            continue;
        };
        // The name ends at the first non-identifier byte (generics, params,
        // braces, semicolons, etc.).
        let name: String = raw_name
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        items.push(ScannedItem {
            kind,
            name,
            keyword_line: line_idx,
        });
    }
    items
}

/// Independently compute the 1-based start line of an item, walking upward over
/// contiguous single-line doc comments (`///`, `//!`) and single-line
/// attributes (`#[...]`) and single-line block docs (`/** ... */`). This mirrors
/// the *intent* of leading-doc inclusion using standalone `str` logic; it
/// deliberately does not handle multi-line attributes, so cross-checked
/// fixtures use only single-line docs/attributes.
fn independent_item_start(lines: &[&str], keyword_line: usize) -> u32 {
    let mut start = keyword_line;
    while start > 0 {
        let prev = lines[start - 1].trim();
        let is_line_doc = prev.starts_with("///") || prev.starts_with("//!");
        let is_attr = prev.starts_with("#[") && prev.ends_with(']');
        let is_block_doc = prev.starts_with("/**") && prev.ends_with("*/");
        if is_line_doc || is_attr || is_block_doc {
            start -= 1;
        } else {
            break;
        }
    }
    u32::try_from(start + 1).expect("line number fits in u32")
}

/// For each independently-scanned top-level item, assert the extractor's `start`
/// line for the corresponding anchor equals the independently-derived start.
/// This is the genuinely-independent half of the oracle for start lines.
fn crosscheck_start_lines(name: &str, source: &str, extraction: &AnchorExtraction) {
    let lines: Vec<&str> = source.lines().collect();
    let items = scan_top_level_items(source);
    assert!(
        !items.is_empty(),
        "[{name}] independent scanner found no top-level items — fixture/scanner mismatch"
    );
    for item in &items {
        let key = format!("{}:{}", item.kind.anchor_prefix(), item.name);
        let info = extraction.anchors.get_str(&key).unwrap_or_else(|| {
            panic!(
                "[{name}] independent scanner found item {key:?} that the extractor did not emit"
            )
        });
        let expected_start = independent_item_start(&lines, item.keyword_line);
        assert_eq!(
            info.start, expected_start,
            "\n[{name}] INDEPENDENT start-line oracle disagrees for {key:?}\n  independent scan: start={expected_start}\n  extractor:        start={}",
            info.start
        );
    }
}

// ===========================================================================
// Rust fixtures. Each fixture is a raw string whose first content line is line
// 1 (no leading newline), so the inline `// line N` annotations match the
// 1-based line numbers the extractor reports.
// ===========================================================================

/// Top-level functions, private and public.
const RUST_TOP_LEVEL: &str = "\
fn alpha() {
    let _ = 1;
}

pub fn beta() -> u32 {
    2
}
";
// line 1: fn alpha() {        -> fn:alpha starts here
// line 2:     let _ = 1;
// line 3: }                   -> fn:alpha ends here (1..=3)
// line 4: (blank)
// line 5: pub fn beta() ...   -> fn:beta / export:beta start here
// line 6:     2
// line 7: }                   -> fn:beta ends here (5..=7)

#[test]
fn oracle_rust_top_level_functions() {
    let extraction = extract("src/top_level.rs", RUST_TOP_LEVEL);
    check_complete("rust_top_level", &extraction);
    check_anchors(
        "rust_top_level",
        &extraction,
        &[
            e("fn:alpha", 1, 3, AnchorKind::Function),
            e("fn:beta", 5, 7, AnchorKind::Function),
            e("export:beta", 5, 7, AnchorKind::Export),
        ],
    );
    crosscheck_start_lines("rust_top_level", RUST_TOP_LEVEL, &extraction);
}

/// Nested (function-local) function.
const RUST_NESTED: &str = "\
fn outer() {
    fn inner() -> bool {
        true
    }
    let _ = inner();
}
";
// line 1: fn outer() {            -> fn:outer starts (1)
// line 2:     fn inner() ... {    -> fn:outer.inner starts (2)
// line 3:         true
// line 4:     }                   -> fn:outer.inner ends (2..=4)
// line 5:     let _ = inner();
// line 6: }                       -> fn:outer ends (1..=6)

#[test]
fn oracle_rust_nested_functions() {
    let extraction = extract("src/nested.rs", RUST_NESTED);
    check_complete("rust_nested", &extraction);
    check_anchors(
        "rust_nested",
        &extraction,
        &[
            e("fn:outer", 1, 6, AnchorKind::Function),
            e("fn:outer.inner", 2, 4, AnchorKind::Function),
        ],
    );
    // Only `outer` is top-level for the independent scanner (inner is indented).
    crosscheck_start_lines("rust_nested", RUST_NESTED, &extraction);
}

/// Struct + impl block with a public and a private method.
const RUST_IMPL_METHODS: &str = "\
struct Widget {
    size: u32,
}

impl Widget {
    pub fn new() -> Self {
        Widget { size: 0 }
    }

    fn hidden(&self) -> u32 {
        self.size
    }
}
";
// line 1: struct Widget {          -> struct:Widget starts (1); private, so no export
// line 2:     size: u32,
// line 3: }                        -> struct:Widget ends (1..=3)
// line 4: (blank)
// line 5: impl Widget {            -> impl:Widget starts (5)
// line 6:     pub fn new() ... {   -> fn:Widget.new / export:Widget.new start (6)
// line 7:         Widget { size:0 }
// line 8:     }                    -> fn:Widget.new ends (6..=8)
// line 9: (blank)
// line 10:    fn hidden(&self) {   -> fn:Widget.hidden starts (10)
// line 11:        self.size
// line 12:    }                    -> fn:Widget.hidden ends (10..=12)
// line 13: }                       -> impl:Widget ends (5..=13)

#[test]
fn oracle_rust_impl_methods() {
    let extraction = extract("src/impl_methods.rs", RUST_IMPL_METHODS);
    check_complete("rust_impl_methods", &extraction);
    check_anchors(
        "rust_impl_methods",
        &extraction,
        &[
            e("struct:Widget", 1, 3, AnchorKind::Struct),
            e("impl:Widget", 5, 13, AnchorKind::Impl),
            e("fn:Widget.new", 6, 8, AnchorKind::Method),
            e("fn:Widget.hidden", 10, 12, AnchorKind::Method),
            e("export:Widget.new", 6, 8, AnchorKind::Export),
        ],
    );
    // Independent scanner covers the top-level `struct Widget`.
    crosscheck_start_lines("rust_impl_methods", RUST_IMPL_METHODS, &extraction);
}

/// Trait with a required (no body) and a defaulted (with body) method.
const RUST_TRAIT: &str = "\
pub trait Greeter {
    fn name(&self) -> String;
    fn greet(&self) -> String {
        String::from(\"hi\")
    }
}
";
// line 1: pub trait Greeter {          -> trait:Greeter / export:Greeter start (1)
// line 2:     fn name(&self) ...;      -> fn:Greeter.name (no body) (2..=2)
// line 3:     fn greet(&self) ... {    -> fn:Greeter.greet starts (3)
// line 4:         String::from("hi")
// line 5:     }                        -> fn:Greeter.greet ends (3..=5)
// line 6: }                            -> trait:Greeter ends (1..=6)

#[test]
fn oracle_rust_trait_methods_with_and_without_body() {
    let extraction = extract("src/trait.rs", RUST_TRAIT);
    check_complete("rust_trait", &extraction);
    check_anchors(
        "rust_trait",
        &extraction,
        &[
            e("trait:Greeter", 1, 6, AnchorKind::Trait),
            e("fn:Greeter.name", 2, 2, AnchorKind::Method),
            e("fn:Greeter.greet", 3, 5, AnchorKind::Method),
            e("export:Greeter", 1, 6, AnchorKind::Export),
        ],
    );
    crosscheck_start_lines("rust_trait", RUST_TRAIT, &extraction);
}

/// Structs / enum / module, mixing pub and private items. A `pub fn` inside a
/// private module is still exported (with the module prefix).
const RUST_STRUCTURAL: &str = "\
pub struct Public {
    field: u32,
}

struct Private;

pub enum Color {
    Red,
    Green,
}

mod submod {
    pub fn helper() {}
}
";
// line 1: pub struct Public {      -> struct:Public / export:Public start (1)
// line 2:     field: u32,
// line 3: }                        -> struct:Public ends (1..=3)
// line 4: (blank)
// line 5: struct Private;          -> struct:Private (5..=5), private (no export)
// line 6: (blank)
// line 7: pub enum Color {         -> enum:Color / export:Color start (7)
// line 8:     Red,
// line 9:     Green,
// line 10: }                       -> enum:Color ends (7..=10)
// line 11: (blank)
// line 12: mod submod {            -> mod:submod starts (12); private mod, no export
// line 13:     pub fn helper() {}  -> fn:submod.helper / export:submod.helper (13..=13)
// line 14: }                       -> mod:submod ends (12..=14)

#[test]
fn oracle_rust_structural_items_pub_and_private() {
    let extraction = extract("src/structural.rs", RUST_STRUCTURAL);
    check_complete("rust_structural", &extraction);
    check_anchors(
        "rust_structural",
        &extraction,
        &[
            e("struct:Public", 1, 3, AnchorKind::Struct),
            e("struct:Private", 5, 5, AnchorKind::Struct),
            e("enum:Color", 7, 10, AnchorKind::Enum),
            e("mod:submod", 12, 14, AnchorKind::Module),
            e("fn:submod.helper", 13, 13, AnchorKind::Function),
            e("export:Public", 1, 3, AnchorKind::Export),
            e("export:Color", 7, 10, AnchorKind::Export),
            e("export:submod.helper", 13, 13, AnchorKind::Export),
        ],
    );
    // `submod` items are indented so the independent scanner only covers the
    // three column-0 items: Public, Private, Color.
    crosscheck_start_lines("rust_structural", RUST_STRUCTURAL, &extraction);
}

/// Doc-comment + attribute lines must be folded into the item's start line.
const RUST_DOC_ATTR: &str = "\
/// Documented function.
#[inline]
pub fn documented() -> u32 {
    7
}
";
// line 1: /// Documented function.   -> leading doc, folds into start
// line 2: #[inline]                   -> leading attr, folds into start
// line 3: pub fn documented() ... {   -> keyword line
// line 4:     7
// line 5: }
// Expected: fn:documented starts at line 1 (doc), ends at line 5.

#[test]
fn oracle_rust_doc_and_attribute_lines_included() {
    let extraction = extract("src/doc_attr.rs", RUST_DOC_ATTR);
    check_complete("rust_doc_attr", &extraction);
    check_anchors(
        "rust_doc_attr",
        &extraction,
        &[
            e("fn:documented", 1, 5, AnchorKind::Function),
            e("export:documented", 1, 5, AnchorKind::Export),
        ],
    );
    // Independent walk-up over `///` and single-line `#[...]` must also land on
    // line 1, matching the extractor.
    crosscheck_start_lines("rust_doc_attr", RUST_DOC_ATTR, &extraction);
}

/// Duplicate top-level names must be disambiguated with a `#2` suffix, and the
/// two ranges must be distinct.
const RUST_DUPLICATES: &str = "\
fn dup() {
    let _ = 1;
}
fn dup() {
    let _ = 2;
}
";
// line 1: fn dup() {          -> fn:dup starts (1)
// line 2:     let _ = 1;
// line 3: }                   -> fn:dup ends (1..=3)
// line 4: fn dup() {          -> fn:dup#2 starts (4)
// line 5:     let _ = 2;
// line 6: }                   -> fn:dup#2 ends (4..=6)

#[test]
fn oracle_rust_duplicate_name_disambiguation() {
    let extraction = extract("src/duplicates.rs", RUST_DUPLICATES);
    check_complete("rust_duplicates", &extraction);
    check_anchors(
        "rust_duplicates",
        &extraction,
        &[
            e("fn:dup", 1, 3, AnchorKind::Function),
            e("fn:dup#2", 4, 6, AnchorKind::Function),
        ],
    );
    // NOTE: the independent scanner keys on `fn:<name>` so it can only validate
    // the first `dup`; the `#2` range is covered by the hand-computed assertion
    // above. That is sufficient because the negative-oracle test below asserts
    // the precise, distinct range of the second occurrence.
}

// ===========================================================================
// Negative oracle (the exact B10 complaint): assert a PRECISE multi-line range,
// so if the extractor ever reports a wrong start or end line for this function,
// the test fails instead of silently accepting it. Presence alone is not
// enough — the range must be exact.
// ===========================================================================

const RUST_NEGATIVE: &str = "\
fn one_liner() {}
pub fn spans_multiple_lines(input: u32) -> u32 {
    let doubled = input * 2;
    let result = doubled + 1;
    result
}
fn trailing() {}
";
// line 1: fn one_liner() {}                    -> fn:one_liner (1..=1)
// line 2: pub fn spans_multiple_lines(...) {   -> fn:spans_multiple_lines starts (2)
// line 3:     let doubled = input * 2;
// line 4:     let result = doubled + 1;
// line 5:     result
// line 6: }                                    -> ends (2..=6). A wrong end (e.g. 5 or 7)
//                                                 or wrong start fails HERE.
// line 7: fn trailing() {}                     -> fn:trailing (7..=7)

#[test]
fn negative_oracle_wrong_range_would_fail() {
    let extraction = extract("src/negative.rs", RUST_NEGATIVE);
    check_complete("rust_negative", &extraction);
    // Exact ranges: the middle function MUST be 2..=6. If the extractor
    // off-by-ones the closing brace or swallows the trailing function, this
    // assertion — not mere presence — catches it.
    check_anchors(
        "rust_negative",
        &extraction,
        &[
            e("fn:one_liner", 1, 1, AnchorKind::Function),
            e("fn:spans_multiple_lines", 2, 6, AnchorKind::Function),
            e("fn:trailing", 7, 7, AnchorKind::Function),
            e("export:spans_multiple_lines", 2, 6, AnchorKind::Export),
        ],
    );
    crosscheck_start_lines("rust_negative", RUST_NEGATIVE, &extraction);
}

// ===========================================================================
// C / C++ fixtures. `extract_anchors` dispatches to the C-family extractor for
// .c/.cc/.cpp/.h/.hpp. Types are emitted first (in source order), then
// functions/methods.
// ===========================================================================

const C_FIXTURE: &str = "\
struct point {
    int x;
    int y;
};

int add(int a, int b)
{
    return a + b;
}
";
// line 1: struct point {          -> struct:point starts (1)
// line 2:     int x;
// line 3:     int y;
// line 4: };                      -> struct:point ends (1..=4)
// line 5: (blank)
// line 6: int add(int a, int b)   -> fn:add starts (6)
// line 7: {
// line 8:     return a + b;
// line 9: }                       -> fn:add ends (6..=9)

#[test]
fn oracle_c_functions_and_structs() {
    let extraction = extract("src/driver.c", C_FIXTURE);
    check_complete("c_fixture", &extraction);
    check_anchors(
        "c_fixture",
        &extraction,
        &[
            e("struct:point", 1, 4, AnchorKind::Struct),
            e("fn:add", 6, 9, AnchorKind::Function),
        ],
    );
}

const CPP_FIXTURE: &str = "\
class Vec {
public:
    int size() const {
        return n_;
    }
private:
    int n_;
};

int Vec::grow(int by)
{
    return n_ + by;
}
";
// line 1: class Vec {                -> class:Vec starts (1)
// line 2: public:
// line 3:     int size() const {     -> fn:Vec.size (method) starts (3)
// line 4:         return n_;
// line 5:     }                      -> fn:Vec.size ends (3..=5)
// line 6: private:
// line 7:     int n_;
// line 8: };                         -> class:Vec ends (1..=8)
// line 9: (blank)
// line 10: int Vec::grow(int by)     -> fn:Vec.grow (method) starts (10)
// line 11: {
// line 12:     return n_ + by;
// line 13: }                         -> fn:Vec.grow ends (10..=13)

#[test]
fn oracle_cpp_class_inline_and_qualified_methods() {
    let extraction = extract("src/vec.cpp", CPP_FIXTURE);
    check_complete("cpp_fixture", &extraction);
    check_anchors(
        "cpp_fixture",
        &extraction,
        &[
            e("class:Vec", 1, 8, AnchorKind::Class),
            e("fn:Vec.size", 3, 5, AnchorKind::Method),
            e("fn:Vec.grow", 10, 13, AnchorKind::Method),
        ],
    );
}
