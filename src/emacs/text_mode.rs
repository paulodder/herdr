//! TEXT mode: point/mark over a frozen, read-only view of pane scrollback.
//!
//! All coordinates are absolute scrollback positions: `row` counts from the
//! top of the scrollback (same addressing as `Selection::line_range` and
//! ghostty's `read_text_screen`), `col` is a character index in the row.
//! Known limitation (accepted): one char == one grid column, so wide/CJK
//! cells drift — the same class of limitation as upstream copy-mode math.

/// Absolute position in a pane's scrollback grid. Ordered row-major, so
/// `Ord` gives region ordering for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub row: u32,
    pub col: u16,
}

/// Read-only view of the frozen buffer. Implemented over a real
/// `TerminalRuntime` by the App adapter and over `Vec<String>` in tests.
pub trait TextBuffer {
    fn total_rows(&self) -> u32;
    /// Row content with trailing whitespace trimmed; empty for
    /// out-of-range rows.
    fn line(&self, row: u32) -> String;
}

fn line_len(buf: &dyn TextBuffer, row: u32) -> u16 {
    buf.line(row).chars().count().min(u16::MAX as usize) as u16
}

fn last_row(buf: &dyn TextBuffer) -> u32 {
    buf.total_rows().saturating_sub(1)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Clamp a position into the buffer (col may sit at end-of-line, one past
/// the last character — Emacs point semantics).
pub fn clamp(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let row = pos.row.min(last_row(buf));
    Pos {
        row,
        col: pos.col.min(line_len(buf, row)),
    }
}

pub fn forward_char(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let pos = clamp(buf, pos);
    if pos.col < line_len(buf, pos.row) {
        Pos {
            row: pos.row,
            col: pos.col + 1,
        }
    } else if pos.row < last_row(buf) {
        Pos {
            row: pos.row + 1,
            col: 0,
        }
    } else {
        pos
    }
}

pub fn backward_char(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let pos = clamp(buf, pos);
    if pos.col > 0 {
        Pos {
            row: pos.row,
            col: pos.col - 1,
        }
    } else if pos.row > 0 {
        let row = pos.row - 1;
        Pos {
            row,
            col: line_len(buf, row),
        }
    } else {
        pos
    }
}

pub fn next_line(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let pos = clamp(buf, pos);
    if pos.row < last_row(buf) {
        clamp(
            buf,
            Pos {
                row: pos.row + 1,
                col: pos.col,
            },
        )
    } else {
        pos
    }
}

pub fn previous_line(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let pos = clamp(buf, pos);
    if pos.row > 0 {
        clamp(
            buf,
            Pos {
                row: pos.row - 1,
                col: pos.col,
            },
        )
    } else {
        pos
    }
}

pub fn line_beginning(pos: Pos) -> Pos {
    Pos {
        row: pos.row,
        col: 0,
    }
}

pub fn line_end(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let pos = clamp(buf, pos);
    Pos {
        row: pos.row,
        col: line_len(buf, pos.row),
    }
}

pub fn buffer_beginning() -> Pos {
    Pos { row: 0, col: 0 }
}

pub fn buffer_end(buf: &dyn TextBuffer) -> Pos {
    let row = last_row(buf);
    Pos {
        row,
        col: line_len(buf, row),
    }
}

fn char_at(buf: &dyn TextBuffer, pos: Pos) -> Option<char> {
    buf.line(pos.row).chars().nth(usize::from(pos.col))
}

/// Emacs `forward-word`: move past any non-word chars, then to the end of
/// the next word, crossing line boundaries.
pub fn forward_word(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let mut pos = clamp(buf, pos);
    let end = buffer_end(buf);
    // skip separators
    while pos < end && !char_at(buf, pos).is_some_and(is_word_char) {
        pos = forward_char(buf, pos);
    }
    // skip the word
    while pos < end && char_at(buf, pos).is_some_and(is_word_char) {
        pos = forward_char(buf, pos);
    }
    pos
}

/// Emacs `backward-word`: move back over any non-word chars, then to the
/// start of the previous word, crossing line boundaries.
pub fn backward_word(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let mut pos = clamp(buf, pos);
    let start = buffer_beginning();
    while pos > start {
        let prev = backward_char(buf, pos);
        if char_at(buf, prev).is_some_and(is_word_char) {
            break;
        }
        pos = prev;
    }
    while pos > start {
        let prev = backward_char(buf, pos);
        if !char_at(buf, prev).is_some_and(is_word_char) {
            break;
        }
        pos = prev;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 0: "alpha one"
    /// 1: ""
    /// 2: "  bravo_2 charlie"
    /// 3: "delta"
    fn buf() -> FakeBuffer {
        FakeBuffer(vec![
            "alpha one".into(),
            "".into(),
            "  bravo_2 charlie".into(),
            "delta".into(),
        ])
    }

    struct FakeBuffer(Vec<String>);

    impl TextBuffer for FakeBuffer {
        fn total_rows(&self) -> u32 {
            self.0.len() as u32
        }
        fn line(&self, row: u32) -> String {
            self.0.get(row as usize).cloned().unwrap_or_default()
        }
    }

    fn p(row: u32, col: u16) -> Pos {
        Pos { row, col }
    }

    #[test]
    fn char_motions_wrap_lines_and_stop_at_buffer_ends() {
        let b = buf();
        assert_eq!(forward_char(&b, p(0, 0)), p(0, 1));
        // point may sit at end-of-line (col == line length)
        assert_eq!(forward_char(&b, p(0, 8)), p(0, 9));
        assert_eq!(forward_char(&b, p(0, 9)), p(1, 0));
        assert_eq!(forward_char(&b, p(1, 0)), p(2, 0));
        assert_eq!(forward_char(&b, p(3, 5)), p(3, 5), "buffer end sticks");
        assert_eq!(backward_char(&b, p(0, 0)), p(0, 0), "buffer start sticks");
        assert_eq!(backward_char(&b, p(1, 0)), p(0, 9));
        assert_eq!(backward_char(&b, p(2, 1)), p(2, 0));
    }

    #[test]
    fn line_motions_clamp_to_line_length() {
        let b = buf();
        assert_eq!(next_line(&b, p(0, 7)), p(1, 0), "clamped to empty line");
        assert_eq!(next_line(&b, p(2, 17)), p(3, 5));
        assert_eq!(next_line(&b, p(3, 2)), p(3, 2), "last line sticks");
        assert_eq!(previous_line(&b, p(2, 10)), p(1, 0));
        assert_eq!(previous_line(&b, p(0, 4)), p(0, 4), "first line sticks");
        assert_eq!(line_beginning(p(2, 9)), p(2, 0));
        assert_eq!(line_end(&b, p(2, 0)), p(2, 17));
    }

    #[test]
    fn word_motions_cross_lines() {
        let b = buf();
        // M-f from start of "alpha one" -> end of "alpha"
        assert_eq!(forward_word(&b, p(0, 0)), p(0, 5));
        assert_eq!(forward_word(&b, p(0, 5)), p(0, 9)); // end of "one"
                                                        // crossing the empty line to "bravo_2" ('_' is a word char)
        assert_eq!(forward_word(&b, p(0, 9)), p(2, 9));
        assert_eq!(backward_word(&b, p(2, 9)), p(2, 2));
        assert_eq!(backward_word(&b, p(2, 2)), p(0, 6)); // back to start of "one"
        assert_eq!(forward_word(&b, p(3, 5)), p(3, 5), "buffer end sticks");
    }

    #[test]
    fn buffer_end_and_clamp() {
        let b = buf();
        assert_eq!(buffer_end(&b), p(3, 5));
        assert_eq!(clamp(&b, p(99, 99)), p(3, 5));
        assert_eq!(clamp(&b, p(2, 99)), p(2, 17));
        assert_eq!(clamp(&b, p(0, 3)), p(0, 3));
    }
}
