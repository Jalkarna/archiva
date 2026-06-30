use std::cmp::Ordering;
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecisionStatus {
    Undecided,
    Stale,
    Orphan,
}

impl DecisionStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "UNDECIDED" => Some(Self::Undecided),
            "STALE" => Some(Self::Stale),
            "ORPHAN" => Some(Self::Orphan),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Undecided => "UNDECIDED",
            Self::Stale => "STALE",
            Self::Orphan => "ORPHAN",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DmapEntry {
    pub start_line: i64,
    pub end_line: i64,
    pub anchor: String,
    pub status: Option<DecisionStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DmapErrorKind {
    InvalidLine,
    InvalidRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DmapError {
    kind: DmapErrorKind,
    line: String,
}

impl DmapError {
    fn invalid_line(line: &str) -> Self {
        Self {
            kind: DmapErrorKind::InvalidLine,
            line: line.to_string(),
        }
    }

    fn invalid_range(line: &str) -> Self {
        Self {
            kind: DmapErrorKind::InvalidRange,
            line: line.to_string(),
        }
    }

    pub fn kind(&self) -> &DmapErrorKind {
        &self.kind
    }

    pub fn line(&self) -> &str {
        &self.line
    }
}

impl fmt::Display for DmapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            DmapErrorKind::InvalidLine => write!(formatter, "Invalid .dmap line: {}", self.line),
            DmapErrorKind::InvalidRange => write!(formatter, "Invalid .dmap range: {}", self.line),
        }
    }
}

impl Error for DmapError {}

pub fn parse_dmap(input: &str) -> Result<Vec<DmapEntry>, DmapError> {
    let mut entries = Vec::new();
    for raw_line in split_js_lines(input) {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        entries.push(parse_line(line)?);
    }
    Ok(entries)
}

pub fn render_dmap(entries: &[DmapEntry]) -> String {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|left, right| {
        left.start_line
            .cmp(&right.start_line)
            .then_with(|| js_locale_compare_ascii_latin(&left.anchor, &right.anchor))
    });

    let mut output = String::new();
    for entry in sorted {
        output.push_str(&entry.start_line.to_string());
        output.push('-');
        output.push_str(&entry.end_line.to_string());
        output.push(':');
        output.push_str(&entry.anchor);
        if let Some(status) = entry.status {
            output.push(':');
            output.push_str(status.as_str());
        }
        output.push('\n');
    }
    output
}

fn parse_line(line: &str) -> Result<DmapEntry, DmapError> {
    let Some(first_colon) = line.find(':') else {
        return Err(DmapError::invalid_line(line));
    };
    let range_part = &line[..first_colon];
    let rest = &line[first_colon + 1..];
    let (anchor, status) = match rest.rfind(':') {
        Some(last_colon) => {
            let possible_status = &rest[last_colon + 1..];
            if let Some(status) = DecisionStatus::parse(possible_status) {
                (&rest[..last_colon], Some(status))
            } else {
                (rest, None)
            }
        }
        None => (rest, None),
    };

    if range_part.is_empty() || anchor.is_empty() {
        return Err(DmapError::invalid_line(line));
    }

    let (start, end) =
        parse_range_like_javascript(range_part).ok_or_else(|| DmapError::invalid_range(line))?;
    Ok(DmapEntry {
        start_line: start,
        end_line: end,
        anchor: anchor.to_string(),
        status,
    })
}

fn parse_range_like_javascript(range: &str) -> Option<(i64, i64)> {
    let mut parts = range.split('-');
    let start = parse_js_integer(parts.next()?)?;
    let end = parse_js_integer(parts.next()?)?;
    Some((start, end))
}

fn parse_js_integer(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Some(0);
    }
    if trimmed.eq_ignore_ascii_case("infinity") || trimmed.eq_ignore_ascii_case("nan") {
        return None;
    }

    let parsed = trimmed.parse::<f64>().ok()?;
    if !parsed.is_finite() || parsed.fract() != 0.0 {
        return None;
    }
    if parsed < i64::MIN as f64 || parsed > i64::MAX as f64 {
        return None;
    }
    Some(parsed as i64)
}

fn split_js_lines(input: &str) -> Vec<&str> {
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

fn js_locale_compare_ascii_latin(left: &str, right: &str) -> Ordering {
    let mut left_chars = left.chars();
    let mut right_chars = right.chars();
    loop {
        match (left_chars.next(), right_chars.next()) {
            (Some(left_char), Some(right_char)) => {
                let order = collator_char_key(left_char).cmp(&collator_char_key(right_char));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, None) => return Ordering::Equal,
        }
    }
}

fn collator_char_key(character: char) -> (u32, u32) {
    match character {
        '_' => (0, 0),
        '-' => (1, 0),
        ':' => (2, 0),
        '0'..='9' => (10 + character as u32 - '0' as u32, 0),
        'a'..='z' => (100 + character as u32 - 'a' as u32, 0),
        'A'..='Z' => (100 + character as u32 - 'A' as u32, 1),
        '\u{00e1}' => (100, 2),
        '\u{00c1}' => (100, 3),
        '\u{00e0}' => (100, 4),
        '\u{00c0}' => (100, 5),
        '\u{00e2}' => (100, 6),
        '\u{00c2}' => (100, 7),
        '\u{00e4}' => (100, 8),
        '\u{00c4}' => (100, 9),
        _ => (10_000 + character as u32, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_dmap, render_dmap, DecisionStatus, DmapEntry};

    #[test]
    fn parses_typescript_compatible_dmap_lines() {
        assert_eq!(
            parse_dmap("  1-2:fn:trim  \r\n\r\n 3-4:fn:colon:ORPHAN \n").unwrap(),
            vec![
                DmapEntry {
                    start_line: 1,
                    end_line: 2,
                    anchor: "fn:trim".to_string(),
                    status: None,
                },
                DmapEntry {
                    start_line: 3,
                    end_line: 4,
                    anchor: "fn:colon".to_string(),
                    status: Some(DecisionStatus::Orphan),
                },
            ]
        );
        assert_eq!(parse_dmap("0-1:fn:x\n").unwrap()[0].start_line, 0);
        assert_eq!(parse_dmap("5-2:fn:x\n").unwrap()[0].end_line, 2);
        assert_eq!(parse_dmap("-2:fn:x\n").unwrap()[0].start_line, 0);
        assert_eq!(parse_dmap("1-2-3:fn:x\n").unwrap()[0].end_line, 2);
        assert_eq!(
            parse_dmap("2-3:fn:x:NOT_STATUS\n").unwrap()[0].anchor,
            "fn:x:NOT_STATUS"
        );
        assert_eq!(
            parse_dmap("2-3:fn:x:STALE:ignored\n").unwrap()[0].anchor,
            "fn:x:STALE:ignored"
        );
        assert_eq!(
            parse_dmap("2-3:block:if_x:STALE\n").unwrap()[0].status,
            Some(DecisionStatus::Stale)
        );
        assert_eq!(
            parse_dmap("1-2:\n").unwrap_err().to_string(),
            "Invalid .dmap line: 1-2:"
        );
        assert_eq!(
            parse_dmap("a-2:fn:x\n").unwrap_err().to_string(),
            "Invalid .dmap range: a-2:fn:x"
        );
        assert_eq!(
            parse_dmap("2:fn:x\n").unwrap_err().to_string(),
            "Invalid .dmap range: 2:fn:x"
        );
    }

    #[test]
    fn renders_with_typescript_locale_compare_contract() {
        let entries = vec![
            entry(1, 1, "fn:z", None),
            entry(1, 1, "fn:A", None),
            entry(1, 1, "fn:a", None),
            entry(1, 1, "fn:_", None),
            entry(1, 1, "fn:\u{00e1}", None),
            entry(1, 1, "fn:Z", None),
            entry(2, 3, "block:if_x", Some(DecisionStatus::Stale)),
        ];

        assert_eq!(
            render_dmap(&entries),
            "1-1:fn:_\n1-1:fn:a\n1-1:fn:A\n1-1:fn:\u{00e1}\n1-1:fn:z\n1-1:fn:Z\n2-3:block:if_x:STALE\n"
        );
        assert_eq!(render_dmap(&[]), "");
    }

    fn entry(
        start_line: i64,
        end_line: i64,
        anchor: &str,
        status: Option<DecisionStatus>,
    ) -> DmapEntry {
        DmapEntry {
            start_line,
            end_line,
            anchor: anchor.to_string(),
            status,
        }
    }
}
