//! Rendezvous for typed codes (see `yacs_core::code`): a numbered mailbox
//! where the inviter (side `a`, a member of the space) and the device that
//! typed the code (side `b`) leave each other a few messages, each written
//! once. Kept in memory for 10 minutes at most; a restart drops them, and the
//! inviter shows a new code.
//!
//! Each message can be written once, so the side typing the code gets one
//! answer, and with it one guess, per nameplate.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use axum::body::Bytes;
use tokio::sync::watch;
use yacs_core::{CODE_TTL_SECS, ChannelId, MAX_NAMEPLATE};

/// Open codes per space: an Invite window shows one, two leaves room for a second window.
pub const MAX_PER_CHANNEL: usize = 2;
/// Messages per side: `a/0`, `b/0` and `a/1` are all a code exchange uses.
pub const MAX_INDEX: u8 = 3;
/// A SPAKE2 message, a device name or an invite, sealed.
pub const MAX_MESSAGE: usize = 4096;
/// Longest a read waits for its message, below the 60 s after which proxies
/// (nginx) drop a quiet request.
pub const MAX_WAIT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// The inviter, in the space.
    A,
    /// The device that typed the code.
    B,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RendezvousError {
    /// No such nameplate, or it expired or was closed.
    NotFound,
    /// That message was written already.
    Taken,
    TooMany,
    Full,
}

struct Mailbox {
    channel: ChannelId,
    expires_at_ms: u64,
    messages: HashMap<(Side, u8), Bytes>,
    /// Wakes readers when a message arrives. Dropped with the mailbox, which
    /// wakes them too.
    changed: watch::Sender<()>,
}

#[derive(Default)]
pub struct Rendezvous {
    board: Mutex<Board>,
}

/// How long a nameplate rests once its rendezvous ends, so a code typed late
/// (or guessed from an old one) finds nothing instead of someone's new code.
const REST_MS: u64 = CODE_TTL_SECS * 1000;

#[derive(Default)]
struct Board {
    open: BTreeMap<u16, Mailbox>,
    /// Nameplates not to hand out before the given time.
    resting: HashMap<u16, u64>,
}

impl Board {
    fn end(&mut self, nameplate: u16, now_ms: u64) {
        if let Some(mailbox) = self.open.remove(&nameplate) {
            let ended = now_ms.min(mailbox.expires_at_ms);
            self.resting.insert(nameplate, ended + REST_MS);
        }
    }

    fn expire(&mut self, now_ms: u64) {
        let expired: Vec<u16> = self
            .open
            .iter()
            .filter(|(_, m)| m.expires_at_ms <= now_ms)
            .map(|(n, _)| *n)
            .collect();
        for nameplate in expired {
            self.end(nameplate, now_ms);
        }
        self.resting.retain(|_, until| *until > now_ms);
    }

    /// The mailbox, if it's there, unexpired, and `channel`'s (when given).
    fn live(
        &mut self,
        nameplate: u16,
        channel: Option<&ChannelId>,
        now_ms: u64,
    ) -> Result<&mut Mailbox, RendezvousError> {
        if self
            .open
            .get(&nameplate)
            .is_some_and(|m| m.expires_at_ms <= now_ms)
        {
            self.end(nameplate, now_ms);
        }
        let mailbox = self
            .open
            .get_mut(&nameplate)
            .ok_or(RendezvousError::NotFound)?;
        if channel.is_some_and(|c| *c != mailbox.channel) {
            return Err(RendezvousError::NotFound);
        }
        Ok(mailbox)
    }
}

impl Rendezvous {
    /// A new mailbox for `channel`, holding the inviter's first message as
    /// `a/0`. Returns its nameplate, the lowest one free.
    pub fn open(
        &self,
        channel: ChannelId,
        first: Bytes,
        now_ms: u64,
        expires_at_ms: u64,
    ) -> Result<u16, RendezvousError> {
        let mut board = self.lock();
        board.expire(now_ms);
        if board.open.values().filter(|m| m.channel == channel).count() >= MAX_PER_CHANNEL {
            return Err(RendezvousError::TooMany);
        }
        let nameplate = (1..=MAX_NAMEPLATE)
            .find(|n| !board.open.contains_key(n) && !board.resting.contains_key(n))
            .ok_or(RendezvousError::Full)?;
        board.open.insert(
            nameplate,
            Mailbox {
                channel,
                expires_at_ms,
                messages: HashMap::from([((Side::A, 0), first)]),
                changed: watch::Sender::new(()),
            },
        );
        Ok(nameplate)
    }

    /// Writes message `side/index`. Side `a` only for its own space's members
    /// (`channel`), side `b` for anyone.
    pub fn put(
        &self,
        nameplate: u16,
        side: Side,
        index: u8,
        body: Bytes,
        channel: Option<&ChannelId>,
        now_ms: u64,
    ) -> Result<(), RendezvousError> {
        if side == Side::A && channel.is_none() {
            return Err(RendezvousError::NotFound);
        }
        let mut board = self.lock();
        let mailbox = board.live(nameplate, channel, now_ms)?;
        if mailbox.messages.contains_key(&(side, index)) {
            return Err(RendezvousError::Taken);
        }
        mailbox.messages.insert((side, index), body);
        mailbox.changed.send_replace(());
        Ok(())
    }

    /// Message `side/index`, waiting up to `wait` for it: `None` if it didn't
    /// come in time. Side `b`'s messages only for the space's members. The
    /// joiner's read of the invite (`a/1`) closes the mailbox, since that's
    /// the end of the exchange.
    pub async fn read(
        &self,
        nameplate: u16,
        side: Side,
        index: u8,
        channel: Option<&ChannelId>,
        now_ms: impl Fn() -> u64,
        wait: Duration,
    ) -> Result<Option<Bytes>, RendezvousError> {
        if side == Side::B && channel.is_none() {
            return Err(RendezvousError::NotFound);
        }
        let deadline = tokio::time::Instant::now() + wait.min(MAX_WAIT);
        loop {
            let mut changed = {
                let now = now_ms();
                let mut board = self.lock();
                let mailbox = board.live(nameplate, channel, now)?;
                if let Some(message) = mailbox.messages.get(&(side, index)).cloned() {
                    if channel.is_none() && (side, index) == (Side::A, 1) {
                        board.end(nameplate, now);
                    }
                    return Ok(Some(message));
                }
                mailbox.changed.subscribe()
            };
            // Otherwise a message came, or the mailbox went: look again.
            if tokio::time::timeout_at(deadline, changed.changed())
                .await
                .is_err()
            {
                return Ok(None);
            }
        }
    }

    /// For the inviter, when its window closes or the code was guessed wrong.
    pub fn close(&self, channel: &ChannelId, nameplate: u16, now_ms: u64) -> bool {
        let mut board = self.lock();
        if board
            .open
            .get(&nameplate)
            .is_some_and(|m| m.channel == *channel)
        {
            board.end(nameplate, now_ms);
            return true;
        }
        false
    }

    pub fn prune(&self, now_ms: u64) {
        self.lock().expire(now_ms);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Board> {
        self.board.lock().expect("rendezvous lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    const NO_WAIT: Duration = Duration::ZERO;

    fn channel(n: u8) -> ChannelId {
        ChannelId::from_bytes([n; 32])
    }

    #[tokio::test]
    async fn a_whole_exchange() {
        let r = Rendezvous::default();
        let a = channel(1);
        let n = r.open(a, Bytes::from_static(b"a0"), 0, 100).unwrap();
        assert_eq!(n, 1);
        let now = || 0;

        let first = r.read(n, Side::A, 0, None, now, NO_WAIT).await;
        assert_eq!(first, Ok(Some(Bytes::from_static(b"a0"))));
        r.put(n, Side::B, 0, Bytes::from_static(b"b0"), None, 0)
            .unwrap();
        assert_eq!(
            r.put(n, Side::B, 0, Bytes::from_static(b"guess"), None, 0),
            Err(RendezvousError::Taken)
        );
        let answer = r.read(n, Side::B, 0, Some(&a), now, NO_WAIT).await;
        assert_eq!(answer, Ok(Some(Bytes::from_static(b"b0"))));
        r.put(n, Side::A, 1, Bytes::from_static(b"a1"), Some(&a), 0)
            .unwrap();
        let invite = r.read(n, Side::A, 1, None, now, NO_WAIT).await;
        assert_eq!(invite, Ok(Some(Bytes::from_static(b"a1"))));
        // Done: the mailbox is gone.
        let again = r.read(n, Side::A, 1, None, now, NO_WAIT).await;
        assert_eq!(again, Err(RendezvousError::NotFound));
    }

    #[tokio::test]
    async fn only_members_write_side_a_and_read_side_b() {
        let r = Rendezvous::default();
        let n = r.open(channel(1), Bytes::new(), 0, 100).unwrap();
        for other in [None, Some(&channel(2))] {
            assert_eq!(
                r.put(n, Side::A, 1, Bytes::new(), other, 0),
                Err(RendezvousError::NotFound)
            );
            let read = r.read(n, Side::B, 0, other, || 0, NO_WAIT).await;
            assert_eq!(read, Err(RendezvousError::NotFound));
        }
        r.put(n, Side::A, 1, Bytes::new(), Some(&channel(1)), 0)
            .unwrap();
        assert!(!r.close(&channel(2), n, 0));
        assert!(r.close(&channel(1), n, 0));
    }

    #[tokio::test]
    async fn readers_wait_for_the_message_or_the_end() {
        let r = Arc::new(Rendezvous::default());
        let a = channel(1);
        let n = r.open(a, Bytes::new(), 0, 100).unwrap();
        let wait = Duration::from_secs(5);

        let reader = {
            let r = r.clone();
            tokio::spawn(async move { r.read(n, Side::B, 0, Some(&a), || 0, wait).await })
        };
        tokio::task::yield_now().await;
        r.put(n, Side::B, 0, Bytes::from_static(b"b0"), None, 0)
            .unwrap();
        assert_eq!(reader.await.unwrap(), Ok(Some(Bytes::from_static(b"b0"))));

        let reader = {
            let r = r.clone();
            tokio::spawn(async move { r.read(n, Side::A, 1, None, || 0, wait).await })
        };
        tokio::task::yield_now().await;
        r.close(&a, n, 0);
        assert_eq!(reader.await.unwrap(), Err(RendezvousError::NotFound));

        let n = r.open(a, Bytes::new(), 0, 100).unwrap();
        let quiet = r.read(n, Side::B, 0, Some(&a), || 0, Duration::from_millis(10));
        assert_eq!(quiet.await, Ok(None));
    }

    #[tokio::test]
    async fn limited_per_space_and_expiring() {
        let r = Rendezvous::default();
        let (a, b) = (channel(1), channel(2));
        assert_eq!(r.open(a, Bytes::new(), 0, 100), Ok(1));
        assert_eq!(r.open(a, Bytes::new(), 0, 100), Ok(2));
        assert_eq!(
            r.open(a, Bytes::new(), 0, 100),
            Err(RendezvousError::TooMany)
        );
        assert_eq!(r.open(b, Bytes::new(), 0, 100), Ok(3));

        let expired = r.read(2, Side::A, 0, None, || 100, NO_WAIT).await;
        assert_eq!(expired, Err(RendezvousError::NotFound));
        r.prune(100);
        assert!(r.lock().open.is_empty());
    }

    #[tokio::test]
    async fn nameplates_rest_before_they_come_back() {
        let r = Rendezvous::default();
        let a = channel(1);
        assert_eq!(r.open(a, Bytes::new(), 0, 100), Ok(1));
        r.close(&a, 1, 10);
        // A late code for 1 finds nothing, not the next code.
        assert_eq!(r.open(a, Bytes::new(), 10, 110), Ok(2));
        let late = r.read(1, Side::A, 0, None, || 20, NO_WAIT).await;
        assert_eq!(late, Err(RendezvousError::NotFound));

        r.close(&a, 2, 20);
        assert_eq!(r.open(a, Bytes::new(), 10 + REST_MS, u64::MAX), Ok(1));
        // Expired ones rest from when they expired: 3 is skipped.
        assert_eq!(r.open(a, Bytes::new(), 0, 50), Ok(3));
        assert_eq!(r.open(a, Bytes::new(), 60, u64::MAX), Ok(4));
        r.prune(50 + REST_MS - 1);
        assert!(r.lock().resting.contains_key(&3));
        r.prune(50 + REST_MS);
        assert!(!r.lock().resting.contains_key(&3));
    }
}
