use crate::achievement::{Achievement, AchievementAwardBatch, AchievementContext, AwardedTracker};
use crate::bloop::{Bloop, BloopProvider, ProcessedBloop};
use crate::event::Event;
use crate::network::{AchievementResponse, ErrorResponse, StandardResponse};
use crate::nfc_uid::NfcUid;
use crate::player::{PlayerInfo, PlayerMutator, PlayerRegistry};
use crate::trigger::TriggerRegistry;
#[cfg(feature = "tokio-graceful-shutdown")]
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use md5::Digest;
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
#[cfg(feature = "tokio-graceful-shutdown")]
use tokio_graceful_shutdown::{FutureExt, IntoSubsystem, SubsystemHandle};
use tracing::warn;
use uuid::Uuid;

pub enum EngineRequest {
    Bloop { client_id: String, nfc_uid: NfcUid },
    RetrieveAudio { id: Uuid },
    PreloadCheck { state_hash: Option<Vec<u8>> },
}

struct HotAchievement {
    id: Uuid,
    client_id: String,
    until: DateTime<Utc>,
}

impl HotAchievement {
    fn new<Player: PlayerInfo>(id: Uuid, bloop: &Bloop<Player>, duration: Duration) -> Self {
        Self {
            id,
            client_id: bloop.client_id.clone(),
            until: bloop.recorded_at + duration,
        }
    }
}

pub struct Engine<Player, Metadata, Trigger>
where
    Player: PlayerInfo + PlayerMutator,
    Trigger: Copy + PartialEq + Eq + Debug,
{
    bloop_provider: BloopProvider<Player>,
    achievements: HashMap<Uuid, Achievement<Player, Metadata, Trigger>>,
    audio_base_path: PathBuf,
    audio_hashes: HashMap<Uuid, Digest>,
    global_hash: Digest,
    player_registry: Arc<Mutex<PlayerRegistry<Player>>>,
    metadata: Arc<Mutex<Metadata>>,
    trigger_registry: TriggerRegistry<Trigger>,
    hot_achievements: Vec<HotAchievement>,
    network_rx: mpsc::Receiver<(EngineRequest, oneshot::Sender<StandardResponse>)>,
    event_tx: broadcast::Sender<Event>,
}

impl<Player, Metadata, Trigger> Engine<Player, Metadata, Trigger>
where
    Player: PlayerInfo + PlayerMutator,
    Trigger: Copy + PartialEq + Eq + Debug,
{
    pub async fn process_requests(&mut self) {
        while let Some((request, response)) = self.network_rx.recv().await {
            match request {
                EngineRequest::Bloop { nfc_uid, client_id } => {
                    self.handle_bloop(nfc_uid, client_id, response).await;
                }
                EngineRequest::RetrieveAudio { id } => {
                    self.handle_retrieve_audio(id, response);
                }
                EngineRequest::PreloadCheck { state_hash } => {
                    self.handle_preload_check(state_hash, response);
                }
            }
        }
    }

    async fn handle_bloop(
        &mut self,
        nfc_uid: NfcUid,
        client_id: String,
        response: oneshot::Sender<StandardResponse>,
    ) {
        if self
            .trigger_registry
            .try_activate_trigger(nfc_uid, &client_id)
        {
            let _ = response.send(StandardResponse::BloopRecorded {
                achievements: Vec::new(),
            });
            return;
        }

        let bloop = {
            let player_maps = self.player_registry.lock().await;
            let Some(player) = player_maps.by_nfc_uid.get(&nfc_uid) else {
                let _ = response.send(StandardResponse::Error(ErrorResponse::UnknownNfcUid));
                return;
            };
            player.write().unwrap().increment_bloops();
            Bloop::new(player.clone(), client_id, Utc::now())
        };

        let mut awarded_tracker = self.evaluate_achievements(&bloop).await;
        self.activate_hot_achievements(&bloop, &awarded_tracker);
        self.inject_hot_achievements(&bloop, &mut awarded_tracker);

        let player_maps = self.player_registry.lock().await;
        awarded_tracker.remove_duplicates(&player_maps.by_id);

        let achievement_ids: Vec<Uuid> = awarded_tracker
            .for_player(bloop.player_id)
            .map_or_else(Vec::new, |set| set.iter().cloned().collect());

        self.apply_awarded(awarded_tracker).await;

        let processed_bloop: ProcessedBloop = (&bloop).into();
        self.bloop_provider.add(Arc::new(bloop));
        let _ = self.event_tx.send(Event::BloopProcessed(processed_bloop));

        let _ = response.send(StandardResponse::BloopRecorded {
            achievements: achievement_ids
                .into_iter()
                .map(|id| AchievementResponse {
                    id,
                    audio_hash: *self.audio_hashes.get(&id).unwrap(),
                })
                .collect(),
        });
    }

    async fn evaluate_achievements(&mut self, bloop: &Bloop<Player>) -> AwardedTracker {
        let previous_awarded: HashSet<Uuid> = {
            let player = bloop.player();
            player.awarded_achievements().keys().cloned().collect()
        };
        let metadata = self.metadata.lock().await;
        let ctx = AchievementContext::new(
            bloop,
            &self.bloop_provider,
            &*metadata,
            &mut self.trigger_registry,
        );

        for achievement in self.achievements.values() {
            if !previous_awarded.contains(&achievement.definition.id) {
                achievement.evaluate(&ctx);
            }
        }

        ctx.take_awarded_tracker()
    }

    fn inject_hot_achievements(
        &mut self,
        bloop: &Bloop<Player>,
        awarded_tracker: &mut AwardedTracker,
    ) {
        let awarded = awarded_tracker.for_player_mut(bloop.player_id);

        self.hot_achievements.retain(|hot_achievement| {
            if hot_achievement.until < bloop.recorded_at {
                return false;
            }

            if hot_achievement.client_id == bloop.client_id {
                awarded.insert(hot_achievement.id);
            }

            true
        });
    }

    fn activate_hot_achievements(
        &mut self,
        bloop: &Bloop<Player>,
        awarded_tracker: &AwardedTracker,
    ) {
        let Some(awarded) = awarded_tracker.for_player(bloop.player_id) else {
            return;
        };

        for achievement_id in awarded {
            let Some(achievement) = self.achievements.get(achievement_id) else {
                continue;
            };

            if let Some(hot_duration) = achievement.hot_duration {
                self.hot_achievements.push(HotAchievement::new(
                    *achievement_id,
                    bloop,
                    hot_duration,
                ));
            };
        }
    }

    async fn apply_awarded(&self, tracker: AwardedTracker) {
        let player_maps = self.player_registry.lock().await;
        let batch: AchievementAwardBatch = tracker.into();

        for player_awards in batch.players.iter() {
            let Some(player) = player_maps.by_id.get(&player_awards.player_id) else {
                warn!(
                    "Player ID {} has been removed from player map",
                    player_awards.player_id
                );
                continue;
            };

            let mut player = player.write().unwrap();

            for achievement_id in player_awards.achievement_ids.iter() {
                player.add_awarded_achievement(*achievement_id, batch.awarded_at);
            }
        }

        let _ = self.event_tx.send(Event::AchievementsAwarded(batch));
    }

    fn handle_retrieve_audio(&self, id: Uuid, response: oneshot::Sender<StandardResponse>) {
        let Some(achievement) = self.achievements.get(&id) else {
            let _ = response.send(StandardResponse::Error(ErrorResponse::UnknownAchievementId));
            return;
        };

        let path = self.audio_base_path.join(&achievement.audio_path);

        tokio::spawn(async move {
            let mut file = match File::open(&path).await {
                Ok(file) => file,
                Err(err) => {
                    warn!("Failed to open file {:?}: {:?}", &path, err);
                    let _ =
                        response.send(StandardResponse::Error(ErrorResponse::UnknownAchievementId));
                    return;
                }
            };

            let mut data = vec![];

            if let Err(err) = file.read_to_end(&mut data).await {
                warn!("Failed to read file {:?}: {:?}", &path, err);
                let _ = response.send(StandardResponse::Error(ErrorResponse::UnknownAchievementId));
                return;
            }

            let _ = response.send(StandardResponse::Audio { data });
        });
    }

    fn handle_preload_check(
        &self,
        state_hash: Option<Vec<u8>>,
        response: oneshot::Sender<StandardResponse>,
    ) {
        if let Some(ref state_hash) = state_hash {
            if state_hash.as_slice() == self.global_hash.as_ref() {
                let _ = response.send(StandardResponse::PreloadStateMatch);
                return;
            }
        }

        let _ = response.send(StandardResponse::PreloadStateMismatch {
            state_hash: self.global_hash.to_vec(),
            achievements: self
                .achievements
                .values()
                .map(|achievement| AchievementResponse {
                    id: achievement.definition.id,
                    audio_hash: *self.audio_hashes.get(&achievement.definition.id).unwrap(),
                })
                .collect(),
        });
    }
}

#[cfg(feature = "tokio-graceful-shutdown")]
#[derive(Debug, Error)]
pub enum NeverError {}

#[cfg(feature = "tokio-graceful-shutdown")]
#[async_trait]
impl<Player, Metadata, Trigger> IntoSubsystem<NeverError> for Engine<Player, Metadata, Trigger>
where
    Player: PlayerInfo + PlayerMutator + Send + Sync + 'static,
    Metadata: Send + Sync + 'static,
    Trigger: Copy + PartialEq + Eq + Debug + Send + Sync + 'static,
{
    async fn run(mut self, subsys: SubsystemHandle) -> Result<(), NeverError> {
        let _ = self.process_requests().cancel_on_shutdown(&subsys).await;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum BuilderError {
    #[error("missing field: {0}")]
    MissingField(&'static str),
}

#[derive(Default)]
pub struct EngineBuilder<Player, Metadata = (), Trigger = ()>
where
    Player: PlayerInfo + PlayerMutator,
    Metadata: Default,
    Trigger: Copy + PartialEq + Eq + Debug,
{
    bloops: Vec<Bloop<Player>>,
    achievements: Vec<Achievement<Player, Metadata, Trigger>>,
    bloop_retention: Option<Duration>,
    audio_base_path: Option<PathBuf>,
    player_registry: Option<Arc<Mutex<PlayerRegistry<Player>>>>,
    metadata: Option<Arc<Mutex<Metadata>>>,
    trigger_registry: Option<TriggerRegistry<Trigger>>,
    network_rx: Option<mpsc::Receiver<(EngineRequest, oneshot::Sender<StandardResponse>)>>,
    event_tx: Option<broadcast::Sender<Event>>,
}

impl<Player, Metadata, Trigger> EngineBuilder<Player, Metadata, Trigger>
where
    Player: PlayerInfo + PlayerMutator,
    Metadata: Default,
    Trigger: Copy + PartialEq + Eq + Debug,
{
    pub fn new() -> Self {
        Self {
            bloops: Vec::new(),
            achievements: Vec::new(),
            bloop_retention: None,
            audio_base_path: None,
            player_registry: None,
            metadata: None,
            trigger_registry: None,
            network_rx: None,
            event_tx: None,
        }
    }

    pub fn bloops(mut self, bloops: Vec<Bloop<Player>>) -> Self {
        self.bloops = bloops;
        self
    }

    pub fn achievements(
        mut self,
        achievements: Vec<Achievement<Player, Metadata, Trigger>>,
    ) -> Self {
        self.achievements = achievements;
        self
    }

    pub fn bloop_retention(mut self, retention: Duration) -> Self {
        self.bloop_retention = Some(retention);
        self
    }

    pub fn audio_base_path<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.audio_base_path = Some(path.into());
        self
    }

    pub fn player_registry(mut self, registry: Arc<Mutex<PlayerRegistry<Player>>>) -> Self {
        self.player_registry = Some(registry);
        self
    }

    pub fn metadata(mut self, metadata: Arc<Mutex<Metadata>>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn trigger_registry(mut self, registry: TriggerRegistry<Trigger>) -> Self {
        self.trigger_registry = Some(registry);
        self
    }

    pub fn network_rx(
        mut self,
        rx: mpsc::Receiver<(EngineRequest, oneshot::Sender<StandardResponse>)>,
    ) -> Self {
        self.network_rx = Some(rx);
        self
    }

    pub fn event_tx(mut self, tx: broadcast::Sender<Event>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Consumes the builder and constructs the Engine.
    pub fn build(self) -> Result<Engine<Player, Metadata, Trigger>, BuilderError> {
        let bloop_retention = self
            .bloop_retention
            .ok_or(BuilderError::MissingField("bloop_retention"))?;
        let audio_base_path = self
            .audio_base_path
            .ok_or(BuilderError::MissingField("audio_base_path"))?;
        let player_registry = self
            .player_registry
            .ok_or(BuilderError::MissingField("player_registry"))?;
        let network_rx = self
            .network_rx
            .ok_or(BuilderError::MissingField("network_rx"))?;
        let event_tx = self
            .event_tx
            .ok_or(BuilderError::MissingField("event_tx"))?;

        let (audio_hashes, global_hash) =
            collect_audio_hashes(&audio_base_path, &self.achievements);
        let bloop_provider = BloopProvider::with_bloops(bloop_retention, self.bloops);
        let achievements: HashMap<Uuid, Achievement<Player, Metadata, Trigger>> = self
            .achievements
            .into_iter()
            .map(|a| (a.definition.id, a))
            .collect();
        let metadata = self
            .metadata
            .unwrap_or_else(|| Arc::new(Mutex::new(Default::default())));
        let trigger_registry = self
            .trigger_registry
            .unwrap_or_else(|| TriggerRegistry::new(HashMap::new()));
        let hot_achievements = Vec::new();

        Ok(Engine {
            bloop_provider,
            achievements,
            audio_base_path,
            audio_hashes,
            global_hash,
            player_registry,
            metadata,
            trigger_registry,
            hot_achievements,
            network_rx,
            event_tx,
        })
    }
}

fn collect_audio_hashes<Player, Metadata, Trigger: Copy + PartialEq + Eq + Debug>(
    audio_base_path: &Path,
    achievements: &[Achievement<Player, Metadata, Trigger>],
) -> (HashMap<Uuid, Digest>, Digest) {
    let audio_hashes: HashMap<Uuid, Digest> = achievements
        .iter()
        .map(|achievement| {
            let path = audio_base_path.join(&achievement.audio_path);
            let file_content = fs::read(path).unwrap_or_default();
            let digest = md5::compute(file_content);
            (achievement.definition.id, digest)
        })
        .collect();

    let mut entries: Vec<_> = audio_hashes.iter().collect();
    entries.sort_by_key(|(id, _)| *id);
    let mut hash_input = Vec::with_capacity(entries.len() * 32);

    for (id, hash) in entries {
        hash_input.extend(id.as_bytes());
        hash_input.extend_from_slice(&hash.0);
    }

    let global_hash = md5::compute(hash_input);

    (audio_hashes, global_hash)
}
