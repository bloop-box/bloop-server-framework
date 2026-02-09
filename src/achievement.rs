//! Provides types and logic for defining, evaluating, and awarding
//! achievements.
//!
//! This module includes builder types, achievement evaluation contexts,
//! award tracking, and associated errors.

use crate::bloop::{Bloop, BloopProvider, bloops_for_player, bloops_since};
use crate::evaluator::EvalResult;
use crate::evaluator::boxed::{DynEvaluator, IntoDynEvaluator};
use crate::message::DataHash;
use crate::player::{PlayerInfo, PlayerMutator, PlayerRegistry};
use crate::trigger::TriggerRegistry;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::MutexGuard;
use tracing::warn;
use uuid::Uuid;

/// Error type for errors encountered when building an achievement.
#[derive(Debug, Error)]
pub enum BuilderError {
    /// Indicates a required field was missing during build.
    #[error("missing field: {0}")]
    MissingField(&'static str),
}

/// Builder for constructing [`Achievement`] instances with typed parameters.
#[derive(Debug, Default)]
pub struct AchievementBuilder<Player, State, Trigger, Metadata> {
    id: Option<Uuid>,
    evaluator: Option<Box<dyn DynEvaluator<Player, State, Trigger>>>,
    metadata: Metadata,
    audio_path: Option<PathBuf>,
    hot_duration: Option<Duration>,
}

impl AchievementBuilder<(), (), (), ()> {
    pub fn new() -> Self {
        Self {
            id: None,
            evaluator: None,
            metadata: (),
            audio_path: None,
            hot_duration: None,
        }
    }
}

impl<Player, State, Trigger, Metadata> AchievementBuilder<Player, State, Trigger, Metadata> {
    /// Sets the unique ID for the achievement.
    pub fn id(mut self, id: Uuid) -> Self {
        self.id = Some(id);
        self
    }

    /// Sets the evaluator that determines when the achievement is awarded.
    pub fn evaluator<P, S, T>(
        self,
        evaluator: impl IntoDynEvaluator<P, S, T>,
    ) -> AchievementBuilder<P, S, T, Metadata> {
        AchievementBuilder {
            id: self.id,
            evaluator: Some(evaluator.into_dyn_evaluator()),
            metadata: self.metadata,
            audio_path: self.audio_path,
            hot_duration: self.hot_duration,
        }
    }

    /// Sets the metadata associated with the achievement.
    pub fn metadata<T>(self, metadata: T) -> AchievementBuilder<Player, State, Trigger, T> {
        AchievementBuilder {
            id: self.id,
            evaluator: self.evaluator,
            metadata,
            audio_path: self.audio_path,
            hot_duration: self.hot_duration,
        }
    }

    /// Optionally sets a path to audio to play when the achievement is awarded.
    pub fn audio_path<P: Into<PathBuf>>(mut self, audio_path: P) -> Self {
        self.audio_path = Some(audio_path.into());
        self
    }

    /// Optionally sets a duration for which the achievement remains "hot" after
    /// being awarded.
    pub fn hot_duration(mut self, hot_duration: Duration) -> Self {
        self.hot_duration = Some(hot_duration);
        self
    }

    /// Attempts to build an achievement, returning an error if required fields are
    /// missing.
    ///
    /// # Errors
    ///
    /// Returns [`BuilderError::MissingField`] if `id` or `evaluator` was not set.
    pub fn build(self) -> Result<Achievement<Metadata, Player, State, Trigger>, BuilderError> {
        let id = self.id.ok_or(BuilderError::MissingField("id"))?;
        let evaluator = self
            .evaluator
            .ok_or(BuilderError::MissingField("evaluator"))?;

        Ok(Achievement {
            id,
            evaluator,
            metadata: self.metadata,
            audio_file: self.audio_path.into(),
            hot_duration: self.hot_duration,
        })
    }
}

#[derive(Debug)]
pub struct AudioFile {
    pub path: PathBuf,
    pub hash: DataHash,
}

#[derive(Debug)]
pub struct AudioSource {
    pub relative: Option<PathBuf>,
    resolved: OnceLock<Option<AudioFile>>,
}

impl From<Option<PathBuf>> for AudioSource {
    fn from(path: Option<PathBuf>) -> Self {
        AudioSource {
            relative: path,
            resolved: OnceLock::new(),
        }
    }
}

impl AudioSource {
    pub fn resolve(&self, base_path: &Path) -> Option<&AudioFile> {
        self.resolved
            .get_or_init(|| {
                let relative = self.relative.as_ref()?;

                let path = base_path.join(relative);
                let Ok(file_content) = fs::read(path.clone()) else {
                    warn!("Audio file missing: {:?}", path);
                    return None;
                };

                let digest = md5::compute(file_content);

                Some(AudioFile {
                    path,
                    hash: digest.into(),
                })
            })
            .as_ref()
    }
}

/// Represents a configured achievement with evaluation logic.
#[derive(Debug)]
pub struct Achievement<Metadata, Player, State, Trigger> {
    /// Unique ID of the achievement.
    pub id: Uuid,
    /// Metadata definition of the achievement.
    pub metadata: Metadata,
    /// Audio file to play when achievement is awarded.
    pub audio_file: AudioSource,
    /// Evaluation logic for determining if the achievement should be awarded.
    pub evaluator: Box<dyn DynEvaluator<Player, State, Trigger>>,
    /// Optional duration for which the achievement remains "hot".
    pub hot_duration: Option<Duration>,
}

impl<Metadata, Player, State, Trigger> Achievement<Metadata, Player, State, Trigger> {
    /// Evaluates the achievement against the provided context.
    ///
    /// Awards the achievement to one or multiple players depending on the
    /// evaluation result.
    pub fn evaluate(&self, ctx: &AchievementContext<Player, State, Trigger>) {
        match self.evaluator.evaluate(ctx) {
            EvalResult::AwardSelf => {
                ctx.award_achievement(self.id, ctx.current_bloop.player_id);
            }
            EvalResult::AwardMultiple(player_ids) => {
                for player_id in player_ids {
                    ctx.award_achievement(self.id, player_id);
                }
            }
            EvalResult::NoAward => {}
        }
    }
}

/// Represents the achievements awarded to a single player at a specific time.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerAchievementAwards {
    /// The unique ID of the player who received the achievements.
    pub player_id: Uuid,
    /// The list of achievement IDs awarded to the player.
    pub achievement_ids: Vec<Uuid>,
}

/// Represents a batch of achievements awarded to multiple players at a given timestamp.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AchievementAwardBatch {
    /// The timestamp when the achievements were awarded.
    pub awarded_at: DateTime<Utc>,
    /// The list of players and their awarded achievements.
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

/// Tracks achievements awarded to players during an evaluation cycle.
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

    pub fn remove_duplicates<Player: PlayerInfo + PlayerMutator>(
        &mut self,
        player_registry: MutexGuard<PlayerRegistry<Player>>,
    ) {
        self.awarded.retain(|player_id, achievements| {
            let Some(player) = player_registry.read_by_id(*player_id) else {
                return false;
            };

            let awarded = player.awarded_achievements();
            achievements.retain(|achievement_id| !awarded.contains_key(achievement_id));
            !achievements.is_empty()
        });
    }
}

/// Context used when evaluating and awarding achievements for a specific bloop.
#[derive(Debug)]
pub struct AchievementContext<'a, Player, State, Trigger> {
    /// The bloop currently being evaluated.
    pub current_bloop: &'a Bloop<Player>,
    /// Provides access to bloop data for the clients and players.
    pub bloop_provider: &'a BloopProvider<Player>,
    /// Additional evaluation-specific state.
    pub state: &'a State,
    /// Registry of all active triggers.
    trigger_registry: Mutex<&'a mut TriggerRegistry<Trigger>>,
    /// Tracks achievements awarded during this evaluation.
    awarded_tracker: Mutex<AwardedTracker>,
}

impl<'a, Player, State, Trigger> AchievementContext<'a, Player, State, Trigger> {
    pub fn new(
        current_bloop: &'a Bloop<Player>,
        bloop_provider: &'a BloopProvider<Player>,
        state: &'a State,
        trigger_registry: &'a mut TriggerRegistry<Trigger>,
    ) -> Self {
        AchievementContext {
            current_bloop,
            bloop_provider,
            state,
            trigger_registry: Mutex::new(trigger_registry),
            awarded_tracker: Mutex::new(AwardedTracker::new(current_bloop)),
        }
    }

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

    /// Records that a given achievement should be awarded to a player.
    pub fn award_achievement(&self, achievement_id: Uuid, player_id: Uuid) {
        self.awarded_tracker
            .lock()
            .unwrap()
            .add(achievement_id, player_id);
    }

    /// Consumes the context and returns all awarded achievements.
    pub(crate) fn take_awarded_tracker(self) -> AwardedTracker {
        self.awarded_tracker.into_inner().unwrap()
    }
}

impl<'a, Player, Metadata, Trigger: PartialEq> AchievementContext<'a, Player, Metadata, Trigger> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::achievement::{AchievementBuilder, AchievementContext, BuilderError};
    use crate::bloop::Bloop;
    use crate::evaluator::Evaluator;
    use crate::nfc_uid::NfcUid;
    use crate::test_utils::{MockPlayer, MockPlayerBuilder, TestCtxBuilder};
    use crate::trigger::{TriggerOccurrence, TriggerRegistry, TriggerSpec};
    use chrono::Utc;
    use uuid::Uuid;

    #[derive(Debug)]
    struct DummyEvaluator(EvalResult);

    impl Evaluator<MockPlayer, (), ()> for DummyEvaluator {
        fn evaluate(&self, _ctx: &AchievementContext<MockPlayer, (), ()>) -> impl Into<EvalResult> {
            self.0.clone()
        }
    }

    #[test]
    fn missing_id_fails_to_build() {
        let builder = AchievementBuilder::<(), (), (), ()>::new();
        let err = builder.build().unwrap_err();
        assert!(matches!(err, BuilderError::MissingField("id")));
    }

    #[test]
    fn missing_evaluator_fails_to_build() {
        let builder = AchievementBuilder::<(), (), (), ()>::new().id(Uuid::new_v4());
        let err = builder.build().unwrap_err();
        assert!(matches!(err, BuilderError::MissingField("evaluator")));
    }

    #[test]
    fn builds_successfully_with_all_required_fields() {
        let id = Uuid::new_v4();
        let evaluator = DummyEvaluator(EvalResult::AwardSelf);

        let achievement = AchievementBuilder::new()
            .id(id)
            .evaluator(evaluator)
            .build()
            .expect("should build");

        assert_eq!(achievement.id, id);
    }

    #[test]
    fn evaluate_awards_self_when_evaluator_returns_award_self() {
        let evaluator = DummyEvaluator(EvalResult::AwardSelf);

        let (player, player_id) = MockPlayerBuilder::new().build();
        let bloop = Bloop::new(player, "client", Utc::now());

        let id = Uuid::new_v4();
        let achievement = AchievementBuilder::new()
            .id(id)
            .evaluator(evaluator)
            .build()
            .unwrap();

        let mut ctx_builder = TestCtxBuilder::new(bloop);
        let ctx = ctx_builder.build();
        achievement.evaluate(&ctx);

        let awarded = ctx.take_awarded_tracker();
        assert!(awarded.for_player(player_id).unwrap().contains(&id));
    }

    #[test]
    fn evaluate_awards_multiple_players_when_evaluator_returns_award_multiple() {
        let evaluator = DummyEvaluator(EvalResult::AwardMultiple(vec![
            Uuid::new_v4(),
            Uuid::new_v4(),
        ]));

        let (player, _) = MockPlayerBuilder::new().build();
        let bloop = Bloop::new(player, "client", Utc::now());

        let mut ctx_builder = TestCtxBuilder::new(bloop);
        let ctx = ctx_builder.build();

        let id = Uuid::new_v4();
        let achievement = AchievementBuilder::new()
            .id(id)
            .evaluator(evaluator)
            .build()
            .unwrap();

        achievement.evaluate(&ctx);

        let awarded = ctx.take_awarded_tracker();
        assert_eq!(
            awarded.awarded.values().map(|set| set.len()).sum::<usize>(),
            2
        );
    }

    #[test]
    fn award_achievement_records_award_correctly() {
        let (player, player_id) = MockPlayerBuilder::new().build();
        let bloop = Bloop::new(player, "client", Utc::now());

        let mut ctx_builder = TestCtxBuilder::new(bloop);
        let ctx = ctx_builder.build();

        let achievement_id = Uuid::new_v4();
        ctx.award_achievement(achievement_id, ctx.current_bloop.player_id);

        let awarded = ctx.take_awarded_tracker();
        let awards = awarded.for_player(player_id).unwrap();
        assert!(awards.contains(&achievement_id));
    }

    #[test]
    fn has_trigger_returns_true_when_trigger_active() {
        #[derive(Copy, Clone, PartialEq, Debug)]
        enum DummyTrigger {
            Active,
            Inactive,
        }

        let (player, _) = MockPlayerBuilder::new().build();
        let bloop = Bloop::new(player, "client", Utc::now());

        let nfc_uid = NfcUid::default();
        let mut triggers = HashMap::new();
        triggers.insert(
            nfc_uid,
            TriggerSpec {
                trigger: DummyTrigger::Active,
                occurrence: TriggerOccurrence::Once,
                global: false,
            },
        );
        let mut registry = TriggerRegistry::new(triggers);
        registry.try_activate_trigger(nfc_uid, "client");

        let mut ctx_builder = TestCtxBuilder::new(bloop).trigger_registry(registry);
        let ctx = ctx_builder.build();

        assert!(ctx.has_trigger(DummyTrigger::Active));
        assert!(!ctx.has_trigger(DummyTrigger::Inactive));
    }
}
