//! Resolve conservative URL and filesystem targets around terminal point.
//!
//! Discovery is intentionally separate from opening policy. Terminal text is
//! untrusted, so URLs are allowlisted and paths must already exist.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenTarget {
    WebUrl(String),
    Path {
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
}

pub fn resolve_selection(selection: &str, cwd: &Path, home: Option<&Path>) -> Option<OpenTarget> {
    let selection = selection.trim();
    if selection.is_empty() || selection.chars().any(char::is_control) {
        return None;
    }
    let mut candidates = Vec::new();
    push_candidate_variants(&mut candidates, selection);
    resolve_candidates(candidates, cwd, home)
}

pub fn resolve_at_point(
    text: &str,
    point_byte: usize,
    cwd: &Path,
    home: Option<&Path>,
) -> Option<OpenTarget> {
    resolve_candidates(candidates_at_point(text, point_byte), cwd, home)
}

fn resolve_candidates(
    candidates: impl IntoIterator<Item = String>,
    cwd: &Path,
    home: Option<&Path>,
) -> Option<OpenTarget> {
    let candidates: Vec<String> = candidates
        .into_iter()
        .filter(|candidate| !candidate.is_empty() && !candidate.chars().any(char::is_control))
        .collect();

    for candidate in &candidates {
        if is_safe_web_url(candidate) {
            return Some(OpenTarget::WebUrl(candidate.clone()));
        }
    }
    for candidate in candidates {
        if let Some(target) = resolve_path_candidate(&candidate, cwd, home) {
            return Some(target);
        }
    }
    None
}

fn candidates_at_point(text: &str, point_byte: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    let point_byte = floor_char_boundary(text, point_byte.min(text.len()));
    let Some(probe) = probe_byte(text, point_byte) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();

    if let Some(candidate) = quoted_candidate(text, probe) {
        push_candidate_variants(&mut candidates, candidate);
    }

    let start = text[..probe]
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace() || *ch == '|')
        .map_or(0, |(idx, ch)| idx + ch.len_utf8());
    let end = text[probe..]
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace() || *ch == '|')
        .map_or(text.len(), |(idx, _)| probe + idx);
    let segment = &text[start..end];

    for scheme in ["https://", "http://"] {
        let mut search_from = 0;
        while let Some(relative) = segment[search_from..].find(scheme) {
            let url_start = search_from + relative;
            let raw = &segment[url_start..];
            let trimmed = trim_trailing_prose(raw);
            let absolute_start = start + url_start;
            let absolute_end = absolute_start + raw.len();
            if probe >= absolute_start && probe < absolute_end {
                push_unique(&mut candidates, trimmed.to_string());
                break;
            }
            search_from = url_start + scheme.len();
        }
    }

    push_candidate_variants(&mut candidates, segment);
    candidates
}

fn probe_byte(text: &str, point_byte: usize) -> Option<usize> {
    if point_byte < text.len() {
        let ch = text[point_byte..].chars().next()?;
        if !ch.is_whitespace() && ch != '|' {
            return Some(point_byte);
        }
    }
    let (previous, ch) = text[..point_byte].char_indices().next_back()?;
    (!ch.is_whitespace() && ch != '|').then_some(previous)
}

fn floor_char_boundary(text: &str, mut idx: usize) -> usize {
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn quoted_candidate(text: &str, probe: usize) -> Option<&str> {
    for quote in ['"', '\'', '`'] {
        let Some(open) = text[..probe].rfind(quote) else {
            continue;
        };
        let Some(close) = text[open + quote.len_utf8()..]
            .find(quote)
            .map(|idx| open + quote.len_utf8() + idx)
        else {
            continue;
        };
        if probe > open && probe < close {
            return text.get(open + quote.len_utf8()..close);
        }
    }
    None
}

fn push_candidate_variants(candidates: &mut Vec<String>, raw: &str) {
    let raw = raw.trim();
    push_unique(candidates, raw.to_string());

    let unwrapped = raw
        .trim_start_matches(['(', '[', '{', '<', '"', '\'', '`'])
        .trim_end_matches([')', ']', '}', '>', '"', '\'', '`']);
    push_unique(candidates, unwrapped.to_string());
    push_unique(candidates, trim_trailing_prose(unwrapped).to_string());

    for separator in ['(', '[', '{', '<', ',', ';', '!'] {
        if let Some((_, suffix)) = unwrapped.rsplit_once(separator) {
            push_unique(candidates, trim_trailing_prose(suffix).to_string());
        }
    }
}

fn push_unique(candidates: &mut Vec<String>, candidate: String) {
    if !candidate.is_empty() && !candidates.contains(&candidate) {
        candidates.push(candidate);
    }
}

fn trim_trailing_prose(raw: &str) -> &str {
    let mut end = raw.len();
    loop {
        let Some(ch) = raw[..end].chars().next_back() else {
            return "";
        };
        let trim = match ch {
            '.' | ',' | ';' | ':' | '!' | '?' | '>' | '"' | '\'' | '`' => true,
            ')' => unmatched_trailing_closer(&raw[..end], '(', ')'),
            ']' => unmatched_trailing_closer(&raw[..end], '[', ']'),
            '}' => unmatched_trailing_closer(&raw[..end], '{', '}'),
            _ => false,
        };
        if !trim {
            return &raw[..end];
        }
        end -= ch.len_utf8();
    }
}

fn unmatched_trailing_closer(raw: &str, open: char, close: char) -> bool {
    let before_last = &raw[..raw.len() - close.len_utf8()];
    before_last.chars().filter(|ch| *ch == open).count()
        <= before_last.chars().filter(|ch| *ch == close).count()
}

fn is_safe_web_url(candidate: &str) -> bool {
    (candidate.starts_with("http://") || candidate.starts_with("https://"))
        && !candidate.chars().any(char::is_control)
}

fn resolve_path_candidate(candidate: &str, cwd: &Path, home: Option<&Path>) -> Option<OpenTarget> {
    if let Some(path) = existing_path(candidate, cwd, home) {
        return Some(OpenTarget::Path {
            path,
            line: None,
            column: None,
        });
    }

    let (raw_path, line, column) = split_location(candidate)?;
    let path = existing_path(raw_path, cwd, home)?;
    Some(OpenTarget::Path {
        path,
        line: Some(line),
        column,
    })
}

fn existing_path(candidate: &str, cwd: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let expanded = if candidate == "~" {
        home?.to_path_buf()
    } else if let Some(rest) = candidate
        .strip_prefix("~/")
        .or_else(|| candidate.strip_prefix("~\\"))
    {
        home?.join(rest)
    } else {
        PathBuf::from(candidate)
    };
    let resolved = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };
    resolved.exists().then_some(resolved)
}

fn split_location(candidate: &str) -> Option<(&str, u32, Option<u32>)> {
    let (before_last, last) = candidate.rsplit_once(':')?;
    let last = positive_u32(last)?;
    if let Some((path, possible_line)) = before_last.rsplit_once(':') {
        if let Some(line) = positive_u32(possible_line) {
            return (!path.is_empty()).then_some((path, line, Some(last)));
        }
    }
    (!before_last.is_empty()).then_some((before_last, last, None))
}

fn positive_u32(raw: &str) -> Option<u32> {
    raw.parse::<u32>().ok().filter(|value| *value > 0)
}

pub fn emacsclient_argv(target: &OpenTarget) -> Vec<String> {
    let mut argv = vec![
        "emacsclient".to_string(),
        "--alternate-editor=".to_string(),
        "--tty".to_string(),
    ];
    match target {
        OpenTarget::WebUrl(url) => {
            argv.push("--eval".to_string());
            argv.push(format!(
                "(progn (require 'eww) (eww {}))",
                elisp_string(url)
            ));
        }
        OpenTarget::Path { path, line, column } => {
            if let Some(line) = line {
                argv.push(match column {
                    Some(column) => format!("+{line}:{column}"),
                    None => format!("+{line}"),
                });
            }
            argv.push("--".to_string());
            argv.push(path.display().to_string());
        }
    }
    argv
}

fn elisp_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "herdr-open-target-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn url_resolves_inside_token_and_at_end() {
        let cwd = Path::new("/");
        let text = "see https://example.test/a(b).";
        let inside = text.find("example").unwrap();
        assert_eq!(
            resolve_at_point(text, inside, cwd, None),
            Some(OpenTarget::WebUrl("https://example.test/a(b)".into()))
        );
        let end = text.len();
        assert_eq!(
            resolve_at_point(text, end, cwd, None),
            Some(OpenTarget::WebUrl("https://example.test/a(b)".into()))
        );
    }

    #[test]
    fn existing_relative_path_and_location_resolve() {
        let cwd = temp_dir("relative");
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        std::fs::write(cwd.join("src/main.rs"), "fn main() {}\n").unwrap();
        assert_eq!(
            resolve_at_point("--> src/main.rs:42:7", 8, &cwd, None),
            Some(OpenTarget::Path {
                path: cwd.join("src/main.rs"),
                line: Some(42),
                column: Some(7),
            })
        );
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[test]
    fn quoted_path_with_spaces_resolves() {
        let cwd = temp_dir("quoted");
        std::fs::write(cwd.join("notes one.md"), "notes\n").unwrap();
        let text = "open \"notes one.md\" now";
        assert_eq!(
            resolve_at_point(text, text.find("one").unwrap(), &cwd, None),
            Some(OpenTarget::Path {
                path: cwd.join("notes one.md"),
                line: None,
                column: None,
            })
        );
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[test]
    fn whitespace_after_token_does_not_resolve() {
        let cwd = temp_dir("space");
        std::fs::write(cwd.join("file.rs"), "x").unwrap();
        assert_eq!(resolve_at_point("file.rs  ", 9, &cwd, None), None);
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[test]
    fn selected_multiline_or_missing_target_is_rejected() {
        let cwd = temp_dir("missing");
        assert_eq!(resolve_selection("one\ntwo", &cwd, None), None);
        assert_eq!(resolve_selection("missing.rs", &cwd, None), None);
        std::fs::remove_dir_all(cwd).unwrap();
    }

    #[test]
    fn location_parser_preserves_windows_drive_colon() {
        assert_eq!(
            split_location(r"C:\\work\\main.rs:42:7"),
            Some((r"C:\\work\\main.rs", 42, Some(7)))
        );
        assert_eq!(
            split_location("src/main.rs:42"),
            Some(("src/main.rs", 42, None))
        );
        assert_eq!(split_location("src/main.rs:0"), None);
    }

    #[test]
    fn emacsclient_argv_is_direct_and_location_aware() {
        let target = OpenTarget::Path {
            path: PathBuf::from("/tmp/a file.rs"),
            line: Some(12),
            column: Some(3),
        };
        assert_eq!(
            emacsclient_argv(&target),
            vec![
                "emacsclient",
                "--alternate-editor=",
                "--tty",
                "+12:3",
                "--",
                "/tmp/a file.rs",
            ]
        );
    }

    #[test]
    fn eww_argv_escapes_elisp_string_content() {
        let argv = emacsclient_argv(&OpenTarget::WebUrl("https://example.test/a?x=\\\"y".into()));
        assert_eq!(argv[3], "--eval");
        assert_eq!(
            argv[4],
            r#"(progn (require 'eww) (eww "https://example.test/a?x=\\\"y"))"#
        );
    }
}
