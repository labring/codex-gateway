use std::sync::atomic::Ordering;

use chrono::Utc;
use tracing::warn;

use crate::models::{
    BridgeEvent, BridgeStateSnapshot, SummaryEvent, TranscriptEntry, WarningEvent,
};

use super::protocol::{preview_string, unix_millis};
use super::transcript::trim_transcript;
use super::{CodexAppServerBridge, MAX_EVENTS};

impl CodexAppServerBridge {
    pub(super) fn emit_warning(&self, warning: WarningEvent) {
        warn!(
            warning_type = %warning.warning_type,
            detail = warning.detail.as_deref().unwrap_or("-"),
            message = %warning.message,
            "bridge warning"
        );
        self.record_summary_event(SummaryEvent {
            at: Utc::now().to_rfc3339(),
            event_type: "warning".to_string(),
            method: Some(warning.warning_type.clone()),
            item_type: None,
            item_id: None,
            status: Some("warning".to_string()),
            text_preview: preview_string(&warning.message).or_else(|| warning.detail.clone()),
        });
        self.touch_activity();
        let _ = self.inner.events.send(BridgeEvent::Warning(warning));
        self.emit_state();
    }

    pub(super) fn emit_state(&self) {
        self.touch_activity();
        let _ = self
            .inner
            .events
            .send(BridgeEvent::State(Box::new(self.get_state())));
    }

    pub(super) fn with_state<T>(&self, mutator: impl FnOnce(&mut BridgeStateSnapshot) -> T) -> T {
        let mut state = self.inner.state.write().unwrap();
        mutator(&mut state)
    }

    pub(super) fn push_transcript(&self, entry: TranscriptEntry) {
        self.with_state(|state| {
            state.transcript.push(entry);
            trim_transcript(&mut state.transcript);
        });
    }

    pub(super) fn upsert_transcript(&self, entry: TranscriptEntry) {
        self.with_state(|state| {
            if let Some(existing) = state
                .transcript
                .iter_mut()
                .find(|current| current.id == entry.id)
            {
                existing.role = entry.role;
                existing.text = entry.text;
                existing.status = entry.status;
                existing.source = entry.source;
                return;
            }

            state.transcript.push(entry);
            trim_transcript(&mut state.transcript);
        });
    }

    pub(super) fn push_system_note(&self, text: String) {
        self.push_transcript(TranscriptEntry {
            id: self.make_local_id("system"),
            role: "system".to_string(),
            text,
            status: "completed".to_string(),
            source: "bridge".to_string(),
            created_at: unix_millis(),
        });
    }

    pub(super) fn has_recent_user_text(&self, text: &str) -> bool {
        self.get_state()
            .transcript
            .into_iter()
            .rev()
            .find(|entry| entry.role == "user")
            .map(|entry| entry.text == text)
            .unwrap_or(false)
    }

    pub(super) fn record_summary_event(&self, entry: SummaryEvent) {
        self.with_state(|state| {
            state.recent_events.push(entry);
            if state.recent_events.len() > MAX_EVENTS {
                let drain = state.recent_events.len() - MAX_EVENTS;
                state.recent_events.drain(0..drain);
            }
        });
    }

    pub(super) fn make_local_id(&self, prefix: &str) -> String {
        let next = self.inner.local_counter.fetch_add(1, Ordering::SeqCst) + 1;
        format!("local-{prefix}-{next}")
    }

    pub(super) fn touch_activity(&self) {
        (self.inner.activity_touch)();
    }
}
