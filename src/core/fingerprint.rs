use crate::core::hash::sha256;

/// Whitespace exactly as JavaScript's regex `\s` matches it, which is the
/// character class the TypeScript oracle uses to normalize code before
/// fingerprinting (`line.trim().replace(/\s+/g, " ")`). This deliberately does
/// NOT use Rust's `char::is_whitespace` (Unicode White_Space): the two classes
/// diverge on U+FEFF (BOM) — JS `\s` matches it, Unicode White_Space does not —
/// and on U+0085 (NEL) — the reverse. A leading UTF-8 BOM is common in
/// Windows-authored files, so using the wrong class made fingerprints of
/// byte-identical code differ across the TS→Rust migration and produced phantom
/// STALE results (audit finding F19). Matching JS `\s` keeps fingerprints
/// TS-compatible.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

pub fn normalize_code(content: &str) -> String {
    split_js_lines(content)
        .into_iter()
        .map(|line| {
            line.split(is_js_whitespace)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn fingerprint(content: &str) -> String {
    sha256::digest_hex(normalize_code(content).as_bytes())
        .chars()
        .take(8)
        .collect()
}

pub fn get_lines(content: &str, start: usize, end: usize) -> String {
    split_js_lines(content)
        .into_iter()
        .skip(start.saturating_sub(1))
        .take(end.saturating_sub(start).saturating_add(1))
        .collect::<Vec<_>>()
        .join("\n")
}

fn split_js_lines(content: &str) -> Vec<&str> {
    let bytes = content.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\n' {
            let end = if index > start && bytes[index - 1] == b'\r' {
                index - 1
            } else {
                index
            };
            lines.push(&content[start..end]);
            start = index + 1;
        }
        index += 1;
    }
    lines.push(&content[start..]);
    lines
}

#[cfg(test)]
mod tests {
    use super::{fingerprint, get_lines, normalize_code};

    #[test]
    fn matches_typescript_normalization_contract() {
        assert_eq!(normalize_code("  const   x   =   1;\n\n"), "const x = 1;");
        assert_eq!(fingerprint("const x = 1;\n"), "3f41cbb3");
        assert_eq!(fingerprint("  const   x   =   1;\n\n"), "3f41cbb3");
        assert_eq!(
            normalize_code("function kept() {\n  return 1;\n}\n"),
            "function kept() {\nreturn 1;\n}"
        );
        assert_eq!(
            fingerprint("function kept() {\n  return 1;\n}\n"),
            "479b6cd1"
        );
    }

    #[test]
    fn gets_one_based_inclusive_lines_with_crlf_normalization() {
        assert_eq!(get_lines("a\r\nb\r\nc\r\n", 2, 3), "b\nc");
        assert_eq!(get_lines("a\nb\nc", 1, 2), "a\nb");
    }

    #[test]
    fn normalizes_whitespace_like_the_javascript_regex_class() {
        // F19: fingerprints must match the TS oracle's `\s` normalization, not
        // Rust's Unicode White_Space. The two diverge on U+FEFF (BOM) and
        // U+0085 (NEL). Values below are the JS-oracle fingerprints from the
        // audit.
        //
        // U+FEFF is JS whitespace: a leading BOM and a mid-line BOM collapse
        // exactly like a space, so BOM-containing files stay TS-compatible.
        assert_eq!(fingerprint("\u{FEFF}abc"), fingerprint("abc"));
        assert_eq!(fingerprint("\u{FEFF}abc"), "ba7816bf");
        assert_eq!(fingerprint("a\u{FEFF}b"), fingerprint("a b"));
        assert_eq!(fingerprint("a\u{FEFF}b"), "c8687a08");

        // U+0085 (NEL) is NOT matched by JS `\s`, so it must be preserved (Rust's
        // char::is_whitespace would wrongly collapse it).
        assert_ne!(fingerprint("a\u{0085}b"), fingerprint("a b"));

        // Codepoints the audit confirmed both implementations already agree on
        // as whitespace remain collapsed.
        for space in ["\u{00A0}", "\u{2028}", "\u{2029}", "\u{3000}", "\u{000B}"] {
            assert_eq!(
                fingerprint(&format!("a{space}b")),
                fingerprint("a b"),
                "expected {space:?} to normalize like a space"
            );
        }
        // U+200B (zero-width space) is whitespace in neither implementation.
        assert_ne!(fingerprint("a\u{200B}b"), fingerprint("a b"));
    }
}
