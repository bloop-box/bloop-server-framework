use crate::nfc_uid::NfcUid;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

pub trait PlayerInfo: Debug {
    fn id(&self) -> Uuid;
    fn nfc_uid(&self) -> NfcUid;
    fn total_bloops(&self) -> usize;
    fn awarded_achievements(&self) -> &HashMap<Uuid, DateTime<Utc>>;
}

pub trait PlayerMutator {
    /// Increments the bloop counter by one.
    fn increment_bloops(&mut self);

    /// Adds an awarded achievement to the player.
    fn add_awarded_achievement(&mut self, achievement_id: Uuid, awarded_at: DateTime<Utc>);
}

pub struct PlayerRegistry<Player: PlayerInfo + PlayerMutator> {
    pub by_id: HashMap<Uuid, Arc<RwLock<Player>>>,
    pub by_nfc_uid: HashMap<NfcUid, Arc<RwLock<Player>>>,
}

impl<Player: PlayerInfo + PlayerMutator> PlayerRegistry<Player> {
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

    pub fn add(&mut self, player: Player) {
        let id = player.id();
        let nfc_uid = player.nfc_uid();
        let player = Arc::new(RwLock::new(player));

        self.by_id.insert(id, player.clone());
        self.by_nfc_uid.insert(nfc_uid, player);
    }

    pub fn remove(&mut self, id: Uuid) {
        let Some(player) = self.by_id.remove(&id) else {
            return;
        };

        let player = player.read().unwrap();
        self.by_nfc_uid.remove(&player.nfc_uid());
    }
}
