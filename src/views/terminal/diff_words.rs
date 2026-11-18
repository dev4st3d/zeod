//! Word-level changes inside a replaced line pair: text in, byte ranges out.

use std::ops::Range;

/// Pairs sharing less than this share of their words are rewrites, not edits.
/// Emphasizing nearly every word would only add noise on top of the row tint.
const MIN_SIMILARITY: f32 = 0.4;
/// Largest token product the alignment table may cover for one pair. Beyond
/// it the edited stretch is not aligned, so only the unchanged head and tail
/// count as shared and a pair with scattered edits reads as a rewrite.
const MAX_CELLS: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct WordChanges {
    pub(super) before: Vec<Range<usize>>,
    pub(super) after: Vec<Range<usize>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Symbol,
}

fn class(ch: char) -> Class {
    if ch.is_alphanumeric() || ch == '_' {
        Class::Word
    } else if ch.is_whitespace() {
        Class::Space
    } else {
        Class::Symbol
    }
}

/// Words and whitespace runs are one token each; every other character stands alone.
fn tokens(text: &str) -> Vec<Range<usize>> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, first)) = chars.next() {
        let kind = class(first);
        let mut end = start + first.len_utf8();
        if kind != Class::Symbol {
            while let Some(&(next, ch)) = chars.peek() {
                if class(ch) != kind {
                    break;
                }
                end = next + ch.len_utf8();
                chars.next();
            }
        }
        tokens.push(start..end);
    }
    tokens
}

/// Ranges of `before` and `after` that differ, or `None` when the lines are
/// identical or too dissimilar for word emphasis to help.
pub(super) fn changes(before: &str, after: &str) -> Option<WordChanges> {
    let old = tokens(before);
    let new = tokens(after);
    let same = |i: usize, j: usize| before[old[i].clone()] == after[new[j].clone()];
    let shortest = old.len().min(new.len());
    let prefix = (0..shortest).take_while(|&i| same(i, i)).count();
    let suffix = (0..shortest - prefix)
        .take_while(|&k| same(old.len() - 1 - k, new.len() - 1 - k))
        .count();
    let mut old_kept = vec![false; old.len()];
    let mut new_kept = vec![false; new.len()];
    old_kept[..prefix].fill(true);
    new_kept[..prefix].fill(true);
    old_kept[old.len() - suffix..].fill(true);
    new_kept[new.len() - suffix..].fill(true);

    let (rows, columns) = (old.len() - suffix - prefix, new.len() - suffix - prefix);
    if rows * columns <= MAX_CELLS {
        let width = columns + 1;
        let mut table = vec![0u16; (rows + 1) * width];
        for i in (0..rows).rev() {
            for j in (0..columns).rev() {
                table[i * width + j] = if same(prefix + i, prefix + j) {
                    table[(i + 1) * width + j + 1] + 1
                } else {
                    table[(i + 1) * width + j].max(table[i * width + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < rows && j < columns {
            if same(prefix + i, prefix + j) {
                old_kept[prefix + i] = true;
                new_kept[prefix + j] = true;
                i += 1;
                j += 1;
            } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }

    let (old_words, shared) = word_counts(before, &old, &old_kept);
    let (new_words, _) = word_counts(after, &new, &new_kept);
    if old_words + new_words == 0
        || (2 * shared) as f32 / ((old_words + new_words) as f32) < MIN_SIMILARITY
    {
        return None;
    }

    let result = WordChanges {
        before: changed_ranges(before, &old, &mut old_kept),
        after: changed_ranges(after, &new, &mut new_kept),
    };
    (!result.before.is_empty() || !result.after.is_empty()).then_some(result)
}

/// Non-blank tokens, and how many of them survived unchanged.
fn word_counts(text: &str, tokens: &[Range<usize>], kept: &[bool]) -> (usize, usize) {
    let (mut all, mut shared) = (0, 0);
    for (token, kept) in tokens.iter().zip(kept) {
        if text[token.clone()].trim().is_empty() {
            continue;
        }
        all += 1;
        shared += usize::from(*kept);
    }
    (all, shared)
}

/// Merges unchanged tokens into the surrounding change when only whitespace
/// separates two edits, so `foo bar` → `baz qux` reads as one highlighted run.
fn changed_ranges(text: &str, tokens: &[Range<usize>], kept: &mut [bool]) -> Vec<Range<usize>> {
    for index in 1..tokens.len().saturating_sub(1) {
        if kept[index]
            && !kept[index - 1]
            && !kept[index + 1]
            && text[tokens[index].clone()].trim().is_empty()
        {
            kept[index] = false;
        }
    }
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (token, kept) in tokens.iter().zip(kept.iter()) {
        if *kept {
            continue;
        }
        match ranges.last_mut() {
            Some(last) if last.end == token.start => last.end = token.end,
            _ => ranges.push(token.clone()),
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces<'a>(text: &'a str, ranges: &[Range<usize>]) -> Vec<&'a str> {
        ranges.iter().map(|range| &text[range.clone()]).collect()
    }

    #[test]
    fn a_replaced_word_is_the_only_emphasis() {
        let (old, new) = ("let total = count(1);", "let total = count(2);");
        let changes = changes(old, new).unwrap();
        assert_eq!(pieces(old, &changes.before), ["1"]);
        assert_eq!(pieces(new, &changes.after), ["2"]);
    }

    #[test]
    fn separated_edits_are_aligned_instead_of_spanning_the_line() {
        let (old, new) = ("alpha + keep + beta", "gamma + keep + delta");
        let changes = changes(old, new).unwrap();
        assert_eq!(pieces(old, &changes.before), ["alpha", "beta"]);
        assert_eq!(pieces(new, &changes.after), ["gamma", "delta"]);
    }

    #[test]
    fn additions_only_mark_the_new_side() {
        let (old, new) = ("call(a)", "call(a, b)");
        let changes = changes(old, new).unwrap();
        assert!(changes.before.is_empty());
        assert_eq!(pieces(new, &changes.after), [", b"]);
    }

    #[test]
    fn whitespace_between_two_edits_joins_them() {
        let (old, new) = ("let foo bar = 1;", "let baz qux = 1;");
        let changes = changes(old, new).unwrap();
        assert_eq!(pieces(old, &changes.before), ["foo bar"]);
        assert_eq!(pieces(new, &changes.after), ["baz qux"]);
    }

    #[test]
    fn indentation_changes_are_visible() {
        let changes = changes("    return x;", "  return x;").unwrap();
        assert_eq!(changes.before, [0..4]);
        assert_eq!(changes.after, [0..2]);
    }

    #[test]
    fn ranges_stay_on_character_boundaries_for_unicode() {
        let (old, new) = ("café → 界 x", "café → 界 y");
        let changes = changes(old, new).unwrap();
        assert_eq!(pieces(old, &changes.before), ["x"]);
        assert_eq!(pieces(new, &changes.after), ["y"]);
    }

    #[test]
    fn identical_and_rewritten_lines_get_no_word_emphasis() {
        assert_eq!(changes("same line", "same line"), None);
        assert_eq!(
            changes("fn parse(input: &str)", "struct Cache { entries: Vec<u8> }"),
            None
        );
        assert_eq!(changes("   ", "\t"), None);
    }

    #[test]
    fn long_lines_skip_alignment_but_keep_edits_near_a_shared_head() {
        let line = |first: &str, last: &str| {
            let middle = (0..150)
                .map(|index| format!("w{index}"))
                .collect::<Vec<_>>()
                .join(" ");
            format!("{first} {middle} {last}")
        };
        // Edits at both ends leave a middle too large to align: a rewrite.
        assert_eq!(changes(&line("one", "two"), &line("uno", "dos")), None);
        // A single edit at the end still resolves through the shared head.
        let (old, new) = (line("one", "two"), line("one", "dos"));
        let changes = changes(&old, &new).unwrap();
        assert_eq!(pieces(&old, &changes.before), ["two"]);
        assert_eq!(pieces(&new, &changes.after), ["dos"]);
    }
}
