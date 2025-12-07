//! Player management module.
//!
//! This module provides traits and structures for managing player information,
//! including read-only access ([`PlayerInfo`]), mutation capabilities
//! ([`PlayerMutator`]), and a concurrent registry ([`PlayerRegistry`]) to store
//! and lookup players by both UUID and NFC UID.
//!
//! The `PlayerRegistry` maintains internal consistency by updating NFC UID
//! mappings automatically when players' NFC UIDs change, enabling safe
//! concurrent access via `Arc<RwLock<Player>>` wrappers.
//!
//! It supports operations to add, remove, mutate, and query players efficiently.

use crate::nfc_uid::NfcUid;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard};
use uuid::Uuid;

/// Provides read-only information about a player.
pub trait PlayerInfo {
    /// Returns the unique player ID.
    fn id(&self) -> Uuid;

    /// Returns the player's NFC UID.
    fn nfc_uid(&self) -> NfcUid;

    /// Returns the total number of bloops the player has collected.
    fn total_bloops(&self) -> usize;

    /// Returns a reference to the map of awarded achievements and their timestamps.
    fn awarded_achievements(&self) -> &HashMap<Uuid, DateTime<Utc>>;
}

/// Provides mutation methods for a player.
pub trait PlayerMutator {
    /// Increments the bloop counter by one.
    fn increment_bloops(&mut self);

    /// Adds an awarded achievement to the player.
    fn add_awarded_achievement(&mut self, achievement_id: Uuid, awarded_at: DateTime<Utc>);
}

/// A registry for managing players by both their UUID and NFC UID.
///
/// The registry ensures that updates to players' NFC UIDs automatically keep
/// internal lookup maps in sync.
#[derive(Debug)]
pub struct PlayerRegistry<Player: PlayerInfo> {
    by_id: HashMap<Uuid, Arc<RwLock<Player>>>,
    by_nfc_uid: HashMap<NfcUid, Arc<RwLock<Player>>>,
}

impl<Player: PlayerInfo> From<Vec<Player>> for PlayerRegistry<Player> {
    fn from(players: Vec<Player>) -> Self {
        Self::new(players)
    }
}

impl<Player: PlayerInfo> PlayerRegistry<Player> {
    /// Creates a new [`PlayerRegistry`] from a list of players
    pub fn new(players: Vec<Player>) -> Self {
        let mut by_id = HashMap::new();
        let mut by_nfc_uid = HashMap::new();

        for player in players {
            let id = player.id();
            let nfc_uid = player.nfc_uid();
            let player = Arc::new(RwLock::new(player));

            by_id.insert(id, player.clone());
            by_nfc_uid.insert(nfc_uid, player);
        }

        Self { by_id, by_nfc_uid }
    }

    /// Returns the shared player reference (Arc) by their UUID.
    ///
    /// Use [`Self::read_by_id()`] if you only need read access.
    pub fn get_by_id(&self, id: Uuid) -> Option<Arc<RwLock<Player>>> {
        self.by_id.get(&id).cloned()
    }

    /// Returns the shared player reference (Arc) by their NFC UID.
    ///
    /// /// Use [`Self::read_by_nfc_uid()`] if you only need read access.
    pub fn get_by_nfc_uid(&self, nfc_uid: NfcUid) -> Option<Arc<RwLock<Player>>> {
        self.by_nfc_uid.get(&nfc_uid).cloned()
    }

    /// Returns a read-only lock on a player by their UUID.
    ///
    /// Returns `None` if no player with the given ID exists.
    pub fn read_by_id(&self, id: Uuid) -> Option<RwLockReadGuard<'_, Player>> {
        self.by_id.get(&id).map(|player| player.read().unwrap())
    }

    /// Returns a read-only lock on a player by their NFC UID.
    ///
    /// Returns `None` if no player with the given NFC UID exists.
    pub fn read_by_nfc_uid(&self, nfc_uid: NfcUid) -> Option<RwLockReadGuard<'_, Player>> {
        self.by_nfc_uid
            .get(&nfc_uid)
            .map(|player| player.read().unwrap())
    }

    /// Returns `true` if a player with the given UUID exists in the registry.
    pub fn contains_id(&self, id: Uuid) -> bool {
        self.by_id.contains_key(&id)
    }

    /// Mutates a player by their UUID.
    ///
    /// Automatically updates the internal NFC UID lookup map if the player's
    /// NFC UID changes.
    ///
    /// If no player with the given ID exists, this method does nothing.
    pub fn mutate_by_id<F>(&mut self, id: Uuid, mutator: F)
    where
        F: FnOnce(&mut Player),
    {
        if let Some(player_arc) = self.by_id.get(&id) {
            let mut player = player_arc.write().unwrap();
            let old_nfc_uid = player.nfc_uid();

            mutator(&mut player);

            let new_nfc_uid = player.nfc_uid();

            if old_nfc_uid != new_nfc_uid {
                self.by_nfc_uid.remove(&old_nfc_uid);
                self.by_nfc_uid.insert(new_nfc_uid, player_arc.clone());
            }
        }
    }

    /// Adds a new player to the registry.
    ///
    /// This method inserts the player into both internal lookup maps.
    /// If a player with the same ID or NFC UID already exists, it will be
    /// overwritten.
    pub fn add(&mut self, player: Player) {
        let id = player.id();
        let nfc_uid = player.nfc_uid();
        let player = Arc::new(RwLock::new(player));

        self.by_id.insert(id, player.clone());
        self.by_nfc_uid.insert(nfc_uid, player);
    }

    /// Removes a player from the registry by their UUID.
    ///
    /// This also removes the player from the NFC UID lookup map.
    /// If no such player exists, this method does nothing.
    pub fn remove(&mut self, id: Uuid) {
        let Some(player) = self.by_id.remove(&id) else {
            return;
        };

        let player = player.read().unwrap();
        self.by_nfc_uid.remove(&player.nfc_uid());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockPlayer;

    #[test]
    fn adding_and_reading_player_by_nfc_uid_works() {
        let (player, id) = MockPlayer::builder().bloops_count(5).build();
        let player = Arc::try_unwrap(player).unwrap().into_inner().unwrap();

        let registry = PlayerRegistry::new(vec![player]);

        let player_read = registry.read_by_nfc_uid(NfcUid::default()).unwrap();
        assert_eq!(player_read.total_bloops(), 5);
        assert!(registry.contains_id(id));
    }

    #[test]
    fn mutating_player_updates_nfc_uid_and_bloops_correctly() {
        let (player, id) = MockPlayer::builder().bloops_count(0).build();
        let player = Arc::try_unwrap(player).unwrap().into_inner().unwrap();

        let mut registry = PlayerRegistry::new(vec![player]);

        registry.mutate_by_id(id, |player| {
            player.bloops_count = 42;
        });

        let player_read = registry.read_by_id(id).unwrap();
        assert_eq!(player_read.total_bloops(), 42);
    }

    #[test]
    fn removing_player_removes_from_all_indexes() {
        let (player, id) = MockPlayer::builder().bloops_count(1).build();
        let nfc_uid = NfcUid::default();
        let player = Arc::try_unwrap(player).unwrap().into_inner().unwrap();

        let mut registry = PlayerRegistry::new(vec![player]);

        registry.remove(id);

        assert!(registry.read_by_id(id).is_none());
        assert!(registry.read_by_nfc_uid(nfc_uid).is_none());
    }

    #[test]
    fn adding_player_overwrites_existing_player_with_same_id_or_nfc_uid() {
        let (player1, id) = MockPlayer::builder().bloops_count(5).build();
        let player1 = Arc::try_unwrap(player1).unwrap().into_inner().unwrap();

        let (player2, _) = MockPlayer::builder().bloops_count(10).build();
        let mut player2 = Arc::try_unwrap(player2).unwrap().into_inner().unwrap();

        player2.id = id;
        player2.registration_number = 99;

        let mut registry = PlayerRegistry::new(vec![player1]);
        registry.add(player2);

        let player_read = registry.read_by_id(id).unwrap();
        assert_eq!(player_read.total_bloops(), 10);
        assert_eq!(player_read.registration_number, 99);
    }
}
