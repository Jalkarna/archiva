#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitignoreMatcher {
    patterns: Vec<String>,
}

impl GitignoreMatcher {
    pub fn from_gitignore(input: &str) -> Self {
        Self {
            patterns: parse_gitignore(input),
        }
    }

    pub fn from_patterns(patterns: Vec<String>) -> Self {
        Self { patterns }
    }

    pub fn is_ignored(&self, relative_path: &str) -> bool {
        matches_gitignore(relative_path, &self.patterns)
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }
}

pub fn parse_gitignore(input: &str) -> Vec<String> {
    split_lines_like_typescript(input)
        .into_iter()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

pub fn matches_gitignore(relative_path: &str, patterns: &[String]) -> bool {
    let normalized = relative_path.replace('\\', "/");
    let segments = normalized.split('/').collect::<Vec<_>>();

    for pattern in patterns {
        if pattern.starts_with('!') {
            continue;
        }
        if match_pattern(&normalized, &segments, pattern) {
            return true;
        }
    }
    false
}

fn match_pattern(normalized: &str, segments: &[&str], pattern: &str) -> bool {
    let anchored = pattern.starts_with('/');
    let dir_only = pattern.ends_with('/');
    let mut body = pattern;
    if anchored {
        body = &body[1..];
    }
    if dir_only {
        body = &body[..body.len() - 1];
    }

    let glob = Glob::parse(body);
    if anchored {
        return glob.matches(normalized);
    }
    if dir_only {
        return segments.iter().any(|segment| glob.matches(segment));
    }
    glob.matches(normalized) || segments.iter().any(|segment| glob.matches(segment))
}

fn split_lines_like_typescript(input: &str) -> Vec<&str> {
    let bytes = input.as_bytes();
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
            lines.push(&input[start..end]);
            start = index + 1;
        }
        index += 1;
    }
    lines.push(&input[start..]);
    lines
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Glob {
    tokens: Vec<Token>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Token {
    Literal(char),
    AnySingleNonSlash,
    AnyManyNonSlash,
    AnyMany,
}

impl Glob {
    fn parse(pattern: &str) -> Self {
        let chars = pattern.chars().collect::<Vec<_>>();
        let mut tokens = Vec::new();
        let mut index = 0;
        while index < chars.len() {
            match chars[index] {
                '*' if chars.get(index + 1) == Some(&'*') => {
                    tokens.push(Token::AnyMany);
                    index += 2;
                    if chars.get(index) == Some(&'/') {
                        index += 1;
                    }
                }
                '*' => {
                    tokens.push(Token::AnyManyNonSlash);
                    index += 1;
                }
                '?' => {
                    tokens.push(Token::AnySingleNonSlash);
                    index += 1;
                }
                literal => {
                    tokens.push(Token::Literal(literal));
                    index += 1;
                }
            }
        }
        Self { tokens }
    }

    fn matches(&self, candidate: &str) -> bool {
        let chars = candidate.chars().collect::<Vec<_>>();
        let mut memo = vec![vec![None; chars.len() + 1]; self.tokens.len() + 1];
        self.matches_from(0, 0, &chars, &mut memo)
    }

    fn matches_from(
        &self,
        token_index: usize,
        char_index: usize,
        chars: &[char],
        memo: &mut [Vec<Option<bool>>],
    ) -> bool {
        if let Some(value) = memo[token_index][char_index] {
            return value;
        }

        let result = if token_index == self.tokens.len() {
            char_index == chars.len()
        } else {
            match self.tokens[token_index] {
                Token::Literal(literal) => {
                    chars.get(char_index) == Some(&literal)
                        && self.matches_from(token_index + 1, char_index + 1, chars, memo)
                }
                Token::AnySingleNonSlash => {
                    chars
                        .get(char_index)
                        .is_some_and(|character| *character != '/')
                        && self.matches_from(token_index + 1, char_index + 1, chars, memo)
                }
                Token::AnyManyNonSlash => {
                    self.matches_from(token_index + 1, char_index, chars, memo)
                        || (chars
                            .get(char_index)
                            .is_some_and(|character| *character != '/')
                            && self.matches_from(token_index, char_index + 1, chars, memo))
                }
                Token::AnyMany => {
                    self.matches_from(token_index + 1, char_index, chars, memo)
                        || (char_index < chars.len()
                            && self.matches_from(token_index, char_index + 1, chars, memo))
                }
            }
        };

        memo[token_index][char_index] = Some(result);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{matches_gitignore, parse_gitignore, GitignoreMatcher};

    #[test]
    fn parses_gitignore_like_typescript() {
        assert_eq!(
            parse_gitignore(" ignored.ts \r\n# comment\n\n\t*.test.ts\t\n!rescue.ts\n"),
            vec![
                "ignored.ts".to_string(),
                "*.test.ts".to_string(),
                "!rescue.ts".to_string()
            ]
        );
    }

    #[test]
    fn matches_current_contract_quirks_used_by_lint_scan() {
        let matcher =
            GitignoreMatcher::from_gitignore("ignored.ts\n!rescue.ts\nsrc/generated/\n*.test.ts\n");

        assert!(matcher.is_ignored("ignored.ts"));
        assert!(!matcher.is_ignored("rescue.ts"));
        assert!(!matcher.is_ignored("src/generated/a.ts"));
        assert!(matcher.is_ignored("src/a.test.ts"));
        assert!(!matcher.is_ignored("src/a.ts"));
    }

    #[test]
    fn supports_anchored_unanchored_and_segment_fallback_patterns() {
        let matcher = GitignoreMatcher::from_patterns(vec![
            "/root-only.ts".to_string(),
            "anywhere.ts".to_string(),
            "build".to_string(),
        ]);

        assert!(matcher.is_ignored("root-only.ts"));
        assert!(!matcher.is_ignored("src/root-only.ts"));
        assert!(matcher.is_ignored("src/anywhere.ts"));
        assert!(matcher.is_ignored("src/build/output.ts"));
        assert!(!matcher.is_ignored("src/rebuild/output.ts"));
    }

    #[test]
    fn supports_star_globstar_question_and_backslash_normalization() {
        let patterns = vec![
            "src/**/*.test.ts".to_string(),
            "file?.js".to_string(),
            "tmp/*.log".to_string(),
        ];

        assert!(matches_gitignore("src/a.test.ts", &patterns));
        assert!(matches_gitignore("src/deep/a.test.ts", &patterns));
        assert!(matches_gitignore("nested/file1.js", &patterns));
        assert!(!matches_gitignore("nested/file10.js", &patterns));
        assert!(matches_gitignore("tmp\\debug.log", &patterns));
    }

    #[test]
    fn keeps_typescript_directory_only_behavior() {
        let matcher = GitignoreMatcher::from_gitignore("generated/\n");

        assert!(matcher.is_ignored("src/generated/a.ts"));
        assert!(!matcher.is_ignored("src/generated-by-name/a.ts"));

        let nested_pattern = GitignoreMatcher::from_gitignore("src/generated/\n");
        assert!(!nested_pattern.is_ignored("src/generated/a.ts"));
    }
}
