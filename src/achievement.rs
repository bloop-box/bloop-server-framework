use crate::bloop::{Bloop, BloopProvider, bloops_for_player, bloops_since};
use crate::evaluator::Evaluator;
use crate::player::PlayerInfo;
use crate::trigger::TriggerRegistry;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use uuid::Uuid;

/// Metadata describing a possible achievement that can be awarded to players.
#[derive(Debug, Serialize)]
pub struct AchievementDefinition {
    /// Unique ID of the achievement.
    pub id: Uuid,
    /// Display title of the achievement.
    pub title: String,
    /// Detailed description shown to players.
    pub description: String,
    /// Number of points the achievement is worth.
    pub points: u32,
    /// Whether the achievement is hidden until awarded.
    pub is_hidden: bool,
}

/// Defines a runtime-configured achievement including logic for evaluation.
#[derive(Debug)]
pub struct Achievement<Player, Metadata, Trigger>
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    /// Metadata definition of the achievement.
    pub definition: AchievementDefinition,
    /// Path to audio played when achievement is awarded.
    pub audio_path: PathBuf,
    /// Evaluation logic for determining if the achievement should be awarded.
    pub evaluator: Evaluator<Player, Metadata, Trigger>,
    /// Optional duration for which the achievement remains "hot".
    pub hot_duration: Option<Duration>,
}

impl<Player, Metadata, Trigger> Achievement<Player, Metadata, Trigger>
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    pub fn new(
        definition: AchievementDefinition,
        audio_path: impl Into<PathBuf>,
        evaluator: Evaluator<Player, Metadata, Trigger>,
    ) -> Self {
        Self {
            definition,
            audio_path: audio_path.into(),
            evaluator,
            hot_duration: None,
        }
    }

    pub fn new_hot(
        definition: AchievementDefinition,
        audio_path: impl Into<PathBuf>,
        evaluator: Evaluator<Player, Metadata, Trigger>,
        hot_duration: Duration,
    ) -> Self {
        Self {
            definition,
            audio_path: audio_path.into(),
            evaluator,
            hot_duration: Some(hot_duration),
        }
    }

    pub fn evaluate(&self, ctx: &AchievementContext<Player, Metadata, Trigger>) {
        match &self.evaluator {
            Evaluator::Single(evaluator) => {
                if evaluator.evaluate(ctx) {
                    ctx.award_achievement(self.definition.id, ctx.current_bloop.player_id);
                }
            }
            Evaluator::Multi(evaluator) => {
                if let Some(player_ids) = evaluator.evaluate(ctx) {
                    for player_id in player_ids {
                        ctx.award_achievement(self.definition.id, player_id);
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayerAchievementAwards {
    pub player_id: Uuid,
    pub achievement_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AchievementAwardBatch {
    pub awarded_at: DateTime<Utc>,
    pub players: Vec<PlayerAchievementAwards>,
}

impl From<AwardedTracker> for AchievementAwardBatch {
    fn from(tracker: AwardedTracker) -> Self {
        Self {
            awarded_at: tracker.awarded_at,
            players: tracker
                .awarded
                .into_iter()
                .map(|(player_id, achievements)| PlayerAchievementAwards {
                    player_id,
                    achievement_ids: achievements.into_iter().collect(),
                })
                .collect(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct AwardedTracker {
    awarded_at: DateTime<Utc>,
    awarded: HashMap<Uuid, HashSet<Uuid>>,
}

impl AwardedTracker {
    fn new<Player>(current_bloop: &Bloop<Player>) -> Self {
        Self {
            awarded_at: current_bloop.recorded_at,
            awarded: HashMap::new(),
        }
    }

    pub fn add(&mut self, achievement_id: Uuid, player_id: Uuid) {
        self.awarded
            .entry(player_id)
            .or_default()
            .insert(achievement_id);
    }

    pub fn for_player(&self, player_id: Uuid) -> Option<&HashSet<Uuid>> {
        self.awarded.get(&player_id)
    }

    pub fn for_player_mut(&mut self, player_id: Uuid) -> &mut HashSet<Uuid> {
        self.awarded.entry(player_id).or_default()
    }

    pub fn remove_duplicates<Player: PlayerInfo>(
        &mut self,
        players: &HashMap<Uuid, Arc<RwLock<Player>>>,
    ) {
        self.awarded.retain(|player_id, achievements| {
            let Some(player) = players.get(player_id) else {
                return false;
            };

            let player = player.read().unwrap();
            let awarded = player.awarded_achievements();
            achievements.retain(|achievement_id| !awarded.contains_key(achievement_id));
            !achievements.is_empty()
        });
    }
}

/// Context used when evaluating and awarding achievements for a specific bloop.
#[derive(Debug)]
pub struct AchievementContext<'a, Player, Metadata, Trigger>
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    /// The bloop currently being evaluated.
    pub current_bloop: &'a Bloop<Player>,
    /// Provides access to bloop data for the clients and players.
    pub bloop_provider: &'a BloopProvider<Player>,
    /// Additional evaluation-specific metadata.
    pub metadata: &'a Metadata,
    /// Registry of all active triggers.
    trigger_registry: Mutex<&'a mut TriggerRegistry<Trigger>>,
    /// Tracks achievements awarded during this evaluation.
    awarded_tracker: Mutex<AwardedTracker>,
}

impl<'a, Player, Metadata, Trigger> AchievementContext<'a, Player, Metadata, Trigger>
where
    Player: PlayerInfo,
    Trigger: Copy + PartialEq + Eq + Debug,
{
    pub fn new(
        current_bloop: &'a Bloop<Player>,
        bloop_provider: &'a BloopProvider<Player>,
        metadata: &'a Metadata,
        trigger_registry: &'a mut TriggerRegistry<Trigger>,
    ) -> Self {
        AchievementContext {
            current_bloop,
            bloop_provider,
            metadata,
            trigger_registry: Mutex::new(trigger_registry),
            awarded_tracker: Mutex::new(AwardedTracker::new(current_bloop)),
        }
    }
}

impl<Player, Metadata, Trigger> AchievementContext<'_, Player, Metadata, Trigger>
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    pub fn global_bloops(&self) -> impl Iterator<Item = &Arc<Bloop<Player>>> {
        self.bloop_provider.global().iter()
    }

    /// Returns the bloop collection for the current bloop's client.
    #[inline]
    pub fn client_bloops(&self) -> impl Iterator<Item = &Arc<Bloop<Player>>> {
        self.bloop_provider
            .for_client(&self.current_bloop.client_id)
            .iter()
    }

    /// Returns a filter for bloop collections for the current bloop's player.
    #[inline]
    pub fn filter_current_player(&self) -> impl Fn(&&Arc<Bloop<Player>>) -> bool {
        bloops_for_player(self.current_bloop.player_id)
    }

    /// Returns a filter for bloop collections for a given duration.
    #[inline]
    pub fn filter_within_window(
        &self,
        duration: Duration,
    ) -> impl Fn(&&Arc<Bloop<Player>>) -> bool {
        let since = self.current_bloop.recorded_at - duration;
        bloops_since(since)
    }

    /// Returns whether a given trigger is active right now.
    pub fn has_trigger(&self, trigger: Trigger) -> bool {
        let mut guard = self.trigger_registry.lock().unwrap();
        let registry: &mut TriggerRegistry<_> = *guard;

        registry.check_active_trigger(
            trigger,
            &self.current_bloop.client_id,
            self.current_bloop.recorded_at,
        )
    }

    /// Records that a given achievement should be awarded to a player.
    pub fn award_achievement(&self, achievement_id: Uuid, player_id: Uuid) {
        self.awarded_tracker
            .lock()
            .unwrap()
            .add(achievement_id, player_id);
    }

    /// Consumes the context and returns all awarded achievements.
    pub(crate) fn take_awarded(self) -> AwardedTracker {
        self.awarded_tracker.into_inner().unwrap()
    }
}
