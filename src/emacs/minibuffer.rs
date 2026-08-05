//! One-line Emacs minibuffer state for `M-x` and feedback prompts.
//!
//! This is pure presentation state.

use crate::app::text_input::{TextInputAction, TextInputState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MinibufferKind {
    ExecuteCommand,
    Feedback,
}

/// A live minibuffer prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinibufferState {
    pub prompt: String,
    pub input: String,
    editing: TextInputState,
    pub kind: MinibufferKind,
}

impl MinibufferState {
    pub fn command() -> Self {
        Self::new("M-x ", MinibufferKind::ExecuteCommand)
    }

    pub fn feedback() -> Self {
        Self::new("Feedback ", MinibufferKind::Feedback)
    }

    fn new(prompt: &str, kind: MinibufferKind) -> Self {
        Self {
            prompt: prompt.to_string(),
            input: String::new(),
            editing: TextInputState::default(),
            kind,
        }
    }

    pub fn cursor(&self) -> usize {
        self.editing.cursor(&self.input)
    }

    pub fn insert_char(&mut self, c: char) {
        self.editing.insert_str(&mut self.input, &c.to_string());
    }

    pub fn insert_str(&mut self, text: &str) {
        self.editing.insert_str(&mut self.input, text);
    }

    pub fn delete_backward_char(&mut self) {
        self.editing
            .apply(&mut self.input, TextInputAction::DeleteBackward, "");
    }

    pub fn delete_forward_char(&mut self) {
        self.editing
            .apply(&mut self.input, TextInputAction::DeleteForward, "");
    }

    pub fn kill_line(&mut self) -> Option<String> {
        self.editing
            .apply(&mut self.input, TextInputAction::KillEnd, "")
    }

    pub fn kill_beginning_of_line(&mut self) -> Option<String> {
        self.editing
            .apply(&mut self.input, TextInputAction::KillBeginning, "")
    }

    pub fn backward_kill_word(&mut self) -> Option<String> {
        self.editing
            .apply(&mut self.input, TextInputAction::KillBackwardWord, "")
    }

    pub fn move_beginning_of_line(&mut self) {
        self.editing
            .apply(&mut self.input, TextInputAction::MoveBeginning, "");
    }

    pub fn move_end_of_line(&mut self) {
        self.editing
            .apply(&mut self.input, TextInputAction::MoveEnd, "");
    }

    pub fn forward_char(&mut self) {
        self.editing
            .apply(&mut self.input, TextInputAction::ForwardChar, "");
    }

    pub fn backward_char(&mut self) {
        self.editing
            .apply(&mut self.input, TextInputAction::BackwardChar, "");
    }

    pub fn forward_word(&mut self) {
        self.editing
            .apply(&mut self.input, TextInputAction::ForwardWord, "");
    }

    pub fn backward_word(&mut self) {
        self.editing
            .apply(&mut self.input, TextInputAction::BackwardWord, "");
    }

    /// Apply one of the shared Herdr-owned text editing actions.
    ///
    /// Keeping this delegation here means M-x and feedback use exactly the
    /// same mode-neutral sexp parser as rename and search fields.
    pub(crate) fn apply_text_action(
        &mut self,
        action: TextInputAction,
        yank: &str,
    ) -> Option<String> {
        self.editing.apply(&mut self.input, action, yank)
    }

    pub fn select_all(&mut self) {
        self.editing
            .apply(&mut self.input, TextInputAction::SelectAll, "");
    }

    pub fn render_line(&self) -> String {
        format!("{}{}", self.prompt, self.input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_is_character_indexed() {
        let mut state = MinibufferState::command();
        state.insert_str("héllo");
        state.backward_char();
        state.delete_backward_char();
        state.insert_char('X');
        assert_eq!(state.input, "hélXo");
        assert_eq!(state.cursor(), 4);
    }

    #[test]
    fn feedback_uses_a_plain_prompt() {
        let state = MinibufferState::feedback();
        assert_eq!(state.render_line(), "Feedback ");
        assert_eq!(state.kind, MinibufferKind::Feedback);
    }

    #[test]
    fn backward_kill_word_peels_command_segments() {
        let mut state = MinibufferState::command();
        state.insert_str("split-window-right");
        state.backward_kill_word();
        assert_eq!(state.input, "split-window-");
        state.backward_kill_word();
        assert_eq!(state.input, "split-");
    }

    #[test]
    fn shared_sexp_actions_edit_feedback_unicode_safely() {
        let mut state = MinibufferState::feedback();
        state.insert_str("(héllo [world]) tail");
        state.move_beginning_of_line();
        state.apply_text_action(TextInputAction::MarkSexp, "");
        let killed = state
            .apply_text_action(TextInputAction::KillSexp, "")
            .expect("marked sexp should be killed");
        assert_eq!(killed, "(héllo [world])");
        assert_eq!(state.input, " tail");
    }
}
