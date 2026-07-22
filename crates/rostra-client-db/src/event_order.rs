use rostra_core::{ShortEventId, Timestamp};

/// Total order key for reducers that retain one source event.
///
/// Ordering is lexicographic by the signed event timestamp and then
/// `ShortEventId`. Field declaration order therefore defines reducer
/// precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct EventOrder {
    /// Timestamp from the signed source event.
    timestamp: Timestamp,
    /// Short ID derived from the signed source event.
    event_id: ShortEventId,
}

impl EventOrder {
    /// Construct an event-order key from a signed event's fields.
    pub(crate) fn new(timestamp: Timestamp, event_id: ShortEventId) -> Self {
        Self {
            timestamp,
            event_id,
        }
    }

    /// Return the signed event timestamp component.
    pub(crate) fn timestamp(self) -> Timestamp {
        self.timestamp
    }

    /// Return the event ID component.
    pub(crate) fn event_id(self) -> ShortEventId {
        self.event_id
    }
}
