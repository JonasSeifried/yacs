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

/// Listeners per channel: one per device and window, so a space has room for
/// many, but not for a connection flood.
pub const MAX_LISTENERS: usize = 64;

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

    /// The SSE response for one listener, holding on to `held` (say, its
    /// place among a client's connections) while it lasts. Ends when the relay
    /// shuts down, or when the listener fell too far behind. `None` when the
    /// channel has [`MAX_LISTENERS`] already.
    pub fn stream<H: Send + 'static>(
        &self,
        channel: &ChannelId,
        held: H,
    ) -> Option<Sse<impl Stream<Item = Result<Event, Infallible>> + use<H>>> {
        let rx = {
            let mut channels = self.lock();
            let tx = channels
                .entry(*channel)
                .or_insert_with(|| broadcast::channel(BUFFER).0);
            if tx.receiver_count() >= MAX_LISTENERS {
                return None;
            }
            tx.subscribe()
        };
        let events = stream::unfold((rx, held), |(mut rx, held)| async move {
            let event = rx.recv().await.ok()?;
            let sse = Event::default()
                .json_data(&event)
                .expect("channel events serialize");
            Some((Ok(sse), (rx, held)))
        });
        let mut closed = self.closed.subscribe();
        let closing = async move {
            let _ = closed.wait_for(|closed| *closed).await;
        };
        Some(Sse::new(events.take_until(closing)).keep_alive(KeepAlive::new().interval(KEEP_ALIVE)))
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
        let listener = events.stream(&channel, ()).unwrap();
        events.prune();
        assert_eq!(events.lock().len(), 1);

        drop(listener);
        events.prune();
        assert!(events.lock().is_empty());
        // Publishing to a channel nobody listens to is a no-op.
        events.publish(&channel, ChannelEvent::Cleared);
        assert!(events.lock().is_empty());
    }

    #[tokio::test]
    async fn a_channel_takes_a_limited_number_of_listeners() {
        let events = Events::default();
        let channel = ChannelId::from_bytes([1; 32]);
        let listeners: Vec<_> = (0..MAX_LISTENERS)
            .map(|_| events.stream(&channel, ()).unwrap())
            .collect();
        assert!(events.stream(&channel, ()).is_none());
        assert!(events.stream(&ChannelId::from_bytes([2; 32]), ()).is_some());
        drop(listeners);
        assert!(events.stream(&channel, ()).is_some());
    }
}
