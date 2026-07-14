//! Kill ring and mark ring (spec: depth 60 / 16, config-overridable).

use std::collections::VecDeque;

/// Emacs kill ring: front = most recent kill.
#[derive(Debug, Clone)]
pub struct KillRing {
    entries: VecDeque<String>,
    max: usize,
    /// Rotation cursor for `yank-pop`; 0 = head.
    yank_index: usize,
}

impl KillRing {
    pub fn new(max: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max: max.max(1),
            yank_index: 0,
        }
    }

    pub fn set_max(&mut self, max: usize) {
        self.max = max.max(1);
        self.entries.truncate(self.max);
        self.yank_index = 0;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn head(&self) -> Option<&str> {
        self.entries.front().map(String::as_str)
    }

    /// `kill-new`: push as the newest kill. Empty strings and consecutive
    /// duplicates are ignored (Emacs `kill-do-not-save-duplicates` spirit).
    pub fn push(&mut self, text: String) {
        self.yank_index = 0;
        if text.is_empty() || self.head() == Some(text.as_str()) {
            return;
        }
        self.entries.push_front(text);
        self.entries.truncate(self.max);
    }

    /// `yank`: newest entry; resets the rotation cursor.
    pub fn yank(&mut self) -> Option<String> {
        self.yank_index = 0;
        self.entries.front().cloned()
    }

    /// `yank-pop` (M-y after a yank): next-older entry, wrapping.
    pub fn yank_pop(&mut self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        self.yank_index = (self.yank_index + 1) % self.entries.len();
        self.entries.get(self.yank_index).cloned()
    }

    /// Interprogram paste: adopt the system clipboard as the newest kill
    /// when it holds something new (bidirectional clipboard sync, read side).
    pub fn sync_from_system(&mut self, clipboard: Option<String>) {
        if let Some(clip) = clipboard {
            if !clip.is_empty() && self.head() != Some(clip.as_str()) {
                self.push(clip);
            }
        }
    }
}

/// Per-pane mark ring: front = most recent mark, `(row, col)` absolute.
#[derive(Debug, Clone)]
pub struct MarkRing {
    entries: VecDeque<(u32, u16)>,
    max: usize,
}

impl MarkRing {
    pub fn new(max: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max: max.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn push(&mut self, mark: (u32, u16)) {
        self.entries.push_front(mark);
        self.entries.truncate(self.max);
    }

    /// `C-u C-SPC` primitive (wired in Phase 4): pop the most recent mark
    /// and rotate it to the back.
    pub fn pop_rotate(&mut self) -> Option<(u32, u16)> {
        let front = self.entries.pop_front()?;
        self.entries.push_back(front);
        Some(front)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_ring_pushes_dedupes_and_truncates() {
        let mut ring = KillRing::new(3);
        assert!(ring.is_empty());
        assert_eq!(ring.head(), None);
        ring.push("one".into());
        ring.push("one".into()); // consecutive duplicate ignored
        ring.push(String::new()); // empty ignored
        assert_eq!(ring.len(), 1);
        ring.push("two".into());
        ring.push("three".into());
        ring.push("four".into()); // truncates to max 3
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.head(), Some("four"));
    }

    #[test]
    fn yank_and_yank_pop_rotate_the_ring() {
        let mut ring = KillRing::new(60);
        ring.push("a".into());
        ring.push("b".into());
        ring.push("c".into());
        assert_eq!(ring.yank().as_deref(), Some("c"));
        assert_eq!(ring.yank_pop().as_deref(), Some("b"));
        assert_eq!(ring.yank_pop().as_deref(), Some("a"));
        assert_eq!(ring.yank_pop().as_deref(), Some("c"), "wraps");
        // a fresh kill resets rotation
        ring.push("d".into());
        assert_eq!(ring.yank().as_deref(), Some("d"));
    }

    #[test]
    fn sync_from_system_adopts_fresh_clipboard_text() {
        let mut ring = KillRing::new(60);
        ring.push("kill".into());
        ring.sync_from_system(None);
        ring.sync_from_system(Some(String::new()));
        assert_eq!(ring.head(), Some("kill"));
        ring.sync_from_system(Some("kill".into())); // same as head: no-op
        assert_eq!(ring.len(), 1);
        ring.sync_from_system(Some("clip".into()));
        assert_eq!(ring.head(), Some("clip"));
        assert_eq!(ring.len(), 2);
    }

    #[test]
    fn set_max_shrinks_the_ring() {
        let mut ring = KillRing::new(10);
        for i in 0..5 {
            ring.push(format!("k{i}"));
        }
        ring.set_max(2);
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.head(), Some("k4"));
    }

    #[test]
    fn mark_ring_pushes_truncates_and_rotates() {
        let mut ring = MarkRing::new(2);
        ring.push((1, 0));
        ring.push((2, 3));
        ring.push((5, 1)); // truncates oldest
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.pop_rotate(), Some((5, 1)));
        assert_eq!(ring.pop_rotate(), Some((2, 3)));
        assert_eq!(ring.pop_rotate(), Some((5, 1)), "rotates, Emacs-style");
        assert!(MarkRing::new(4).pop_rotate().is_none());
    }
}
