use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;

#[derive(Clone, Debug, Default)]
pub(super) struct EventCapture {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CapturedEvent {
    pub target: String,
    pub message: String,
    pub fields: BTreeMap<String, String>,
    pub in_span: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CapturedSpan {
    pub target: String,
    pub name: String,
    pub fields: BTreeMap<String, String>,
}

impl EventCapture {
    pub fn subscriber(&self) -> impl Subscriber + Send + Sync {
        tracing_subscriber::registry().with(self.clone())
    }

    pub fn events(&self, message: &str) -> Vec<CapturedEvent> {
        self.events
            .lock()
            .expect("capture lock")
            .iter()
            .filter(|event| event.message == message)
            .cloned()
            .collect()
    }

    pub fn spans(&self, name: &str) -> Vec<CapturedSpan> {
        self.spans
            .lock()
            .expect("capture lock")
            .iter()
            .filter(|span| span.name == name)
            .cloned()
            .collect()
    }
}

impl<S> Layer<S> for EventCapture
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(
        &self,
        attrs: &Attributes<'_>,
        _id: &Id,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);
        self.spans.lock().expect("capture lock").push(CapturedSpan {
            target: attrs.metadata().target().to_owned(),
            name: attrs.metadata().name().to_owned(),
            fields: visitor.fields,
        });
    }

    fn on_event(&self, event: &Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let message = visitor.fields.remove("message").unwrap_or_default();
        self.events
            .lock()
            .expect("capture lock")
            .push(CapturedEvent {
                target: event.metadata().target().to_owned(),
                message,
                fields: visitor.fields,
                in_span: ctx.event_scope(event).is_some(),
            });
    }
}

#[derive(Default)]
struct FieldVisitor {
    fields: BTreeMap<String, String>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields
            .insert(field.name().to_owned(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_owned(), value.to_owned());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }
}

pub(super) fn assert_no_forbidden_fields(event: &CapturedEvent) {
    assert!(
        !event.in_span,
        "privacy-safe diagnostic inherited span fields: {event:?}"
    );
    const FORBIDDEN: &[&str] = &[
        "body",
        "content",
        "identity",
        "identity_id",
        "recipient",
        "recipient_id",
        "endpoint",
        "ticket",
        "packet",
        "secret",
        "error",
        "err",
    ];
    for field in FORBIDDEN {
        assert!(
            !event.fields.contains_key(*field),
            "diagnostic contains forbidden field {field}: {event:?}"
        );
    }
}
