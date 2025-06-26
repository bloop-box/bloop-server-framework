use async_trait::async_trait;
use bloop_server_framework::event::Event;
use bloop_server_framework::nfc_uid::NfcUid;
use bloop_server_framework::player::{PlayerInfo, PlayerMutator};
use chrono::{DateTime, Utc};
use hex::FromHex;
use std::collections::HashMap;
use std::convert::Infallible;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio_graceful_shutdown::{FutureExt, IntoSubsystem, SubsystemHandle};
use tracing::{debug, warn};
use uuid::{Uuid, uuid};

#[derive(Debug)]
pub struct Player {
    pub id: Uuid,
    pub nfc_uid: NfcUid,
    pub total_bloops: usize,
    pub awarded_achievements: HashMap<Uuid, DateTime<Utc>>,
}

impl PlayerInfo for Player {
    fn id(&self) -> Uuid {
        self.id
    }

    fn nfc_uid(&self) -> NfcUid {
        self.nfc_uid
    }

    fn total_bloops(&self) -> usize {
        self.total_bloops
    }

    fn awarded_achievements(&self) -> &HashMap<Uuid, DateTime<Utc>> {
        &self.awarded_achievements
    }
}

impl PlayerMutator for Player {
    fn increment_bloops(&mut self) {
        self.total_bloops += 1;
    }

    fn add_awarded_achievement(&mut self, achievement_id: Uuid, awarded_at: DateTime<Utc>) {
        self.awarded_achievements.insert(achievement_id, awarded_at);
    }
}

pub struct DummyPlayerRepository;

impl DummyPlayerRepository {
    pub async fn load_all(&self) -> Result<Vec<Player>, Infallible> {
        let player = Player {
            id: uuid!("5e9e052c-c276-49f6-8f4d-1e05af0040be"),
            nfc_uid: NfcUid::from_hex("ababababababab").unwrap(),
            total_bloops: 0,
            awarded_achievements: HashMap::new(),
        };

        Ok(vec![player])
    }
}

pub struct AwardedAchievementsPersister {
    pub event_rx: broadcast::Receiver<Event>,
}

impl AwardedAchievementsPersister {
    pub async fn process_events(&mut self) -> anyhow::Result<()> {
        loop {
            match self.event_rx.recv().await {
                Ok(Event::AchievementsAwarded(_awarded)) => {
                    // Persist the awarded achievements to your database.
                }
                Ok(_) => {}
                Err(RecvError::Lagged(n)) => {
                    warn!(
                        "AwardedAchievementsPersister lagged by {n} messages, some achievements were missed"
                    );
                }
                Err(RecvError::Closed) => {
                    debug!("AwardedAchievementsPersister event stream closed, exiting event loop");
                    break;
                }
            }
        }

        Ok(())
    }
}

#[async_trait]
impl IntoSubsystem<anyhow::Error> for AwardedAchievementsPersister {
    async fn run(mut self, subsys: SubsystemHandle) -> anyhow::Result<()> {
        let _ = self.process_events().cancel_on_shutdown(&subsys).await;

        Ok(())
    }
}
