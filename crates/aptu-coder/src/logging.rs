// SPDX-FileCopyrightText: 2026 aptu-coder contributors
// SPDX-License-Identifier: Apache-2.0
//! MCP logging integration via tracing.
//!
//! Provides a custom tracing subscriber that forwards log events to MCP clients.

use serde_json::{Map, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::subscriber::Interest;
use tracing::{Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::Context;

/// Lightweight event sent from `McpLoggingLayer` to consumer task via unbounded channel.
#[derive(Clone, Debug)]
pub struct LogEvent {
    pub level: Level,
    pub logger: String,
    pub data: Value,
}

/// Custom tracing Layer that bridges tracing events to `MCP` client via unbounded channel.
/// Sends lightweight [`LogEvent`] to channel; consumer task in `on_initialized` drains with `recv_many`.
pub struct McpLoggingLayer {
    event_tx: mpsc::UnboundedSender<LogEvent>,
    log_level_filter: Arc<Mutex<LevelFilter>>,
}

impl McpLoggingLayer {
    pub fn new(
        event_tx: mpsc::UnboundedSender<LogEvent>,
        log_level_filter: Arc<Mutex<LevelFilter>>,
    ) -> Self {
        Self {
            event_tx,
            log_level_filter,
        }
    }
}

impl<S> Layer<S> for McpLoggingLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level = *metadata.level();
        let target = metadata.target();

        // Check if event level passes the current filter before processing
        let filter_level = self
            .log_level_filter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if level > *filter_level {
            return;
        }
        drop(filter_level);

        // Extract fields from the event using a visitor that collects into a Map.
        let mut fields = Map::new();
        let mut visitor = MessageVisitor(&mut fields);
        event.record(&mut visitor);

        let logger = target.to_string();
        let data = Value::Object(fields);

        // Send LogEvent to channel without blocking on_event.
        let log_event = LogEvent {
            level,
            logger,
            data,
        };

        // Ignore send error if receiver is dropped (channel closed).
        let _ = self.event_tx.send(log_event);
    }

    fn register_callsite(&self, metadata: &'static tracing::Metadata<'static>) -> Interest {
        let filter_level = self
            .log_level_filter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *metadata.level() <= *filter_level {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn enabled(&self, metadata: &tracing::Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        let filter_level = self
            .log_level_filter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *metadata.level() <= *filter_level
    }
}

/// Visitor to extract fields from tracing event into a JSON Map.
struct MessageVisitor<'a>(&'a mut Map<String, Value>);

impl tracing::field::Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(
            field.name().to_string(),
            Value::String(format!("{value:?}")),
        );
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0
            .insert(field.name().to_string(), Value::String(value.to_string()));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0
            .insert(field.name().to_string(), Value::Number(value.into()));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.0
            .insert(field.name().to_string(), Value::Number(value.into()));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.0.insert(field.name().to_string(), Value::Bool(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::Level;
    #[test]
    fn test_log_event_level_is_tracing_level() {
        let event = LogEvent {
            level: Level::ERROR,
            logger: "test".to_string(),
            data: Value::Null,
        };
        assert_eq!(event.level, Level::ERROR);
    }
}
