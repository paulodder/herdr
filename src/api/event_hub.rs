#[derive(Clone, Default)]
pub struct EventHub {
    inner: std::sync::Arc<std::sync::Mutex<EventHubState>>,
}

#[derive(Default)]
struct EventHubState {
    next_sequence: u64,
    events: Vec<(u64, crate::api::schema::EventEnvelope)>,
}

impl EventHub {
    pub(super) const MAX_EVENTS: usize = 512;

    pub fn push(&self, event: crate::api::schema::EventEnvelope) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        state.next_sequence += 1;
        let sequence = state.next_sequence;
        state.events.push((sequence, event));
        let overflow = state.events.len().saturating_sub(Self::MAX_EVENTS);
        if overflow > 0 {
            state.events.drain(0..overflow);
        }
    }

    pub fn events_after(&self, sequence: u64) -> Vec<(u64, crate::api::schema::EventEnvelope)> {
        let Ok(state) = self.inner.lock() else {
            return Vec::new();
        };
        state
            .events
            .iter()
            .filter(|(event_sequence, _)| *event_sequence > sequence)
            .cloned()
            .collect()
    }

    pub fn current_sequence(&self) -> u64 {
        let Ok(state) = self.inner.lock() else {
            return 0;
        };
        state.next_sequence
    }

    /// Returns whether every event after `sequence` is still replayable.
    pub fn can_replay_after(&self, sequence: u64) -> bool {
        let Ok(state) = self.inner.lock() else {
            return false;
        };
        Self::state_can_replay_after(&state, sequence)
    }

    /// Atomically verifies the replay window and clones retained events.
    ///
    /// A streaming consumer must use this instead of checking and reading in
    /// separate lock acquisitions, otherwise a busy session can evict the
    /// first required event between those operations.
    pub fn events_after_checked(
        &self,
        sequence: u64,
    ) -> Option<Vec<(u64, crate::api::schema::EventEnvelope)>> {
        let state = self.inner.lock().ok()?;
        if !Self::state_can_replay_after(&state, sequence) {
            return None;
        }
        Some(
            state
                .events
                .iter()
                .filter(|(event_sequence, _)| *event_sequence > sequence)
                .cloned()
                .collect(),
        )
    }

    fn state_can_replay_after(state: &EventHubState, sequence: u64) -> bool {
        if sequence > state.next_sequence {
            return false;
        }
        if sequence == state.next_sequence {
            return true;
        }
        let oldest = state
            .events
            .first()
            .map(|(event_sequence, _)| *event_sequence)
            .unwrap_or_else(|| state.next_sequence.saturating_add(1));
        sequence.saturating_add(1) >= oldest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{EventData, EventEnvelope, EventKind};

    fn event(index: usize) -> EventEnvelope {
        EventEnvelope {
            event: EventKind::WorkspaceClosed,
            data: EventData::WorkspaceClosed {
                workspace_id: format!("w{index}"),
                workspace: None,
            },
        }
    }

    #[test]
    fn replay_window_accepts_current_and_retained_cursors() {
        let hub = EventHub::default();
        assert!(hub.can_replay_after(0));
        hub.push(event(1));
        hub.push(event(2));
        assert!(hub.can_replay_after(0));
        assert!(hub.can_replay_after(1));
        assert!(hub.can_replay_after(2));
        assert!(!hub.can_replay_after(3));
    }

    #[test]
    fn replay_window_rejects_evicted_cursors() {
        let hub = EventHub::default();
        for index in 0..=EventHub::MAX_EVENTS {
            hub.push(event(index));
        }
        assert!(!hub.can_replay_after(0));
        assert!(hub.can_replay_after(1));
        assert!(hub.events_after_checked(0).is_none());
        assert_eq!(
            hub.events_after_checked(1).expect("retained replay").len(),
            EventHub::MAX_EVENTS
        );
    }
}
