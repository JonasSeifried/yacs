//! Sealed one-time invites (see `yacs_core::invite`), kept in memory until
//! they're taken once or expire. A restart drops them, like open uploads: the
//! inviter makes a new one.

use std::collections::HashMap;
use std::sync::Mutex;

use axum::body::Bytes;
use yacs_core::{ChannelId, InviteSlot};

/// Open invites per space. Each one is a device about to join, so a few do.
pub const MAX_PER_CHANNEL: usize = 20;
/// All open invites; each holds at most 4 KB.
const MAX_TOTAL: usize = 10_000;

#[derive(Debug, PartialEq, Eq)]
pub enum InviteError {
    /// The slot is taken (only by chance or on purpose: slots are random).
    Exists,
    TooMany,
}

struct Parked {
    channel: ChannelId,
    expires_at_ms: u64,
    sealed: Bytes,
}

#[derive(Default)]
pub struct Invites {
    parked: Mutex<HashMap<InviteSlot, Parked>>,
}

impl Invites {
    pub fn put(
        &self,
        channel: ChannelId,
        slot: InviteSlot,
        sealed: Bytes,
        now_ms: u64,
        expires_at_ms: u64,
    ) -> Result<(), InviteError> {
        let mut parked = self.lock();
        parked.retain(|_, p| p.expires_at_ms > now_ms);
        if parked.contains_key(&slot) {
            return Err(InviteError::Exists);
        }
        let open = parked.values().filter(|p| p.channel == channel).count();
        if open >= MAX_PER_CHANNEL || parked.len() >= MAX_TOTAL {
            return Err(InviteError::TooMany);
        }
        parked.insert(
            slot,
            Parked {
                channel,
                expires_at_ms,
                sealed,
            },
        );
        Ok(())
    }

    /// Hands the invite out once: the next call finds nothing.
    pub fn take(&self, slot: &InviteSlot, now_ms: u64) -> Option<(ChannelId, Bytes)> {
        let parked = self.lock().remove(slot)?;
        (parked.expires_at_ms > now_ms).then_some((parked.channel, parked.sealed))
    }

    /// Only the space's own members can take an invite back.
    pub fn revoke(&self, channel: &ChannelId, slot: &InviteSlot) -> bool {
        let mut parked = self.lock();
        if parked.get(slot).is_some_and(|p| p.channel == *channel) {
            parked.remove(slot);
            return true;
        }
        false
    }

    /// Forget expired invites.
    pub fn prune(&self, now_ms: u64) {
        self.lock().retain(|_, p| p.expires_at_ms > now_ms);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<InviteSlot, Parked>> {
        self.parked.lock().expect("invites lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use yacs_core::InviteSecret;

    use super::*;

    fn slot(n: u8) -> InviteSlot {
        InviteSecret::from_bytes([n; 32]).slot()
    }

    #[test]
    fn taken_once_until_it_expires() {
        let invites = Invites::default();
        let channel = ChannelId::from_bytes([1; 32]);
        let sealed = Bytes::from_static(b"sealed");
        invites
            .put(channel, slot(1), sealed.clone(), 0, 100)
            .unwrap();
        assert_eq!(
            invites.put(channel, slot(1), sealed.clone(), 0, 100),
            Err(InviteError::Exists)
        );
        assert_eq!(invites.take(&slot(1), 50), Some((channel, sealed.clone())));
        assert_eq!(invites.take(&slot(1), 50), None);

        invites
            .put(channel, slot(2), sealed.clone(), 0, 100)
            .unwrap();
        assert_eq!(invites.take(&slot(2), 100), None);
    }

    #[test]
    fn revoked_only_by_its_space_and_limited_per_space() {
        let invites = Invites::default();
        let (a, b) = (
            ChannelId::from_bytes([1; 32]),
            ChannelId::from_bytes([2; 32]),
        );
        for n in 0..MAX_PER_CHANNEL as u8 {
            invites.put(a, slot(n), Bytes::new(), 0, 100).unwrap();
        }
        assert_eq!(
            invites.put(a, slot(200), Bytes::new(), 0, 100),
            Err(InviteError::TooMany)
        );
        invites.put(b, slot(200), Bytes::new(), 0, 100).unwrap();

        assert!(!invites.revoke(&b, &slot(0)));
        assert!(invites.revoke(&a, &slot(0)));
        assert!(!invites.revoke(&a, &slot(0)));
        invites.put(a, slot(201), Bytes::new(), 0, 100).unwrap();

        // Expired ones make room.
        assert_eq!(
            invites.put(a, slot(202), Bytes::new(), 0, 100),
            Err(InviteError::TooMany)
        );
        invites.put(a, slot(202), Bytes::new(), 100, 200).unwrap();
        invites.prune(200);
        assert!(invites.lock().is_empty());
    }
}
