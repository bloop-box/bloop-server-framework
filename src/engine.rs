use crate::achievement::{Achievement, AchievementAwardBatch, AchievementContext, AwardedTracker};
use crate::bloop::{Bloop, BloopProvider, ProcessedBloop, bloops_since};
use crate::event::Event;
use crate::message::{AchievementRecord, DataHash, ErrorResponse, ServerMessage};
use crate::nfc_uid::NfcUid;
use crate::player::{PlayerInfo, PlayerMutator, PlayerRegistry};
use crate::trigger::TriggerRegistry;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
#[cfg(feature = "tokio-graceful-shutdown")]
use tokio_graceful_shutdown::{FutureExt, IntoSubsystem, SubsystemHandle};
use tracing::{info, instrument, warn};
use uuid::Uuid;

#[derive(Debug)]
pub enum EngineRequest {
    Bloop { client_id: String, nfc_uid: NfcUid },
    RetrieveAudio { id: Uuid },
    PreloadCheck { manifest_hash: Option<DataHash> },
}

#[derive(Debug, Deserialize)]
pub struct Throttle {
    max_bloops: usize,
    threshold: Duration,
}

impl Throttle {
    pub fn new(max_bloops: usize, threshold: Duration) -> Self {
        Self {
            max_bloops,
            threshold,
        }
    }
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

pub struct Engine<Metadata, Player, State, Trigger>
where
    Player: PlayerInfo + PlayerMutator,
    Trigger: Copy,
{
    bloop_provider: BloopProvider<Player>,
    achievements: HashMap<Uuid, Achievement<Metadata, Player, State, Trigger>>,
    audio_base_path: PathBuf,
    audio_manifest_hash: DataHash,
    player_registry: Arc<Mutex<PlayerRegistry<Player>>>,
    state: Arc<Mutex<State>>,
    trigger_registry: TriggerRegistry<Trigger>,
    hot_achievements: Vec<HotAchievement>,
    network_rx: mpsc::Receiver<(EngineRequest, oneshot::Sender<ServerMessage>)>,
    event_tx: broadcast::Sender<Event>,
    throttle: Option<Throttle>,
}

impl<Metadata, Player, State, Trigger> Engine<Metadata, Player, State, Trigger>
where
    Player: PlayerInfo + PlayerMutator,
    Trigger: Copy,
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
                EngineRequest::PreloadCheck { manifest_hash } => {
                    self.handle_preload_check(manifest_hash, response);
                }
            }
        }
    }

    #[instrument(skip(self, response))]
    async fn handle_bloop(
        &mut self,
        nfc_uid: NfcUid,
        client_id: String,
        response: oneshot::Sender<ServerMessage>,
    ) {
        if self
            .trigger_registry
            .try_activate_trigger(nfc_uid, &client_id)
        {
            let _ = response.send(ServerMessage::BloopAccepted {
                achievements: Vec::new(),
            });
            return;
        }

        let player = {
            let player_registry = self.player_registry.lock().await;
            let Some(player) = player_registry.get_by_nfc_uid(nfc_uid) else {
                let _ = response.send(ServerMessage::Error(ErrorResponse::UnknownNfcUid));
                return;
            };
            player
        };

        if let Some(throttle) = self.throttle.as_ref() {
            let player_id = player.read().unwrap().id();

            let recent_bloops = self
                .bloop_provider
                .for_client(&client_id)
                .iter()
                .filter(bloops_since(Utc::now() - throttle.threshold))
                .take(throttle.max_bloops)
                .collect::<Vec<_>>();

            if recent_bloops
                .iter()
                .all(|bloop| bloop.player_id == player_id)
                && recent_bloops.len() == throttle.max_bloops
            {
                let _ = response.send(ServerMessage::Error(ErrorResponse::NfcUidThrottled));
                return;
            }
        }

        player.write().unwrap().increment_bloops();
        let bloop = Bloop::new(player.clone(), client_id, Utc::now());

        let mut awarded_tracker = self.evaluate_achievements(&bloop).await;
        self.activate_hot_achievements(&bloop, &awarded_tracker);
        self.inject_hot_achievements(&bloop, &mut awarded_tracker);

        let player_registry = self.player_registry.lock().await;
        awarded_tracker.remove_duplicates(player_registry);

        let achievement_ids: Vec<Uuid> = awarded_tracker
            .for_player(bloop.player_id)
            .map_or_else(Vec::new, |set| set.iter().cloned().collect());

        self.apply_awarded(awarded_tracker).await;

        let processed_bloop: ProcessedBloop = (&bloop).into();
        self.bloop_provider.add(Arc::new(bloop));
        let _ = self.event_tx.send(Event::BloopProcessed(processed_bloop));

        let _ = response.send(ServerMessage::BloopAccepted {
            achievements: achievement_ids
                .into_iter()
                .map(|id| AchievementRecord {
                    id,
                    audio_file_hash: self
                        .achievements
                        .get(&id)
                        .unwrap()
                        .audio_file
                        .resolve(&self.audio_base_path)
                        .as_ref()
                        .map(|file| file.hash),
                })
                .collect(),
        });
    }

    async fn evaluate_achievements(&mut self, bloop: &Bloop<Player>) -> AwardedTracker {
        let previous_awarded: HashSet<Uuid> = {
            let player = bloop.player();
            player.awarded_achievements().keys().cloned().collect()
        };
        let state = self.state.lock().await;
        let ctx = AchievementContext::new(
            bloop,
            &self.bloop_provider,
            &*state,
            &mut self.trigger_registry,
        );

        for achievement in self.achievements.values() {
            if !previous_awarded.contains(&achievement.id) {
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
        let mut player_registry = self.player_registry.lock().await;
        let batch: AchievementAwardBatch = tracker.into();

        for player_awards in batch.players.iter() {
            player_registry.mutate_by_id(player_awards.player_id, |player| {
                for achievement_id in player_awards.achievement_ids.iter() {
                    player.add_awarded_achievement(*achievement_id, batch.awarded_at);
                }
            });
        }

        let _ = self.event_tx.send(Event::AchievementsAwarded(batch));
    }

    #[instrument(skip(self, response))]
    fn handle_retrieve_audio(&self, id: Uuid, response: oneshot::Sender<ServerMessage>) {
        let Some(achievement) = self.achievements.get(&id) else {
            info!("Client requested unknown achievement: {}", id);
            let _ = response.send(ServerMessage::Error(ErrorResponse::AudioUnavailable));
            return;
        };

        let Some(audio_file) = achievement.audio_file.resolve(&self.audio_base_path) else {
            info!("Client requested audio for audio-less achievement: {}", id);
            let _ = response.send(ServerMessage::Error(ErrorResponse::AudioUnavailable));
            return;
        };

        let path = audio_file.path.clone();

        tokio::spawn(async move {
            let mut file = match File::open(&path).await {
                Ok(file) => file,
                Err(err) => {
                    warn!("Failed to open file {:?}: {:?}", &path, err);
                    let _ = response.send(ServerMessage::Error(ErrorResponse::AudioUnavailable));
                    return;
                }
            };

            let mut data = vec![];

            if let Err(err) = file.read_to_end(&mut data).await {
                warn!("Failed to read file {:?}: {:?}", &path, err);
                let _ = response.send(ServerMessage::Error(ErrorResponse::AudioUnavailable));
                return;
            }

            let _ = response.send(ServerMessage::AudioData { data });
        });
    }

    #[instrument(skip(self, response))]
    fn handle_preload_check(
        &self,
        manifest_hash: Option<DataHash>,
        response: oneshot::Sender<ServerMessage>,
    ) {
        if let Some(manifest_hash) = manifest_hash
            && manifest_hash == self.audio_manifest_hash
        {
            let _ = response.send(ServerMessage::PreloadMatch);
            return;
        }

        let _ = response.send(ServerMessage::PreloadMismatch {
            audio_manifest_hash: self.audio_manifest_hash,
            achievements: self
                .achievements
                .values()
                .map(|achievement| AchievementRecord {
                    id: achievement.id,
                    audio_file_hash: achievement
                        .audio_file
                        .resolve(&self.audio_base_path)
                        .as_ref()
                        .map(|file| file.hash),
                })
                .collect(),
        });
    }
}

#[cfg(feature = "tokio-graceful-shutdown")]
#[derive(Debug, Error)]
pub enum NeverError {}

#[cfg(feature = "tokio-graceful-shutdown")]
impl<Metadata, Player, State, Trigger> IntoSubsystem<NeverError>
    for Engine<Metadata, Player, State, Trigger>
where
    Metadata: Send + Sync + 'static,
    Player: PlayerInfo + PlayerMutator + Send + Sync + 'static,
    State: Send + Sync + 'static,
    Trigger: Copy + PartialEq + Eq + Debug + Send + Sync + 'static,
{
    async fn run(mut self, subsys: &mut SubsystemHandle) -> Result<(), NeverError> {
        let _ = self.process_requests().cancel_on_shutdown(subsys).await;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum BuilderError {
    #[error("missing field: {0}")]
    MissingField(&'static str),
}

#[derive(Debug, Default)]
pub struct EngineBuilder<Player, State = (), Trigger = (), Metadata = ()>
where
    Player: PlayerInfo + PlayerMutator,
    Trigger: Copy,
{
    bloops: Vec<Bloop<Player>>,
    achievements: Vec<Achievement<Metadata, Player, State, Trigger>>,
    bloop_retention: Option<Duration>,
    audio_base_path: Option<PathBuf>,
    player_registry: Option<Arc<Mutex<PlayerRegistry<Player>>>>,
    state: Option<Arc<Mutex<State>>>,
    trigger_registry: Option<TriggerRegistry<Trigger>>,
    network_rx: Option<mpsc::Receiver<(EngineRequest, oneshot::Sender<ServerMessage>)>>,
    event_tx: Option<broadcast::Sender<Event>>,
    throttle: Option<Throttle>,
}

impl<Player, State, Trigger, Metadata> EngineBuilder<Player, State, Trigger, Metadata>
where
    Player: PlayerInfo + PlayerMutator,
    State: Default,
    Trigger: Copy + PartialEq + Eq + Debug,
{
    pub fn new() -> Self {
        Self {
            bloops: Vec::new(),
            achievements: Vec::new(),
            bloop_retention: None,
            audio_base_path: None,
            player_registry: None,
            state: None,
            trigger_registry: None,
            network_rx: None,
            event_tx: None,
            throttle: None,
        }
    }

    pub fn bloops(mut self, bloops: Vec<Bloop<Player>>) -> Self {
        self.bloops = bloops;
        self
    }

    pub fn achievements(
        mut self,
        achievements: Vec<Achievement<Metadata, Player, State, Trigger>>,
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

    pub fn state(mut self, state: Arc<Mutex<State>>) -> Self {
        self.state = Some(state);
        self
    }

    pub fn trigger_registry(mut self, registry: TriggerRegistry<Trigger>) -> Self {
        self.trigger_registry = Some(registry);
        self
    }

    pub fn network_rx(
        mut self,
        rx: mpsc::Receiver<(EngineRequest, oneshot::Sender<ServerMessage>)>,
    ) -> Self {
        self.network_rx = Some(rx);
        self
    }

    pub fn event_tx(mut self, tx: broadcast::Sender<Event>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    pub fn throttle(mut self, throttle: Throttle) -> Self {
        self.throttle = Some(throttle);
        self
    }

    /// Consumes the builder and constructs the Engine.
    pub fn build(self) -> Result<Engine<Metadata, Player, State, Trigger>, BuilderError> {
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

        let audio_manifest_hash = calculate_manifest_hash(&audio_base_path, &self.achievements);
        let bloop_provider = BloopProvider::with_bloops(bloop_retention, self.bloops);
        let achievements: HashMap<Uuid, Achievement<Metadata, Player, State, Trigger>> =
            self.achievements.into_iter().map(|a| (a.id, a)).collect();
        let state = self
            .state
            .unwrap_or_else(|| Arc::new(Mutex::new(Default::default())));
        let trigger_registry = self
            .trigger_registry
            .unwrap_or_else(|| TriggerRegistry::new(HashMap::new()));
        let hot_achievements = Vec::new();

        Ok(Engine {
            bloop_provider,
            achievements,
            audio_base_path,
            audio_manifest_hash,
            player_registry,
            state,
            trigger_registry,
            hot_achievements,
            network_rx,
            event_tx,
            throttle: self.throttle,
        })
    }
}

fn calculate_manifest_hash<Metadata, Player, State, Trigger>(
    audio_base_path: &Path,
    achievements: &[Achievement<Metadata, Player, State, Trigger>],
) -> DataHash {
    let audio_file_hashes: HashMap<Uuid, DataHash> = achievements
        .iter()
        .filter_map(|achievement| {
            achievement
                .audio_file
                .resolve(audio_base_path)
                .as_ref()
                .map(|file| (achievement.id, file.hash))
        })
        .collect();

    let mut entries: Vec<_> = audio_file_hashes.iter().collect();
    entries.sort_by_key(|(id, _)| *id);
    let mut hash_input = Vec::with_capacity(entries.len() * 32);

    for (id, hash) in entries {
        hash_input.extend(id.as_bytes());
        hash_input.extend_from_slice(hash.as_bytes());
    }

    let manifest_hash = md5::compute(hash_input);
    manifest_hash.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::MockPlayer;
    use crate::trigger::{TriggerOccurrence, TriggerSpec};

    fn build_test_engine() -> Engine<(), MockPlayer, (), ()> {
        let player_registry = Arc::new(Mutex::new(PlayerRegistry::new(vec![])));
        let trigger_registry = TriggerRegistry::new(HashMap::new());

        EngineBuilder::<MockPlayer, (), (), ()>::new()
            .bloop_retention(Duration::from_secs(3600))
            .audio_base_path("./audio")
            .player_registry(player_registry)
            .trigger_registry(trigger_registry)
            .network_rx(mpsc::channel(1).1)
            .event_tx(broadcast::channel(16).0)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn handle_bloop_rejects_unknown_nfc_uid() {
        let mut engine = build_test_engine();
        let unknown_nfc_uid = NfcUid::default();
        let client_id = "test-client".to_string();

        let (resp_tx, resp_rx) = oneshot::channel();
        engine
            .handle_bloop(unknown_nfc_uid, client_id.clone(), resp_tx)
            .await;

        let response = resp_rx.await.unwrap();
        match response {
            ServerMessage::Error(err) => {
                assert!(matches!(err, ErrorResponse::UnknownNfcUid));
            }
            _ => panic!("Expected Error response for unknown NFC UID"),
        }
    }

    #[tokio::test]
    async fn handle_bloop_accepts_known_player() {
        let mut engine = build_test_engine();
        let nfc_uid = NfcUid::default();
        let client_id = "test-client".to_string();

        {
            let mut registry = engine.player_registry.lock().await;
            let (player, _) = MockPlayer::builder().nfc_uid(nfc_uid).build();
            registry.add(Arc::into_inner(player).unwrap().into_inner().unwrap());
        }

        let (resp_tx, resp_rx) = oneshot::channel();
        engine
            .handle_bloop(nfc_uid, client_id.clone(), resp_tx)
            .await;

        let response = resp_rx.await.unwrap();
        match response {
            ServerMessage::BloopAccepted { achievements } => {
                assert!(achievements.is_empty());
            }
            _ => panic!("Expected BloopAccepted response"),
        }
    }

    #[tokio::test]
    async fn handle_bloop_activates_trigger_and_responds() {
        let mut trigger_registry = HashMap::new();
        trigger_registry.insert(
            NfcUid::default(),
            TriggerSpec {
                trigger: (),
                global: false,
                occurrence: TriggerOccurrence::Once,
            },
        );
        let trigger_registry = TriggerRegistry::new(trigger_registry);

        let player_registry = Arc::new(Mutex::new(PlayerRegistry::new(vec![])));

        let (_tx, rx) = mpsc::channel(1);
        let (evt_tx, _) = broadcast::channel(16);

        let mut engine = EngineBuilder::<MockPlayer, (), (), ()>::new()
            .bloop_retention(Duration::from_secs(3600))
            .audio_base_path("./audio")
            .player_registry(player_registry)
            .trigger_registry(trigger_registry)
            .network_rx(rx)
            .event_tx(evt_tx)
            .build()
            .unwrap();

        let nfc_uid = NfcUid::default();
        let client_id = "client".to_string();
        let (resp_tx, resp_rx) = oneshot::channel();

        engine
            .handle_bloop(nfc_uid, client_id.clone(), resp_tx)
            .await;

        let response = resp_rx.await.unwrap();
        match response {
            ServerMessage::BloopAccepted { achievements } => {
                assert!(achievements.is_empty());
            }
            _ => panic!("Expected BloopAccepted response"),
        }

        assert!(
            engine
                .trigger_registry
                .check_active_trigger((), "client", Utc::now())
        );
    }

    #[tokio::test]
    async fn handle_bloop_respects_throttling() {
        let mut engine = build_test_engine();
        let nfc_uid = NfcUid::default();
        let client_id = "test-client".to_string();

        {
            let mut registry = engine.player_registry.lock().await;
            let (player, _) = MockPlayer::builder().nfc_uid(nfc_uid).build();
            registry.add(Arc::into_inner(player).unwrap().into_inner().unwrap());
        }

        engine.throttle = Some(Throttle::new(1, Duration::from_secs(10)));

        let bloop = Bloop::new(
            engine
                .player_registry
                .lock()
                .await
                .get_by_nfc_uid(nfc_uid)
                .unwrap(),
            client_id.clone(),
            Utc::now(),
        );
        engine.bloop_provider.add(Arc::new(bloop));

        let (resp_tx, resp_rx) = oneshot::channel();
        engine
            .handle_bloop(nfc_uid, client_id.clone(), resp_tx)
            .await;

        let response = resp_rx.await.unwrap();
        match response {
            ServerMessage::Error(err) => {
                assert!(matches!(err, ErrorResponse::NfcUidThrottled));
            }
            _ => panic!("Expected throttling error"),
        }
    }
}
