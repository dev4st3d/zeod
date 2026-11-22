//! Pure link detection for visible terminal rows (OSC 8 hyperlinks, URLs, and project paths).
//!
//! Hover probing stays strictly in memory; filesystem confinement checks run
//! on a background worker only for candidate file links.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use super::terminal_engine::TerminalCell;

const MAX_LINK_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileCandidate {
    pub(crate) path: String,
    pub(crate) line: Option<u32>,
    pub(crate) column: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkTarget {
    Url(String),
    File(FileCandidate),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalLink {
    pub(crate) row: usize,
    pub(crate) cols: Range<usize>,
    pub(crate) target: LinkTarget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResolvedLink {
    Url(String),
    File {
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
}

/// Detect an OSC 8 hyperlink, HTTP(S) URL, or project file path candidate at `(row, col)`.
pub(crate) fn link_at(row: usize, cells: &[TerminalCell], col: usize) -> Option<TerminalLink> {
    if col >= cells.len() {
        return None;
    }
    let anchor = if cells[col].wide_spacer && col > 0 {
        col - 1
    } else {
        col
    };

    // 1. Explicit OSC 8 hyperlinks win over plain-text scanning.
    if let Some(uri) = cells[anchor].hyperlink.as_deref() {
        let mut start = anchor;
        while start > 0 && cells[start - 1].hyperlink.as_deref() == Some(uri) {
            start -= 1;
        }
        let mut end = anchor + 1;
        while end < cells.len() && cells[end].hyperlink.as_deref() == Some(uri) {
            end += 1;
        }
        let target = parse_osc8_uri(uri)?;
        return Some(TerminalLink {
            row,
            cols: start..end,
            target,
        });
    }

    // 2. Reconstruct the visible row with a byte-offset -> column table.
    let mut line = String::with_capacity(cells.len());
    let mut byte_to_col: Vec<usize> = Vec::with_capacity(cells.len());
    for (index, cell) in cells.iter().enumerate() {
        if cell.wide_spacer {
            continue;
        }
        let text = if cell.text.is_empty() {
            " "
        } else {
            cell.text.as_ref()
        };
        let width = if cell.wide { 2 } else { 1 };
        let start_byte = line.len();
        line.push_str(text);
        let added = line.len() - start_byte;
        for _ in 0..added {
            byte_to_col.push(index);
        }
        // Keep trailing cell of a wide character mapped to its leading column during lookup.
        let _ = width;
    }
    if line.is_empty() {
        return None;
    }

    // Find the whitespace-delimited token enclosing `anchor`.
    let (token_start, token_end) = token_bounds_for_col(&line, &byte_to_col, anchor)?;
    let token = &line[token_start..token_end];
    if token.len() > MAX_LINK_BYTES {
        return None;
    }

    // 3. Check for an HTTP(S) URL inside the token.
    if let Some((rel_start, rel_end, url)) = extract_url(token) {
        let cols = byte_range_to_cols(
            &byte_to_col,
            cells,
            token_start + rel_start..token_start + rel_end,
        )?;
        if cols.contains(&anchor) {
            return Some(TerminalLink {
                row,
                cols,
                target: LinkTarget::Url(url),
            });
        }
    }

    // 4. Check for a project file path (with optional `:line[:col]` or `(line[,col])`).
    if let Some((rel_start, rel_end, candidate)) = extract_file_candidate(token) {
        let cols = byte_range_to_cols(
            &byte_to_col,
            cells,
            token_start + rel_start..token_start + rel_end,
        )?;
        if cols.contains(&anchor) {
            return Some(TerminalLink {
                row,
                cols,
                target: LinkTarget::File(candidate),
            });
        }
    }

    None
}

/// Resolve a file candidate against `project` on a background worker, enforcing confinement.
pub(crate) fn resolve_project_file(project: &Path, candidate: &FileCandidate) -> Option<PathBuf> {
    let root = fs::canonicalize(project).ok()?;
    let raw = candidate.path.trim();
    if raw.is_empty() {
        return None;
    }

    let direct = Path::new(raw);
    let mut probes: Vec<PathBuf> = Vec::with_capacity(3);
    if direct.is_absolute() {
        probes.push(direct.to_path_buf());
    } else {
        probes.push(root.join(direct));
        // Git unified diff prefixes (`a/...`, `b/...`).
        if let Some(stripped) = raw
            .strip_prefix("a/")
            .or_else(|| raw.strip_prefix("b/"))
            .or_else(|| raw.strip_prefix(r"a\"))
            .or_else(|| raw.strip_prefix(r"b\"))
        {
            probes.push(root.join(stripped));
        }
    }

    for probe in probes {
        let Ok(canonical) = fs::canonicalize(&probe) else {
            continue;
        };
        if canonical.starts_with(&root) && canonical.is_file() {
            return Some(super::paths::without_windows_verbatim_prefix(&canonical));
        }
    }
    None
}

fn parse_osc8_uri(uri: &str) -> Option<LinkTarget> {
    let uri = uri.trim();
    if uri.is_empty() || uri.len() > MAX_LINK_BYTES || uri.chars().any(char::is_control) {
        return None;
    }
    let lower = uri.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Some(LinkTarget::Url(uri.to_owned()));
    }
    if let Some(rest) = strip_prefix_ignore_ascii_case(uri, "file://") {
        let path = decode_file_uri_path(rest)?;
        return Some(LinkTarget::File(FileCandidate {
            path,
            line: None,
            column: None,
        }));
    }
    None
}

fn strip_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&value[prefix.len()..])
    } else {
        None
    }
}

fn decode_file_uri_path(rest: &str) -> Option<String> {
    // Drop optional `localhost` authority.
    let without_host = strip_prefix_ignore_ascii_case(rest, "localhost").unwrap_or(rest);
    if !without_host.starts_with('/') {
        return None;
    }
    let decoded = percent_decode(without_host)?;
    // `file:///C:/dir/file.rs` -> `C:/dir/file.rs`
    let path = if decoded.len() >= 3
        && decoded.as_bytes()[0] == b'/'
        && decoded.as_bytes()[1].is_ascii_alphabetic()
        && decoded.as_bytes()[2] == b':'
    {
        decoded[1..].to_owned()
    } else {
        decoded
    };
    (!path.is_empty()).then_some(path)
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            let value = u8::from_str_radix(hex, 16).ok()?;
            output.push(value);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn token_bounds_for_col(
    line: &str,
    byte_to_col: &[usize],
    target_col: usize,
) -> Option<(usize, usize)> {
    let byte_index = byte_to_col.iter().position(|&col| col == target_col)?;
    if line[byte_index..].chars().next()?.is_whitespace() {
        return None;
    }
    let mut start = byte_index;
    while start > 0 {
        let prev = line[..start].chars().next_back()?;
        if prev.is_whitespace() {
            break;
        }
        start -= prev.len_utf8();
    }
    let mut end = byte_index;
    while end < line.len() {
        let next = line[end..].chars().next()?;
        if next.is_whitespace() {
            break;
        }
        end += next.len_utf8();
    }
    (start < end).then_some((start, end))
}

fn byte_range_to_cols(
    byte_to_col: &[usize],
    cells: &[TerminalCell],
    bytes: Range<usize>,
) -> Option<Range<usize>> {
    if bytes.is_empty() || bytes.end > byte_to_col.len() {
        return None;
    }
    let start_col = byte_to_col[bytes.start];
    let last_col = byte_to_col[bytes.end - 1];
    let end_col = if cells.get(last_col).is_some_and(|cell| cell.wide) {
        (last_col + 2).min(cells.len())
    } else {
        (last_col + 1).min(cells.len())
    };
    Some(start_col..end_col)
}

fn extract_url(token: &str) -> Option<(usize, usize, String)> {
    let start = find_url_scheme(token)?;
    let mut end = token.len();

    // Stop at markdown/wrapper delimiters that clearly terminate an inline URL.
    if let Some(stop) = token[start..end].find(['"', '\'', '`', '<', '>']) {
        end = start + stop;
    }

    // Trim trailing punctuation and unbalanced closing brackets.
    while end > start {
        let slice = &token[start..end];
        let Some(last) = slice.chars().next_back() else {
            break;
        };
        let trim = match last {
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            ')' => !has_matching_open(slice, '(', ')'),
            ']' => !has_matching_open(slice, '[', ']'),
            '}' => !has_matching_open(slice, '{', '}'),
            _ => false,
        };
        if trim {
            end -= last.len_utf8();
        } else {
            break;
        }
    }

    let url = &token[start..end];
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.starts_with('.') {
        return None;
    }
    Some((start, end, url.to_owned()))
}

fn find_url_scheme(token: &str) -> Option<usize> {
    match (token.find("https://"), token.find("http://")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn has_matching_open(text: &str, open: char, close: char) -> bool {
    let opens = text.chars().filter(|&ch| ch == open).count();
    let closes = text.chars().filter(|&ch| ch == close).count();
    opens >= closes
}

fn extract_file_candidate(token: &str) -> Option<(usize, usize, FileCandidate)> {
    let mut start = 0;
    let mut end = token.len();

    // Peel balanced or leading wrapper punctuation such as `"..."`, `'...'`, `(...)`, `[...]`, `<...>`.
    loop {
        let slice = &token[start..end];
        if slice.is_empty() {
            return None;
        }
        let first = slice.chars().next()?;
        if matches!(first, '"' | '\'' | '`' | '(' | '[' | '<' | '{') {
            start += first.len_utf8();
            continue;
        }
        break;
    }
    loop {
        let slice = &token[start..end];
        if slice.is_empty() {
            return None;
        }
        let last = slice.chars().next_back()?;
        let keep_paren = last == ')' && parse_paren_location(slice).is_some();
        if !keep_paren
            && matches!(
                last,
                '"' | '\'' | '`' | ')' | ']' | '>' | '}' | ',' | ';' | ':'
            )
        {
            end -= last.len_utf8();
            continue;
        }
        break;
    }

    let slice = &token[start..end];
    if slice.is_empty() || slice.contains("://") {
        return None;
    }

    let (path, line, column) = if let Some((path, line, column)) = parse_paren_location(slice) {
        (path, Some(line), column)
    } else {
        parse_colon_location(slice)
    };

    if !looks_like_file_path(path) {
        return None;
    }

    Some((
        start,
        end,
        FileCandidate {
            path: path.to_owned(),
            line,
            column,
        },
    ))
}

/// Parses `path(line)` or `path(line,col)` (MSVC, TypeScript, C#).
fn parse_paren_location(slice: &str) -> Option<(&str, u32, Option<u32>)> {
    let inner = slice.strip_suffix(')')?;
    let open = inner.rfind('(')?;
    let path = &inner[..open];
    if path.is_empty() {
        return None;
    }
    let coords = &inner[open + 1..];
    if let Some((line, col)) = coords.split_once(',') {
        let line = parse_positive_u32(line.trim())?;
        let col = parse_positive_u32(col.trim())?;
        Some((path, line, Some(col)))
    } else {
        let line = parse_positive_u32(coords.trim())?;
        Some((path, line, None))
    }
}

/// Parses `path`, `path:line`, or `path:line:col` while preserving Windows drive letters (`C:\...`).
fn parse_colon_location(slice: &str) -> (&str, Option<u32>, Option<u32>) {
    let drive_prefix = if slice.len() >= 2
        && slice.as_bytes()[0].is_ascii_alphabetic()
        && slice.as_bytes()[1] == b':'
    {
        2
    } else {
        0
    };
    let tail = &slice[drive_prefix..];
    let mut parts = tail.rsplitn(3, ':');
    let first = parts.next().unwrap_or("");
    let second = parts.next();
    let third = parts.next();

    if let (Some(mid), Some(rest)) = (second, third)
        && let (Some(line), Some(col)) = (parse_positive_u32(mid), parse_positive_u32(first))
    {
        let path_end = drive_prefix + rest.len();
        return (&slice[..path_end], Some(line), Some(col));
    }
    if let Some(rest) = second
        && let Some(line) = parse_positive_u32(first)
    {
        let path_end = drive_prefix + rest.len();
        return (&slice[..path_end], Some(line), None);
    }
    (slice, None, None)
}

fn parse_positive_u32(value: &str) -> Option<u32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse::<u32>().ok().filter(|&number| number > 0)
}

fn looks_like_file_path(path: &str) -> bool {
    if path.is_empty()
        || path.len() > 512
        || matches!(path, "." | "..")
        || path.ends_with(['/', '\\'])
        || path
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return false;
    }
    if path.contains(['/', '\\']) {
        return true;
    }
    // Single filename without separator: require an extension (e.g. `Cargo.toml`, `README.md`).
    let Some((stem, ext)) = path.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && !ext.is_empty()
        && ext.len() <= 16
        && ext
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::terminal_engine::TerminalEngine;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "zeod-terminal-links-{}-{}",
                std::process::id(),
                NEXT_TEST.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn row_from(output: &[u8]) -> Vec<TerminalCell> {
        let mut engine = TerminalEngine::new(2, 120);
        engine.feed(output);
        engine.snapshot().rows.into_iter().next().unwrap()
    }

    #[test]
    fn detects_plain_urls_and_strips_surrounding_punctuation() {
        let row = row_from(b"See (https://example.com/a_(b)), now.");
        let found = link_at(0, &row, 10).unwrap();
        assert_eq!(
            found.target,
            LinkTarget::Url("https://example.com/a_(b)".into())
        );
        assert_eq!(found.cols, 5..30);
        assert!(link_at(0, &row, 0).is_none());
        assert!(link_at(0, &row, 31).is_none());
    }

    #[test]
    fn detects_osc8_hyperlinks_and_blocks_unsafe_schemes() {
        let row = row_from(b"click \x1b]8;;https://zed.dev\x07here\x1b]8;;\x07!");
        let found = link_at(0, &row, 7).unwrap();
        assert_eq!(found.target, LinkTarget::Url("https://zed.dev".into()));
        assert_eq!(found.cols, 6..10);

        let unsafe_row = row_from(b"\x1b]8;;javascript:alert(1)\x07bad\x1b]8;;\x07");
        assert!(link_at(0, &unsafe_row, 1).is_none());
    }

    #[test]
    fn detects_rust_windows_and_msvc_file_locations() {
        let row = row_from(b"  --> src/views/terminal.rs:412:15");
        let found = link_at(0, &row, 12).unwrap();
        assert_eq!(
            found.target,
            LinkTarget::File(FileCandidate {
                path: "src/views/terminal.rs".into(),
                line: Some(412),
                column: Some(15),
            })
        );

        let msvc = row_from(b"src\\app.rs(24,8): warning");
        let found = link_at(0, &msvc, 4).unwrap();
        assert_eq!(
            found.target,
            LinkTarget::File(FileCandidate {
                path: r"src\app.rs".into(),
                line: Some(24),
                column: Some(8),
            })
        );
    }

    #[test]
    fn resolves_project_files_and_rejects_paths_outside_project() {
        let root = TestDirectory::new();
        let file = root.0.join("src/main.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "fn main() {}\n").unwrap();

        let outside = TestDirectory::new();
        let escaped = outside.0.join("secret.txt");
        fs::write(&escaped, "nope").unwrap();

        let relative = FileCandidate {
            path: "src/main.rs".into(),
            line: Some(1),
            column: None,
        };
        assert_eq!(
            resolve_project_file(&root.0, &relative),
            Some(crate::services::paths::without_windows_verbatim_prefix(
                &file
            ))
        );

        // Git diff `b/` prefix resolves when `b/src/main.rs` doesn't exist literally.
        let git_prefixed = FileCandidate {
            path: "b/src/main.rs".into(),
            line: None,
            column: None,
        };
        assert_eq!(
            resolve_project_file(&root.0, &git_prefixed),
            Some(crate::services::paths::without_windows_verbatim_prefix(
                &file
            ))
        );

        let escape_candidate = FileCandidate {
            path: escaped.to_string_lossy().into_owned(),
            line: None,
            column: None,
        };
        assert_eq!(resolve_project_file(&root.0, &escape_candidate), None);

        let missing = FileCandidate {
            path: "src/does_not_exist.rs".into(),
            line: None,
            column: None,
        };
        assert_eq!(resolve_project_file(&root.0, &missing), None);
    }
}
