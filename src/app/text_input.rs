//! Pure, single-line editing state shared by Herdr-owned text fields.
//!
//! Terminal applications still own their input. This module is only for
//! Herdr UI prompts such as Navigator search, key help, and rename dialogs.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextInputAction {
    MoveBeginning,
    MoveEnd,
    BackwardChar,
    ForwardChar,
    BackwardWord,
    ForwardWord,
    BackwardSexp,
    ForwardSexp,
    BackwardUpList,
    DownList,
    MarkSexp,
    DeleteBackward,
    DeleteForward,
    KillBeginning,
    KillEnd,
    KillBackwardWord,
    KillSexp,
    UnwrapSelection,
    ShrinkSelection,
    SelectAll,
    Yank,
}

pub(crate) fn action_for_key(key: KeyEvent) -> Option<TextInputAction> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('a'), KeyModifiers::CONTROL) => Some(TextInputAction::MoveBeginning),
        (KeyCode::Char('e'), KeyModifiers::CONTROL) => Some(TextInputAction::MoveEnd),
        (KeyCode::Char('b'), KeyModifiers::CONTROL) => Some(TextInputAction::BackwardChar),
        (KeyCode::Char('f'), KeyModifiers::CONTROL) => Some(TextInputAction::ForwardChar),
        (KeyCode::Char('b'), KeyModifiers::ALT) => Some(TextInputAction::BackwardWord),
        (KeyCode::Char('f'), KeyModifiers::ALT) => Some(TextInputAction::ForwardWord),
        (KeyCode::Char('b'), modifiers)
            if modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT =>
        {
            Some(TextInputAction::BackwardSexp)
        }
        (KeyCode::Char('f'), modifiers)
            if modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT =>
        {
            Some(TextInputAction::ForwardSexp)
        }
        (KeyCode::Char('u'), modifiers)
            if modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT =>
        {
            Some(TextInputAction::BackwardUpList)
        }
        (KeyCode::Char('d'), modifiers)
            if modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT =>
        {
            Some(TextInputAction::DownList)
        }
        (KeyCode::Char(' '), modifiers)
            if modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT =>
        {
            Some(TextInputAction::MarkSexp)
        }
        (KeyCode::Char('@'), modifiers)
            if modifiers.contains(KeyModifiers::CONTROL)
                && modifiers.contains(KeyModifiers::ALT) =>
        {
            Some(TextInputAction::MarkSexp)
        }
        (KeyCode::Backspace, KeyModifiers::NONE) => Some(TextInputAction::DeleteBackward),
        (KeyCode::Char('d'), KeyModifiers::CONTROL) | (KeyCode::Delete, KeyModifiers::NONE) => {
            Some(TextInputAction::DeleteForward)
        }
        (KeyCode::Char('u'), KeyModifiers::CONTROL) => Some(TextInputAction::KillBeginning),
        (KeyCode::Char('k'), KeyModifiers::CONTROL) => Some(TextInputAction::KillEnd),
        (KeyCode::Char('w'), KeyModifiers::CONTROL)
        | (KeyCode::Char('h'), KeyModifiers::CONTROL)
        | (KeyCode::Backspace, KeyModifiers::CONTROL)
        | (KeyCode::Backspace, KeyModifiers::ALT) => Some(TextInputAction::KillBackwardWord),
        (KeyCode::Char('k'), modifiers)
            if modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT =>
        {
            Some(TextInputAction::KillSexp)
        }
        (KeyCode::Backspace, modifiers)
            if modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT =>
        {
            Some(TextInputAction::UnwrapSelection)
        }
        (KeyCode::Char('r'), modifiers)
            if modifiers == KeyModifiers::CONTROL | KeyModifiers::ALT =>
        {
            Some(TextInputAction::ShrinkSelection)
        }
        (KeyCode::Char('h'), KeyModifiers::ALT) => Some(TextInputAction::SelectAll),
        (KeyCode::Char('y'), KeyModifiers::CONTROL) => Some(TextInputAction::Yank),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TextInputState {
    /// Character index, not byte index.
    cursor: usize,
    /// An active selection anchor. `None` means no selection.
    mark: Option<usize>,
    /// Snapshot used to recognize direct AppState string replacement and
    /// place the cursor at the natural end of the replacement.
    observed: String,
}

impl TextInputState {
    pub(crate) fn at_end(text: &str) -> Self {
        Self {
            cursor: text.chars().count(),
            mark: None,
            observed: text.to_string(),
        }
    }

    pub(crate) fn cursor(&self, text: &str) -> usize {
        self.cursor.min(text.chars().count())
    }

    pub(crate) fn with_cursor(&self, text: &str) -> String {
        let at = byte_at(text, self.cursor(text));
        let mut rendered = text.to_string();
        rendered.insert(at, '█');
        rendered
    }

    pub(crate) fn reset(&mut self) {
        self.cursor = 0;
        self.mark = None;
        self.observed.clear();
    }

    pub(crate) fn move_to_end(&mut self, text: &str) {
        self.cursor = text.chars().count();
        self.mark = None;
        self.observed = text.to_string();
    }

    pub(crate) fn insert_str(&mut self, text: &mut String, inserted: &str) {
        self.normalize(text);
        self.delete_selection(text);
        let filtered = inserted
            .chars()
            .filter(|ch| !ch.is_control())
            .collect::<String>();
        let at = byte_at(text, self.cursor);
        text.insert_str(at, &filtered);
        self.cursor += filtered.chars().count();
        self.observed = text.to_string();
    }

    /// Apply an editing action and return any newly killed text.
    pub(crate) fn apply(
        &mut self,
        text: &mut String,
        action: TextInputAction,
        yank: &str,
    ) -> Option<String> {
        self.normalize(text);
        match action {
            TextInputAction::MoveBeginning => self.set_cursor(0),
            TextInputAction::MoveEnd => self.set_cursor(text.chars().count()),
            TextInputAction::BackwardChar => self.set_cursor(self.cursor.saturating_sub(1)),
            TextInputAction::ForwardChar => {
                self.set_cursor((self.cursor + 1).min(text.chars().count()))
            }
            TextInputAction::BackwardWord => {
                let chars = text.chars().collect::<Vec<_>>();
                self.set_cursor(backward_word_start(&chars, self.cursor));
            }
            TextInputAction::ForwardWord => {
                let chars = text.chars().collect::<Vec<_>>();
                self.set_cursor(forward_word_end(&chars, self.cursor));
            }
            TextInputAction::BackwardSexp => {
                let chars = text.chars().collect::<Vec<_>>();
                self.set_cursor(backward_sexp_start(&chars, self.cursor));
            }
            TextInputAction::ForwardSexp => {
                let chars = text.chars().collect::<Vec<_>>();
                self.set_cursor(forward_sexp_end(&chars, self.cursor));
            }
            TextInputAction::BackwardUpList => {
                let chars = text.chars().collect::<Vec<_>>();
                self.set_cursor(backward_up_list_start(&chars, self.cursor));
            }
            TextInputAction::DownList => {
                let chars = text.chars().collect::<Vec<_>>();
                self.set_cursor(down_list_start(&chars, self.cursor));
            }
            TextInputAction::MarkSexp => {
                let chars = text.chars().collect::<Vec<_>>();
                let end = forward_sexp_end(&chars, self.cursor);
                self.mark = (end != self.cursor).then_some(end);
            }
            TextInputAction::DeleteBackward => {
                if !self.delete_selection(text) && self.cursor > 0 {
                    self.delete_range(text, self.cursor - 1, self.cursor);
                }
            }
            TextInputAction::DeleteForward => {
                if !self.delete_selection(text) && self.cursor < text.chars().count() {
                    self.delete_range(text, self.cursor, self.cursor + 1);
                }
            }
            TextInputAction::KillBeginning => {
                let killed = self.take_range(text, 0, self.cursor);
                return (!killed.is_empty()).then_some(killed);
            }
            TextInputAction::KillEnd => {
                let killed = if let Some((start, end)) = self.selection_range(text) {
                    self.take_range(text, start, end)
                } else {
                    self.take_range(text, self.cursor, text.chars().count())
                };
                return (!killed.is_empty()).then_some(killed);
            }
            TextInputAction::KillBackwardWord => {
                let killed = if let Some((start, end)) = self.selection_range(text) {
                    self.take_range(text, start, end)
                } else {
                    let chars = text.chars().collect::<Vec<_>>();
                    let start = backward_word_start(&chars, self.cursor);
                    self.take_range(text, start, self.cursor)
                };
                return (!killed.is_empty()).then_some(killed);
            }
            TextInputAction::KillSexp => {
                let killed = if let Some((start, end)) = self.selection_range(text) {
                    self.take_range(text, start, end)
                } else {
                    let chars = text.chars().collect::<Vec<_>>();
                    let end = forward_sexp_end(&chars, self.cursor);
                    self.take_range(text, self.cursor, end)
                };
                return (!killed.is_empty()).then_some(killed);
            }
            TextInputAction::UnwrapSelection => {
                if let Some((start, end)) = self.selection_range(text) {
                    if end.saturating_sub(start) >= 2 {
                        self.delete_range(text, end - 1, end);
                        self.delete_range(text, start, start + 1);
                        self.mark = (end > start + 2).then_some(end - 2);
                        self.observed = text.to_string();
                    }
                }
            }
            TextInputAction::ShrinkSelection => {
                if let Some((start, end)) = self.selection_range(text) {
                    if end.saturating_sub(start) >= 2 {
                        let point_was_at_start = self.cursor == start;
                        let inner_start = start + 1;
                        let inner_end = end - 1;
                        if point_was_at_start {
                            self.cursor = inner_start;
                            self.mark = Some(inner_end);
                        } else {
                            self.cursor = inner_end;
                            self.mark = Some(inner_start);
                        }
                    }
                }
            }
            TextInputAction::SelectAll => {
                self.mark = Some(0);
                self.cursor = text.chars().count();
            }
            TextInputAction::Yank => self.insert_str(text, yank),
        }
        None
    }

    fn normalize(&mut self, text: &str) {
        if self.observed != text {
            self.cursor = text.chars().count();
            self.mark = None;
            self.observed = text.to_string();
            return;
        }
        let len = text.chars().count();
        self.cursor = self.cursor.min(len);
        self.mark = self.mark.map(|mark| mark.min(len));
    }

    fn set_cursor(&mut self, cursor: usize) {
        self.cursor = cursor;
    }

    fn selection_range(&self, text: &str) -> Option<(usize, usize)> {
        let mark = self.mark?.min(text.chars().count());
        (mark != self.cursor).then_some((mark.min(self.cursor), mark.max(self.cursor)))
    }

    fn delete_selection(&mut self, text: &mut String) -> bool {
        let Some((start, end)) = self.selection_range(text) else {
            self.mark = None;
            return false;
        };
        self.delete_range(text, start, end);
        true
    }

    fn delete_range(&mut self, text: &mut String, start: usize, end: usize) {
        let from = byte_at(text, start);
        let to = byte_at(text, end);
        text.replace_range(from..to, "");
        self.cursor = start;
        self.mark = None;
        self.observed = text.to_string();
    }

    fn take_range(&mut self, text: &mut String, start: usize, end: usize) -> String {
        let from = byte_at(text, start);
        let to = byte_at(text, end);
        let killed = text[from..to].to_string();
        self.delete_range(text, start, end);
        killed
    }
}

fn byte_at(text: &str, idx: usize) -> usize {
    text.char_indices()
        .nth(idx)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn backward_word_start(chars: &[char], cursor: usize) -> usize {
    let mut idx = cursor.min(chars.len());
    while idx > 0 && !is_word(chars[idx - 1]) {
        idx -= 1;
    }
    while idx > 0 && is_word(chars[idx - 1]) {
        idx -= 1;
    }
    idx
}

fn forward_word_end(chars: &[char], cursor: usize) -> usize {
    let mut idx = cursor.min(chars.len());
    while idx < chars.len() && !is_word(chars[idx]) {
        idx += 1;
    }
    while idx < chars.len() && is_word(chars[idx]) {
        idx += 1;
    }
    idx
}

#[derive(Debug)]
struct SexpSyntax {
    pairs: Vec<Option<usize>>,
    syntax: Vec<bool>,
    structural_delimiter: Vec<bool>,
    string_bounds: Vec<Option<(usize, usize)>>,
}

impl SexpSyntax {
    fn parse(chars: &[char]) -> Self {
        let mut pairs = vec![None; chars.len()];
        let mut syntax = vec![false; chars.len()];
        let mut structural_delimiter = vec![false; chars.len()];
        let mut string_bounds = vec![None; chars.len()];
        let mut delimiters = Vec::<(char, usize)>::new();
        let mut string: Option<(char, usize, bool)> = None;

        for (idx, &ch) in chars.iter().enumerate() {
            if let Some((quote, start, escaped)) = string {
                syntax[idx] = true;
                if escaped {
                    string = Some((quote, start, false));
                } else if ch == '\\' {
                    string = Some((quote, start, true));
                } else if ch == quote {
                    pairs[start] = Some(idx);
                    pairs[idx] = Some(start);
                    for bound in &mut string_bounds[start..=idx] {
                        *bound = Some((start, idx + 1));
                    }
                    string = None;
                }
                continue;
            }

            if is_quote_start(chars, idx) {
                syntax[idx] = true;
                string = Some((ch, idx, false));
                continue;
            }

            if is_opening_delimiter(ch) {
                syntax[idx] = true;
                structural_delimiter[idx] = true;
                delimiters.push((ch, idx));
            } else if is_closing_delimiter(ch) {
                syntax[idx] = true;
                structural_delimiter[idx] = true;
                if let Some(&(open, start)) = delimiters.last() {
                    if delimiters_match(open, ch) {
                        delimiters.pop();
                        pairs[start] = Some(idx);
                        pairs[idx] = Some(start);
                    }
                }
            }
        }

        if let Some((_, start, _)) = string {
            for bound in &mut string_bounds[start..] {
                *bound = Some((start, chars.len()));
            }
        }

        Self {
            pairs,
            syntax,
            structural_delimiter,
            string_bounds,
        }
    }
}

fn is_quote_start(chars: &[char], idx: usize) -> bool {
    match chars[idx] {
        '"' | '`' => true,
        // Treat apostrophes as quotes at token boundaries, but not in words
        // such as "don't". This is a useful mode-neutral approximation for
        // Herdr prompts, which do not have a programming-language syntax table.
        '\'' => {
            let at_boundary = idx == 0 || !is_word(chars[idx - 1]);
            at_boundary
                && chars[idx + 1..]
                    .iter()
                    .enumerate()
                    .any(|(offset, ch)| *ch == '\'' && !is_escaped(chars, idx + 1 + offset))
        }
        _ => false,
    }
}

fn is_escaped(chars: &[char], idx: usize) -> bool {
    let mut slashes = 0;
    let mut before = idx;
    while before > 0 && chars[before - 1] == '\\' {
        slashes += 1;
        before -= 1;
    }
    slashes % 2 == 1
}

fn is_opening_delimiter(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{')
}

fn is_closing_delimiter(ch: char) -> bool {
    matches!(ch, ')' | ']' | '}')
}

fn delimiters_match(open: char, close: char) -> bool {
    matches!((open, close), ('(', ')') | ('[', ']') | ('{', '}'))
}

fn forward_sexp_end(chars: &[char], cursor: usize) -> usize {
    let syntax = SexpSyntax::parse(chars);
    let mut idx = cursor.min(chars.len());
    while idx < chars.len() && chars[idx].is_whitespace() {
        idx += 1;
    }
    if idx == chars.len() {
        return idx;
    }

    if let Some((_, end)) = syntax.string_bounds[idx] {
        return end;
    }

    if let Some(pair) = syntax.pairs[idx] {
        if (is_opening_delimiter(chars[idx]) || matches!(chars[idx], '"' | '\'' | '`'))
            && pair > idx
        {
            return pair + 1;
        }
    }
    if is_opening_delimiter(chars[idx]) {
        // An unfinished prompt is common; moving to its end is more useful
        // than making the field appear unresponsive as Emacs' scan error does.
        return chars.len();
    }
    if is_closing_delimiter(chars[idx]) {
        return idx + 1;
    }

    idx += 1;
    while idx < chars.len() && !chars[idx].is_whitespace() && !syntax.syntax[idx] {
        idx += 1;
    }
    idx
}

fn backward_sexp_start(chars: &[char], cursor: usize) -> usize {
    let syntax = SexpSyntax::parse(chars);
    let mut idx = cursor.min(chars.len());
    while idx > 0 && chars[idx - 1].is_whitespace() {
        idx -= 1;
    }
    if idx == 0 {
        return 0;
    }

    let previous = idx - 1;
    if let Some((start, _)) = syntax.string_bounds[previous] {
        return start;
    }
    if let Some(pair) = syntax.pairs[previous] {
        if pair < previous {
            return pair;
        }
    }
    if is_closing_delimiter(chars[previous]) {
        return 0;
    }
    if is_opening_delimiter(chars[previous]) {
        return previous;
    }

    idx = previous;
    while idx > 0 && !chars[idx - 1].is_whitespace() && !syntax.syntax[idx - 1] {
        idx -= 1;
    }
    idx
}

fn backward_up_list_start(chars: &[char], cursor: usize) -> usize {
    let syntax = SexpSyntax::parse(chars);
    let cursor = cursor.min(chars.len());
    (0..cursor)
        .rev()
        .find(|&idx| {
            is_opening_delimiter(chars[idx])
                && syntax.structural_delimiter[idx]
                && syntax.pairs[idx].is_none_or(|close| close >= cursor)
        })
        .unwrap_or(cursor)
}

fn down_list_start(chars: &[char], cursor: usize) -> usize {
    let syntax = SexpSyntax::parse(chars);
    let cursor = cursor.min(chars.len());
    (cursor..chars.len())
        .find(|&idx| {
            is_opening_delimiter(chars[idx])
                && syntax.structural_delimiter[idx]
                && syntax.pairs[idx].is_none_or(|close| close >= cursor)
        })
        .map_or(cursor, |open| open + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_indexed_editing_and_word_motion_are_unicode_safe() {
        let mut text = "héllo world".to_string();
        let mut state = TextInputState::at_end(&text);

        state.apply(&mut text, TextInputAction::BackwardWord, "");
        state.apply(&mut text, TextInputAction::DeleteBackward, "");
        state.insert_str(&mut text, "—");

        assert_eq!(text, "héllo—world");
        assert_eq!(state.cursor(&text), 6);
    }

    #[test]
    fn select_all_replaces_on_insert_and_kills_can_be_yanked() {
        let mut text = "old name".to_string();
        let mut state = TextInputState::at_end(&text);
        state.apply(&mut text, TextInputAction::SelectAll, "");
        state.insert_str(&mut text, "new");
        assert_eq!(text, "new");

        let killed = state
            .apply(&mut text, TextInputAction::KillBeginning, "")
            .expect("kill should return text");
        assert_eq!(killed, "new");
        state.apply(&mut text, TextInputAction::Yank, &killed);
        assert_eq!(text, "new");
    }

    #[test]
    fn emacs_sexp_keys_map_without_claiming_reserved_list_navigation() {
        let control_meta = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Char('f'), control_meta)),
            Some(TextInputAction::ForwardSexp)
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Char('b'), control_meta)),
            Some(TextInputAction::BackwardSexp)
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Char(' '), control_meta)),
            Some(TextInputAction::MarkSexp)
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Char('k'), control_meta)),
            Some(TextInputAction::KillSexp)
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Char('u'), control_meta)),
            Some(TextInputAction::BackwardUpList)
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Char('d'), control_meta)),
            Some(TextInputAction::DownList)
        );
        // Herdr reserves these globally for previous/next workspace.
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Char('n'), control_meta)),
            None
        );
        assert_eq!(
            action_for_key(KeyEvent::new(KeyCode::Char('p'), control_meta)),
            None
        );
    }

    #[test]
    fn sexp_motion_understands_balanced_nesting_strings_and_escapes() {
        let text = "call(α, [\"not ) \\\" yet\", {β: `x]`}]) tail";
        let chars = text.chars().collect::<Vec<_>>();

        assert_eq!(forward_sexp_end(&chars, 0), 4); // call
        let open = text
            .chars()
            .position(|ch| ch == '(')
            .expect("opening paren");
        let close = chars
            .iter()
            .rposition(|ch| *ch == ')')
            .expect("closing paren");
        assert_eq!(forward_sexp_end(&chars, open), close + 1);
        assert_eq!(backward_sexp_start(&chars, close + 1), open);
        assert_eq!(forward_sexp_end(&chars, close + 1), text.chars().count());
    }

    #[test]
    fn apostrophes_inside_words_are_atoms_not_unclosed_strings() {
        let text = "don't stop 'quoted value' now";
        let chars = text.chars().collect::<Vec<_>>();
        assert_eq!(forward_sexp_end(&chars, 0), 5);

        let quoted = text
            .char_indices()
            .find_map(|(byte, _)| text[byte..].starts_with("'quoted").then_some(byte))
            .map(|byte| text[..byte].chars().count())
            .expect("opening quote");
        assert_eq!(
            chars[quoted..forward_sexp_end(&chars, quoted)]
                .iter()
                .collect::<String>(),
            "'quoted value'"
        );
    }

    #[test]
    fn mark_sexp_selects_a_nested_expression_and_kill_uses_the_region() {
        let mut text = "(α [β]) tail".to_string();
        let mut state = TextInputState::at_end(&text);
        state.apply(&mut text, TextInputAction::MoveBeginning, "");
        state.apply(&mut text, TextInputAction::MarkSexp, "");

        assert_eq!(state.cursor(&text), 0);
        assert_eq!(state.selection_range(&text), Some((0, 7)));
        let killed = state
            .apply(&mut text, TextInputAction::KillSexp, "")
            .expect("selected sexp should be killed");
        assert_eq!(killed, "(α [β])");
        assert_eq!(text, " tail");
    }

    #[test]
    fn ordinary_motion_extends_an_active_mark_like_emacs() {
        let mut text = "one two".to_string();
        let mut state = TextInputState::at_end(&text);
        state.apply(&mut text, TextInputAction::SelectAll, "");
        assert_eq!(state.selection_range(&text), Some((0, 7)));

        state.apply(&mut text, TextInputAction::BackwardSexp, "");
        assert_eq!(state.selection_range(&text), Some((0, 4)));
    }

    #[test]
    fn personal_region_commands_shrink_and_unwrap_unicode_safely() {
        let mut text = "(héllo)".to_string();
        let mut state = TextInputState::at_end(&text);
        state.apply(&mut text, TextInputAction::MoveBeginning, "");
        state.apply(&mut text, TextInputAction::MarkSexp, "");
        state.apply(&mut text, TextInputAction::ShrinkSelection, "");
        assert_eq!(state.selection_range(&text), Some((1, 6)));

        // Re-mark the complete expression before applying Paul's delimiter
        // removal command, which intentionally removes the region's ends.
        state.apply(&mut text, TextInputAction::SelectAll, "");
        state.apply(&mut text, TextInputAction::UnwrapSelection, "");
        assert_eq!(text, "héllo");
        assert_eq!(state.selection_range(&text), Some((0, 5)));
    }

    #[test]
    fn list_motion_enters_and_leaves_the_innermost_containing_list() {
        let mut text = "before (outer [inner]) after".to_string();
        let mut state = TextInputState::at_end(&text);
        state.apply(&mut text, TextInputAction::MoveBeginning, "");
        state.apply(&mut text, TextInputAction::DownList, "");
        let outer_inside = "before (".chars().count();
        assert_eq!(state.cursor(&text), outer_inside);

        state.apply(&mut text, TextInputAction::DownList, "");
        let inner_inside = "before (outer [".chars().count();
        assert_eq!(state.cursor(&text), inner_inside);
        state.apply(&mut text, TextInputAction::BackwardUpList, "");
        assert_eq!(state.cursor(&text), inner_inside - 1);
        state.apply(&mut text, TextInputAction::BackwardUpList, "");
        assert_eq!(state.cursor(&text), outer_inside - 1);
    }

    #[test]
    fn list_motion_ignores_delimiters_inside_strings() {
        let text = "prefix \"not (a list)\" then [real]";
        let chars = text.chars().collect::<Vec<_>>();
        let real_open = chars.iter().position(|ch| *ch == '[').expect("real list");
        assert_eq!(down_list_start(&chars, 0), real_open + 1);

        let quoted_open = chars
            .iter()
            .position(|ch| *ch == '(')
            .expect("quoted paren");
        assert_eq!(
            backward_up_list_start(&chars, quoted_open + 2),
            quoted_open + 2,
            "a delimiter in a string is not a containing list"
        );
    }

    #[test]
    fn sexp_motion_from_inside_a_string_moves_across_the_whole_string() {
        let text = "before \"héllo \\\"world\\\"\" after";
        let chars = text.chars().collect::<Vec<_>>();
        let start = chars.iter().position(|ch| *ch == '"').expect("start quote");
        let end = chars.iter().rposition(|ch| *ch == '"').expect("end quote") + 1;
        let inside = start + 3;
        assert_eq!(forward_sexp_end(&chars, inside), end);
        assert_eq!(backward_sexp_start(&chars, inside), start);
    }

    #[test]
    fn unfinished_delimiter_motion_remains_useful() {
        let text = "run(α, [β";
        let chars = text.chars().collect::<Vec<_>>();
        let open = text
            .chars()
            .position(|ch| ch == '(')
            .expect("opening paren");
        assert_eq!(forward_sexp_end(&chars, open), chars.len());
        assert_eq!(backward_up_list_start(&chars, chars.len()), 7);
    }
}
