use crate::achievement::AchievementContext;
use crate::bloop::{Bloop, BloopProvider};
use crate::evaluator::registration_number::RegistrationNumberProvider;
use crate::nfc_uid::NfcUid;
use crate::player::{PlayerInfo, PlayerMutator};
use crate::trigger::TriggerRegistry;
use chrono::DateTime;
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
use tokio::time::Instant;
use uuid::Uuid;

/// A utility struct providing a custom UTC clock based on a fixed base time
/// and a monotonic timer to ensure consistent time progression.
pub struct Utc;

static BASE_INSTANT: OnceLock<Instant> = OnceLock::new();
static BASE_UTC: OnceLock<DateTime<chrono::Utc>> = OnceLock::new();

/// Returns the current time as `DateTime<Utc>`, computed from a fixed base UTC
/// time plus the elapsed monotonic duration since program start.
impl Utc {
    pub fn now() -> DateTime<chrono::Utc> {
        let base_instant = *BASE_INSTANT.get_or_init(Instant::now);
        let base_utc = *BASE_UTC.get_or_init(|| DateTime::from_timestamp(0, 0).unwrap());

        let elapsed = Instant::now()
            .checked_duration_since(base_instant)
            .unwrap_or(Duration::ZERO);

        base_utc + chrono::Duration::from_std(elapsed).unwrap()
    }
}

/// Builder for creating test/mock players with configurable properties.
#[derive(Default, Debug)]
pub struct MockPlayerBuilder {
    nfc_uid: NfcUid,
    bloops_count: usize,
    registration_number: usize,
}

impl MockPlayerBuilder {
    /// Creates a new builder with default values.
    pub fn new() -> Self {
        Self {
            nfc_uid: NfcUid::default(),
            bloops_count: 0,
            registration_number: 0,
        }
    }

    /// Sets the NFC UID of the mock player.
    pub fn nfc_uid(mut self, nfc_uid: NfcUid) -> Self {
        self.nfc_uid = nfc_uid;
        self
    }

    /// Sets the initial bloops count for the mock player.
    pub fn bloops_count(mut self, bloops_count: usize) -> Self {
        self.bloops_count = bloops_count;
        self
    }

    /// Sets the registration number of the mock player.
    pub fn registration_number(mut self, registration_number: usize) -> Self {
        self.registration_number = registration_number;
        self
    }

    /// Builds the [`MockPlayer`] wrapped in `Arc<RwLock>` along with its UUID.
    pub fn build(self) -> (Arc<RwLock<MockPlayer>>, Uuid) {
        let id = Uuid::new_v4();

        (
            Arc::new(RwLock::new(MockPlayer {
                id,
                nfc_uid: self.nfc_uid,
                name: "test".to_string(),
                bloops_count: self.bloops_count,
                awarded: HashMap::new(),
                registration_number: self.registration_number,
            })),
            id,
        )
    }
}

/// Represents a mock implementation of a player.
///
/// Holds player details and state useful for testing achievement logic.
#[derive(Debug)]
pub struct MockPlayer {
    pub id: Uuid,
    pub nfc_uid: NfcUid,
    pub name: String,
    pub bloops_count: usize,
    pub awarded: HashMap<Uuid, DateTime<chrono::Utc>>,
    pub registration_number: usize,
}

impl MockPlayer {
    /// Returns a new [`MockPlayerBuilder`] to construct a mock player instance.
    pub fn builder() -> MockPlayerBuilder {
        MockPlayerBuilder::new()
    }
}

impl PlayerInfo for MockPlayer {
    fn id(&self) -> Uuid {
        self.id
    }

    fn nfc_uid(&self) -> NfcUid {
        self.nfc_uid
    }

    fn total_bloops(&self) -> usize {
        self.bloops_count
    }

    fn awarded_achievements(&self) -> &HashMap<Uuid, DateTime<chrono::Utc>> {
        &self.awarded
    }
}

impl PlayerMutator for MockPlayer {
    fn increment_bloops(&mut self) {
        self.bloops_count += 1;
    }

    fn add_awarded_achievement(&mut self, achievement_id: Uuid, awarded_at: DateTime<chrono::Utc>) {
        self.awarded.insert(achievement_id, awarded_at);
    }
}

impl RegistrationNumberProvider for MockPlayer {
    fn registration_number(&self) -> usize {
        self.registration_number
    }
}

/// Builder pattern struct for assembling an achievement test context.
///
/// Supports configuration of current bloop, player state, bloop provider,
/// and trigger registry for testing achievement logic.
#[derive(Debug)]
pub struct TestCtxBuilder<Player, State = (), Trigger = ()> {
    pub current_bloop: Bloop<Player>,
    pub state: State,
    pub bloop_provider: BloopProvider<Player>,
    pub trigger_registry: TriggerRegistry<Trigger>,
}

impl<Player> TestCtxBuilder<Player, (), ()> {
    /// Creates a new test context builder with a current bloop and default state.
    pub fn new(current_bloop: Bloop<Player>) -> Self {
        Self {
            current_bloop,
            state: (),
            trigger_registry: TriggerRegistry::new(HashMap::new()),
            bloop_provider: BloopProvider::new(Duration::from_secs(1800)),
        }
    }
}

impl<Player, State, Trigger> TestCtxBuilder<Player, State, Trigger> {
    /// Sets the bloops to be used in the context's bloop provider.
    pub fn bloops(mut self, bloops: Vec<Bloop<Player>>) -> Self {
        self.bloop_provider = BloopProvider::with_bloops(Duration::from_secs(1800), bloops);
        self
    }

    /// Sets the custom state for the context.
    pub fn state<T: Default>(self, state: T) -> TestCtxBuilder<Player, T, Trigger> {
        TestCtxBuilder {
            current_bloop: self.current_bloop,
            state,
            bloop_provider: self.bloop_provider,
            trigger_registry: self.trigger_registry,
        }
    }

    /// Sets the trigger registry for the context.
    pub fn trigger_registry<T>(
        self,
        trigger_registry: TriggerRegistry<T>,
    ) -> TestCtxBuilder<Player, State, T> {
        TestCtxBuilder {
            current_bloop: self.current_bloop,
            state: self.state,
            bloop_provider: self.bloop_provider,
            trigger_registry,
        }
    }

    /// Builds the achievement context from the configured components.
    pub fn build(&mut self) -> AchievementContext<'_, Player, State, Trigger> {
        AchievementContext::new(
            &self.current_bloop,
            &self.bloop_provider,
            &self.state,
            &mut self.trigger_registry,
        )
    }
}
