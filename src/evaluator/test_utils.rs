use crate::achievement::AchievementContext;
use crate::bloop::{Bloop, BloopProvider};
use crate::evaluator::registration_number::RegistrationNumberProvider;
use crate::nfc_uid::NfcUid;
use crate::player::PlayerInfo;
use crate::trigger::TriggerRegistry;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

#[derive(Default)]
pub struct MockPlayerBuilder {
    bloops_count: usize,
    registration_number: usize,
}

impl MockPlayerBuilder {
    pub fn new() -> Self {
        Self {
            bloops_count: 0,
            registration_number: 0,
        }
    }

    pub fn bloops_count(mut self, bloops_count: usize) -> Self {
        self.bloops_count = bloops_count;
        self
    }

    pub fn registration_number(mut self, registration_number: usize) -> Self {
        self.registration_number = registration_number;
        self
    }

    pub fn build(self) -> (Arc<RwLock<MockPlayer>>, Uuid) {
        let id = Uuid::new_v4();

        (
            Arc::new(RwLock::new(MockPlayer {
                id,
                bloops_count: self.bloops_count,
                awarded: HashMap::new(),
                registration_number: self.registration_number,
            })),
            id,
        )
    }
}

#[derive(Debug)]
pub struct MockPlayer {
    pub id: Uuid,
    pub bloops_count: usize,
    pub awarded: HashMap<Uuid, DateTime<Utc>>,
    pub registration_number: usize,
}

impl MockPlayer {
    pub fn builder() -> MockPlayerBuilder {
        MockPlayerBuilder::new()
    }
}

impl PlayerInfo for MockPlayer {
    fn id(&self) -> Uuid {
        self.id
    }

    fn nfc_uid(&self) -> NfcUid {
        NfcUid::default()
    }

    fn total_bloops(&self) -> usize {
        self.bloops_count
    }

    fn awarded_achievements(&self) -> &HashMap<Uuid, DateTime<Utc>> {
        &self.awarded
    }
}

impl RegistrationNumberProvider for MockPlayer {
    fn registration_number(&self) -> usize {
        self.registration_number
    }
}

pub struct TestCtxBuilder<Player, Metadata = (), Trigger = ()>
where
    Metadata: Default,
    Trigger: Copy + PartialEq + Eq + Debug,
{
    pub current_bloop: Bloop<Player>,
    pub metadata: Metadata,
    pub bloop_provider: BloopProvider<Player>,
    pub trigger_registry: TriggerRegistry<Trigger>,
}

impl<Player> TestCtxBuilder<Player, (), ()> {
    pub fn new(current_bloop: Bloop<Player>) -> Self {
        Self {
            current_bloop,
            metadata: (),
            trigger_registry: TriggerRegistry::new(HashMap::new()),
            bloop_provider: BloopProvider::new(Duration::from_secs(1800)),
        }
    }
}

impl<Player, Metadata, Trigger> TestCtxBuilder<Player, Metadata, Trigger>
where
    Metadata: Default,
    Trigger: Copy + PartialEq + Eq + Debug,
{
    pub fn bloops(mut self, bloops: Vec<Bloop<Player>>) -> Self {
        self.bloop_provider = BloopProvider::with_bloops(Duration::from_secs(1800), bloops);
        self
    }

    pub fn metadata<T: Default>(self, metadata: T) -> TestCtxBuilder<Player, T, Trigger> {
        TestCtxBuilder {
            current_bloop: self.current_bloop,
            metadata,
            bloop_provider: self.bloop_provider,
            trigger_registry: self.trigger_registry,
        }
    }

    pub fn trigger_registry<T: Copy + PartialEq + Eq + Debug>(
        self,
        trigger_registry: TriggerRegistry<T>,
    ) -> TestCtxBuilder<Player, Metadata, T> {
        TestCtxBuilder {
            current_bloop: self.current_bloop,
            metadata: self.metadata,
            bloop_provider: self.bloop_provider,
            trigger_registry,
        }
    }

    pub fn build(&mut self) -> AchievementContext<Player, Metadata, Trigger> {
        AchievementContext::new(
            &self.current_bloop,
            &self.bloop_provider,
            &self.metadata,
            &mut self.trigger_registry,
        )
    }
}
