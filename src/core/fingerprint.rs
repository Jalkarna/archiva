use crate::core::hash::sha256;

pub fn normalize_code(content: &str) -> String {
    split_js_lines(content)
        .into_iter()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
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
}
