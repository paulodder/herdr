//! Pure state and match navigation for incremental search in TEXT mode.

use std::collections::VecDeque;

use super::text_mode::Pos;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchSpan {
    pub start: Pos,
    pub end: Pos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchSelection {
    pub index: usize,
    pub wrapped: bool,
}

/// One active incremental-search reader. Terminal matches retain their
/// source fingerprint so rendering can discard coordinates invalidated by
/// concurrent terminal output or scrollback pruning.
#[derive(Debug)]
pub struct IsearchState {
    pub direction: SearchDirection,
    pub query: String,
    pub origin: Pos,
    pub matches: Vec<crate::pane::TerminalTextMatch>,
    pub current: Option<usize>,
    pub failing: bool,
    pub wrapped: bool,
    pub history_cursor: Option<usize>,
    pub history_draft: String,
}

impl IsearchState {
    pub fn new(direction: SearchDirection, origin: Pos) -> Self {
        Self {
            direction,
            query: String::new(),
            origin,
            matches: Vec::new(),
            current: None,
            failing: false,
            wrapped: false,
            history_cursor: None,
            history_draft: String::new(),
        }
    }
}

pub fn initial_selection(
    matches: &[SearchSpan],
    direction: SearchDirection,
    origin: Pos,
) -> Option<SearchSelection> {
    if matches.is_empty() {
        return None;
    }
    let index = match direction {
        SearchDirection::Forward => matches.iter().position(|span| span.start >= origin),
        SearchDirection::Backward => matches.iter().rposition(|span| span.end < origin),
    }?;
    Some(SearchSelection {
        index,
        wrapped: false,
    })
}

pub fn repeated_selection(
    matches: &[SearchSpan],
    direction: SearchDirection,
    current: Option<usize>,
) -> Option<SearchSelection> {
    if matches.is_empty() || current.is_some_and(|current| current >= matches.len()) {
        return None;
    }
    let Some(current) = current else {
        return Some(SearchSelection {
            index: match direction {
                SearchDirection::Forward => 0,
                SearchDirection::Backward => matches.len() - 1,
            },
            wrapped: true,
        });
    };
    Some(match direction {
        SearchDirection::Forward if current + 1 < matches.len() => SearchSelection {
            index: current + 1,
            wrapped: false,
        },
        SearchDirection::Backward if current > 0 => SearchSelection {
            index: current - 1,
            wrapped: false,
        },
        SearchDirection::Forward => SearchSelection {
            index: 0,
            wrapped: true,
        },
        SearchDirection::Backward => SearchSelection {
            index: matches.len() - 1,
            wrapped: true,
        },
    })
}

#[derive(Debug, Clone)]
pub struct SearchRing {
    entries: VecDeque<String>,
    max: usize,
}

impl SearchRing {
    pub fn new(max: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max: max.max(1),
        }
    }

    pub fn push(&mut self, query: String) {
        if query.is_empty() {
            return;
        }
        if let Some(index) = self.entries.iter().position(|entry| entry == &query) {
            self.entries.remove(index);
        }
        self.entries.push_front(query);
        self.entries.truncate(self.max);
    }

    pub fn get(&self, index: usize) -> Option<&str> {
        self.entries.get(index).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(row: u32, col: u16) -> Pos {
        Pos { row, col }
    }

    fn spans() -> Vec<SearchSpan> {
        vec![
            SearchSpan {
                start: pos(0, 1),
                end: pos(0, 3),
            },
            SearchSpan {
                start: pos(2, 4),
                end: pos(2, 6),
            },
            SearchSpan {
                start: pos(4, 0),
                end: pos(4, 2),
            },
        ]
    }

    #[test]
    fn initial_search_stops_at_the_buffer_boundary() {
        let matches = spans();
        assert_eq!(
            initial_selection(&matches, SearchDirection::Forward, pos(2, 0)),
            Some(SearchSelection {
                index: 1,
                wrapped: false
            })
        );
        assert_eq!(
            initial_selection(&matches, SearchDirection::Backward, pos(3, 0)),
            Some(SearchSelection {
                index: 1,
                wrapped: false
            })
        );
        assert_eq!(
            initial_selection(&matches, SearchDirection::Forward, pos(9, 0)),
            None
        );
        assert_eq!(
            initial_selection(&matches, SearchDirection::Backward, pos(0, 0)),
            None
        );
    }

    #[test]
    fn repeat_advances_in_either_direction_and_wraps() {
        let matches = spans();
        assert_eq!(
            repeated_selection(&matches, SearchDirection::Forward, Some(1)),
            Some(SearchSelection {
                index: 2,
                wrapped: false
            })
        );
        assert_eq!(
            repeated_selection(&matches, SearchDirection::Forward, Some(2)),
            Some(SearchSelection {
                index: 0,
                wrapped: true
            })
        );
        assert_eq!(
            repeated_selection(&matches, SearchDirection::Backward, Some(0)),
            Some(SearchSelection {
                index: 2,
                wrapped: true
            })
        );
        assert_eq!(
            repeated_selection(&matches, SearchDirection::Forward, None),
            Some(SearchSelection {
                index: 0,
                wrapped: true
            })
        );
    }

    #[test]
    fn search_ring_is_newest_first_deduplicated_and_bounded() {
        let mut ring = SearchRing::new(2);
        ring.push("alpha".to_string());
        ring.push("bravo".to_string());
        ring.push("alpha".to_string());
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.get(0), Some("alpha"));
        assert_eq!(ring.get(1), Some("bravo"));

        ring.push("charlie".to_string());
        assert_eq!(ring.get(0), Some("charlie"));
        assert_eq!(ring.get(1), Some("alpha"));
    }
}
