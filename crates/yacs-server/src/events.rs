//! Live updates: `GET /api/v1/channels/{channel}/events` streams a
//! [`ChannelEvent`] whenever that channel's clips change, so clients don't
//! have to poll. Events carry only metadata the relay already has.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Mutex;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::{Stream, StreamExt, stream};
use tokio::sync::{broadcast, watch};
use yacs_core::ChannelId;
use yacs_core::api::ChannelEvent;

/// Events a slow listener may fall behind by. Past that its stream ends, and
/// the client reconnects and re-lists, which it must do after any gap anyway.
const BUFFER: usize = 16;

/// Comment lines on idle streams, so proxies (nginx drops a quiet upstream
/// after 60 s) and clients can tell a live connection from a dead one.
pub const KEEP_ALIVE: Duration = Duration::from_secs(20);

pub struct Events {
    channels: Mutex<HashMap<ChannelId, broadcast::Sender<ChannelEvent>>>,
    closed: watch::Sender<bool>,
}

impl Default for Events {
    fn default() -> Self {
        Self {
            channels: Mutex::default(),
            closed: watch::Sender::new(false),
        }
    }
}

impl Events {
    pub fn publish(&self, channel: &ChannelId, event: ChannelEvent) {
        if let Some(tx) = self.lock().get(channel) {
            // Fails only when nobody listens; `prune` removes those channels.
            let _ = tx.send(event);
        }
    }

    /// The SSE response for one listener. Ends when the relay shuts down,
    /// or when the listener fell too far behind.
    pub fn stream(
        &self,
        channel: &ChannelId,
    ) -> Sse<impl Stream<Item = Result<Event, Infallible>> + use<>> {
        let rx = self
            .lock()
            .entry(*channel)
            .or_insert_with(|| broadcast::channel(BUFFER).0)
            .subscribe();
        let events = stream::unfold(rx, |mut rx| async move {
            let event = rx.recv().await.ok()?;
            let sse = Event::default()
                .json_data(&event)
                .expect("channel events serialize");
            Some((Ok(sse), rx))
        });
        let mut closed = self.closed.subscribe();
        let closing = async move {
            let _ = closed.wait_for(|closed| *closed).await;
        };
        Sse::new(events.take_until(closing)).keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
    }

    /// Forget channels nobody listens to anymore.
    pub fn prune(&self) {
        self.lock().retain(|_, tx| tx.receiver_count() > 0);
    }

    /// End every stream, so a graceful shutdown doesn't wait on them forever.
    pub fn close(&self) {
        self.closed.send_replace(true);
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<ChannelId, broadcast::Sender<ChannelEvent>>> {
        self.channels.lock().expect("events lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prune_forgets_channels_without_listeners() {
        let events = Events::default();
        let channel = ChannelId::from_bytes([1; 32]);
        let listener = events.stream(&channel);
        events.prune();
        assert_eq!(events.lock().len(), 1);

        drop(listener);
        events.prune();
        assert!(events.lock().is_empty());
        // Publishing to a channel nobody listens to is a no-op.
        events.publish(&channel, ChannelEvent::Cleared);
        assert!(events.lock().is_empty());
    }
}
