use crate::core::dlog::LineRange;

const FULL_LCS_CELL_LIMIT: usize = 1_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LineChange {
    Equal { count: u32 },
    Added { count: u32 },
    Removed { count: u32 },
}

pub fn apply_diff_to_range(old_content: &str, new_content: &str, range: LineRange) -> LineRange {
    let changes = diff_lines(old_content, new_content);
    apply_line_changes_to_range(&changes, range)
}

pub fn apply_line_changes_to_range(changes: &[LineChange], range: LineRange) -> LineRange {
    let mut old_line = 1_i64;
    let mut offset = 0_i64;
    let start = range.start as i64;
    let end = range.end as i64;

    for change in changes {
        match change {
            LineChange::Equal { count } => old_line += *count as i64,
            LineChange::Added { count } => {
                if old_line <= start {
                    offset += *count as i64;
                }
            }
            LineChange::Removed { count } => {
                if old_line + *count as i64 - 1 < start {
                    offset -= *count as i64;
                }
                old_line += *count as i64;
            }
        }
    }

    LineRange {
        start: (start + offset).max(1) as u32,
        end: (end + offset).max(1) as u32,
    }
}

pub fn diff_lines(old_content: &str, new_content: &str) -> Vec<LineChange> {
    let old_lines = logical_lines(old_content);
    let new_lines = logical_lines(new_content);
    let cells = (old_lines.len() + 1).saturating_mul(new_lines.len() + 1);
    if cells <= FULL_LCS_CELL_LIMIT {
        return full_table_diff_lines(&old_lines, &new_lines);
    }
    linear_memory_diff_lines(&old_lines, &new_lines)
}

fn linear_memory_diff_lines(old_lines: &[&str], new_lines: &[&str]) -> Vec<LineChange> {
    let mut changes = Vec::new();
    build_changes(old_lines, new_lines, &mut changes);
    coalesce_changes(changes)
}

fn full_table_diff_lines(old_lines: &[&str], new_lines: &[&str]) -> Vec<LineChange> {
    let mut table = vec![vec![0; new_lines.len() + 1]; old_lines.len() + 1];
    for old_index in (0..old_lines.len()).rev() {
        for new_index in (0..new_lines.len()).rev() {
            table[old_index][new_index] = if old_lines[old_index] == new_lines[new_index] {
                table[old_index + 1][new_index + 1] + 1
            } else {
                table[old_index + 1][new_index].max(table[old_index][new_index + 1])
            };
        }
    }
    let mut changes = Vec::new();
    build_full_table_changes(old_lines, new_lines, &table, 0, 0, &mut changes);
    coalesce_changes(changes)
}

fn build_full_table_changes(
    old_lines: &[&str],
    new_lines: &[&str],
    table: &[Vec<usize>],
    old_index: usize,
    new_index: usize,
    changes: &mut Vec<LineChange>,
) {
    if old_index == old_lines.len() {
        for _ in new_index..new_lines.len() {
            changes.push(LineChange::Added { count: 1 });
        }
        return;
    }
    if new_index == new_lines.len() {
        for _ in old_index..old_lines.len() {
            changes.push(LineChange::Removed { count: 1 });
        }
        return;
    }
    if old_lines[old_index] == new_lines[new_index] {
        changes.push(LineChange::Equal { count: 1 });
        build_full_table_changes(
            old_lines,
            new_lines,
            table,
            old_index + 1,
            new_index + 1,
            changes,
        );
        return;
    }
    if table[old_index + 1][new_index] >= table[old_index][new_index + 1] {
        changes.push(LineChange::Removed { count: 1 });
        build_full_table_changes(
            old_lines,
            new_lines,
            table,
            old_index + 1,
            new_index,
            changes,
        );
    } else {
        changes.push(LineChange::Added { count: 1 });
        build_full_table_changes(
            old_lines,
            new_lines,
            table,
            old_index,
            new_index + 1,
            changes,
        );
    }
}

fn build_changes<'a>(old_lines: &[&'a str], new_lines: &[&'a str], changes: &mut Vec<LineChange>) {
    if old_lines.is_empty() {
        push_change(
            changes,
            LineChange::Added {
                count: new_lines.len() as u32,
            },
        );
        return;
    }
    if new_lines.is_empty() {
        push_change(
            changes,
            LineChange::Removed {
                count: old_lines.len() as u32,
            },
        );
        return;
    }

    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    if prefix > 0 {
        push_change(
            changes,
            LineChange::Equal {
                count: prefix as u32,
            },
        );
    }

    let old_tail = &old_lines[prefix..];
    let new_tail = &new_lines[prefix..];
    if append_contiguous_insertion(old_tail, new_tail, changes)
        || append_contiguous_deletion(old_tail, new_tail, changes)
    {
        return;
    }
    build_middle_changes(old_tail, new_tail, changes);
}

fn build_middle_changes<'a>(
    old_lines: &[&'a str],
    new_lines: &[&'a str],
    changes: &mut Vec<LineChange>,
) {
    if old_lines.is_empty() {
        push_change(
            changes,
            LineChange::Added {
                count: new_lines.len() as u32,
            },
        );
        return;
    }
    if new_lines.is_empty() {
        push_change(
            changes,
            LineChange::Removed {
                count: old_lines.len() as u32,
            },
        );
        return;
    }

    if old_lines.len() == 1 {
        match new_lines.iter().position(|line| *line == old_lines[0]) {
            Some(index) => {
                push_change(
                    changes,
                    LineChange::Added {
                        count: index as u32,
                    },
                );
                push_change(changes, LineChange::Equal { count: 1 });
                push_change(
                    changes,
                    LineChange::Added {
                        count: (new_lines.len() - index - 1) as u32,
                    },
                );
            }
            None => {
                push_change(changes, LineChange::Removed { count: 1 });
                push_change(
                    changes,
                    LineChange::Added {
                        count: new_lines.len() as u32,
                    },
                );
            }
        }
        return;
    }

    if new_lines.len() == 1 {
        match old_lines.iter().position(|line| *line == new_lines[0]) {
            Some(index) => {
                push_change(
                    changes,
                    LineChange::Removed {
                        count: index as u32,
                    },
                );
                push_change(changes, LineChange::Equal { count: 1 });
                push_change(
                    changes,
                    LineChange::Removed {
                        count: (old_lines.len() - index - 1) as u32,
                    },
                );
            }
            None => {
                push_change(
                    changes,
                    LineChange::Removed {
                        count: old_lines.len() as u32,
                    },
                );
                push_change(changes, LineChange::Added { count: 1 });
            }
        }
        return;
    }

    let old_mid = old_lines.len() / 2;
    let forward = lcs_prefix_lengths(&old_lines[..old_mid], new_lines);
    let reverse = lcs_suffix_lengths(&old_lines[old_mid..], new_lines);
    let mut split = 0;
    let mut best = 0;
    for new_index in 0..=new_lines.len() {
        let score = forward[new_index] + reverse[new_lines.len() - new_index];
        if score > best {
            best = score;
            split = new_index;
        }
    }
    build_changes(&old_lines[..old_mid], &new_lines[..split], changes);
    build_changes(&old_lines[old_mid..], &new_lines[split..], changes);
}

fn lcs_prefix_lengths(old_lines: &[&str], new_lines: &[&str]) -> Vec<u32> {
    let mut previous = vec![0_u32; new_lines.len() + 1];
    let mut current = vec![0_u32; new_lines.len() + 1];
    for old_line in old_lines {
        for new_index in 0..new_lines.len() {
            current[new_index + 1] = if *old_line == new_lines[new_index] {
                previous[new_index] + 1
            } else {
                current[new_index].max(previous[new_index + 1])
            };
        }
        std::mem::swap(&mut previous, &mut current);
        current.fill(0);
    }
    previous
}

fn lcs_suffix_lengths(old_lines: &[&str], new_lines: &[&str]) -> Vec<u32> {
    let reversed_old = old_lines.iter().rev().copied().collect::<Vec<_>>();
    let reversed_new = new_lines.iter().rev().copied().collect::<Vec<_>>();
    lcs_prefix_lengths(&reversed_old, &reversed_new)
}

fn append_contiguous_insertion(
    old_lines: &[&str],
    new_lines: &[&str],
    changes: &mut Vec<LineChange>,
) -> bool {
    if old_lines.len() > new_lines.len() {
        return false;
    }
    let Some(index) = find_contiguous_subsequence(new_lines, old_lines) else {
        return false;
    };
    if index > 0 && new_lines[..index].contains(&old_lines[0]) {
        return false;
    }
    push_change(
        changes,
        LineChange::Added {
            count: index as u32,
        },
    );
    push_change(
        changes,
        LineChange::Equal {
            count: old_lines.len() as u32,
        },
    );
    push_change(
        changes,
        LineChange::Added {
            count: (new_lines.len() - index - old_lines.len()) as u32,
        },
    );
    true
}

fn append_contiguous_deletion(
    old_lines: &[&str],
    new_lines: &[&str],
    changes: &mut Vec<LineChange>,
) -> bool {
    if new_lines.len() > old_lines.len() {
        return false;
    }
    let Some(index) = find_contiguous_subsequence(old_lines, new_lines) else {
        return false;
    };
    if index > 0 && old_lines[..index].contains(&new_lines[0]) {
        return false;
    }
    push_change(
        changes,
        LineChange::Removed {
            count: index as u32,
        },
    );
    push_change(
        changes,
        LineChange::Equal {
            count: new_lines.len() as u32,
        },
    );
    push_change(
        changes,
        LineChange::Removed {
            count: (old_lines.len() - index - new_lines.len()) as u32,
        },
    );
    true
}

fn find_contiguous_subsequence(haystack: &[&str], needle: &[&str]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn push_change(changes: &mut Vec<LineChange>, change: LineChange) {
    let count = match change {
        LineChange::Equal { count }
        | LineChange::Added { count }
        | LineChange::Removed { count } => count,
    };
    if count == 0 {
        return;
    }
    changes.push(change);
}

fn coalesce_changes(changes: Vec<LineChange>) -> Vec<LineChange> {
    let mut output = Vec::new();
    for change in changes {
        match (output.last_mut(), change) {
            (Some(LineChange::Equal { count }), LineChange::Equal { count: next }) => {
                *count += next
            }
            (Some(LineChange::Added { count }), LineChange::Added { count: next }) => {
                *count += next
            }
            (Some(LineChange::Removed { count }), LineChange::Removed { count: next }) => {
                *count += next
            }
            (_, change) => output.push(change),
        }
    }
    output
}

fn logical_lines(input: &str) -> Vec<&str> {
    if input.is_empty() {
        return Vec::new();
    }
    if input == "\n" || input == "\r\n" {
        return Vec::new();
    }

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

    if start < input.len() {
        lines.push(&input[start..]);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{
        apply_diff_to_range, apply_line_changes_to_range, diff_lines, logical_lines, LineChange,
    };
    use crate::core::dlog::LineRange;

    #[test]
    fn shifts_ranges_like_typescript_apply_diff_to_range_fixture() {
        let cases = [
            ("a\nb\nc\n", "x\na\nb\nc\n", 2, 3, 3, 4),
            ("a\nb\nc\n", "a\nx\nb\nc\n", 2, 3, 3, 4),
            ("a\nb\nc\n", "a\nb\nx\nc\n", 2, 3, 2, 3),
            ("a\nb\nc\nd\n", "b\nc\nd\n", 3, 4, 2, 3),
            ("a\nb\nc\nd\n", "a\nc\nd\n", 2, 4, 2, 4),
            ("a\nb\nc\nd\n", "a\nb\nd\n", 2, 4, 2, 4),
            ("a\nb\nc\n", "x\nb\nc\n", 2, 3, 2, 3),
            ("a\nb\nc\n", "a\nx\nc\n", 2, 3, 2, 3),
            ("a\nb\nc", "a\nx\nb\nc", 2, 3, 3, 4),
            ("a\r\nb\r\nc\r\n", "a\r\nx\r\nb\r\nc\r\n", 2, 3, 3, 4),
        ];

        for (old_content, new_content, start, end, expected_start, expected_end) in cases {
            assert_eq!(
                apply_diff_to_range(old_content, new_content, LineRange { start, end }),
                LineRange {
                    start: expected_start,
                    end: expected_end
                }
            );
        }
    }

    #[test]
    fn applies_precomputed_changes_to_multiple_ranges_like_single_range_helper() {
        let old_content = "a\nb\nc\nd\n";
        let new_content = "x\na\nb\nd\n";
        let changes = diff_lines(old_content, new_content);
        for range in [
            LineRange { start: 1, end: 1 },
            LineRange { start: 2, end: 3 },
            LineRange { start: 4, end: 4 },
        ] {
            assert_eq!(
                apply_line_changes_to_range(&changes, range.clone()),
                apply_diff_to_range(old_content, new_content, range)
            );
        }
    }

    #[test]
    fn emits_removed_then_added_for_replacements() {
        assert_eq!(
            diff_lines("a\nb\nc\n", "a\nx\nc\n"),
            vec![
                LineChange::Equal { count: 1 },
                LineChange::Removed { count: 1 },
                LineChange::Added { count: 1 },
                LineChange::Equal { count: 1 },
            ]
        );
    }

    #[test]
    fn linear_diff_matches_full_table_for_small_repeated_inputs() {
        let values = generated_line_inputs(4);
        for old_content in &values {
            for new_content in &values {
                assert_eq!(
                    diff_lines(old_content, new_content),
                    full_table_diff_lines(old_content, new_content),
                    "old={old_content:?} new={new_content:?}"
                );
            }
        }
    }

    #[test]
    fn shifts_large_top_insert_without_full_lcs_matrix() {
        let mut old_content = String::new();
        for index in 0..20_000 {
            old_content.push_str(&format!("line {index}\n"));
        }
        let new_content = format!("inserted\n{old_content}");
        assert_eq!(
            apply_diff_to_range(
                &old_content,
                &new_content,
                LineRange {
                    start: 19_990,
                    end: 20_000
                },
            ),
            LineRange {
                start: 19_991,
                end: 20_001
            }
        );
    }

    #[test]
    fn clamps_shifted_ranges_to_one() {
        assert_eq!(
            apply_diff_to_range("a\nb\n", "", LineRange { start: 1, end: 2 }),
            LineRange { start: 1, end: 2 }
        );
        assert_eq!(
            apply_diff_to_range("a\nb\n", "b\n", LineRange { start: 2, end: 2 }),
            LineRange { start: 1, end: 1 }
        );
    }

    #[test]
    fn tokenizes_newline_counts_like_typescript_reanchor_helper() {
        assert!(logical_lines("").is_empty());
        assert!(logical_lines("\n").is_empty());
        assert!(logical_lines("\r\n").is_empty());
        assert_eq!(logical_lines("\n\n"), vec!["", ""]);
        assert_eq!(logical_lines("a\r\nb\n"), vec!["a", "b"]);
        assert_eq!(logical_lines("a\nb"), vec!["a", "b"]);
    }

    fn generated_line_inputs(max_len: usize) -> Vec<String> {
        let alphabet = ["a", "b"];
        let mut output = vec![String::new()];
        for len in 1..=max_len {
            let combinations = 1_usize << len;
            for mask in 0..combinations {
                let mut value = String::new();
                for bit in 0..len {
                    value.push_str(alphabet[(mask >> bit) & 1]);
                    value.push('\n');
                }
                output.push(value);
            }
        }
        output
    }

    fn full_table_diff_lines(old_content: &str, new_content: &str) -> Vec<LineChange> {
        let old_lines = logical_lines(old_content);
        let new_lines = logical_lines(new_content);
        let mut table = vec![vec![0; new_lines.len() + 1]; old_lines.len() + 1];
        for old_index in (0..old_lines.len()).rev() {
            for new_index in (0..new_lines.len()).rev() {
                table[old_index][new_index] = if old_lines[old_index] == new_lines[new_index] {
                    table[old_index + 1][new_index + 1] + 1
                } else {
                    table[old_index + 1][new_index].max(table[old_index][new_index + 1])
                };
            }
        }
        let mut changes = Vec::new();
        build_full_table_changes(&old_lines, &new_lines, &table, 0, 0, &mut changes);
        super::coalesce_changes(changes)
    }

    fn build_full_table_changes(
        old_lines: &[&str],
        new_lines: &[&str],
        table: &[Vec<usize>],
        old_index: usize,
        new_index: usize,
        changes: &mut Vec<LineChange>,
    ) {
        if old_index == old_lines.len() {
            for _ in new_index..new_lines.len() {
                changes.push(LineChange::Added { count: 1 });
            }
            return;
        }
        if new_index == new_lines.len() {
            for _ in old_index..old_lines.len() {
                changes.push(LineChange::Removed { count: 1 });
            }
            return;
        }
        if old_lines[old_index] == new_lines[new_index] {
            changes.push(LineChange::Equal { count: 1 });
            build_full_table_changes(
                old_lines,
                new_lines,
                table,
                old_index + 1,
                new_index + 1,
                changes,
            );
            return;
        }
        if table[old_index + 1][new_index] >= table[old_index][new_index + 1] {
            changes.push(LineChange::Removed { count: 1 });
            build_full_table_changes(
                old_lines,
                new_lines,
                table,
                old_index + 1,
                new_index,
                changes,
            );
        } else {
            changes.push(LineChange::Added { count: 1 });
            build_full_table_changes(
                old_lines,
                new_lines,
                table,
                old_index,
                new_index + 1,
                changes,
            );
        }
    }
}
