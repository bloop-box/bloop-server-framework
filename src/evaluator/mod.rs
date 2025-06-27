use crate::achievement::AchievementContext;
use std::fmt::Debug;
use uuid::Uuid;

pub mod min_bloops;
pub mod registration_number;
pub mod spelling_bee;
pub mod test_utils;
pub mod time;
pub mod trigger;

/// Evaluates whether the current player qualifies for an achievement.
///
/// This trait is intended for achievements that are evaluated on a per-player
/// basis. It runs in the context of a specific triggering event, and can be
/// converted into a general-purpose [`Evaluator`] for registration.
///
/// # Examples
///
/// ```
/// use uuid::Uuid;
/// use bloop_server_framework::achievement::AchievementContext;
/// use bloop_server_framework::evaluator::SingleEvaluator;
///
/// #[derive(Debug)]
/// struct AlwaysAward;
///
/// impl<P, M, T> SingleEvaluator<P, M, T> for AlwaysAward
/// where
///     T: Copy + PartialEq + Eq + std::fmt::Debug,
/// {
///     fn evaluate(&self, _ctx: &AchievementContext<P, M, T>) -> bool {
///         true
///     }
/// }
/// ```
pub trait SingleEvaluator<Player, Metadata, Trigger>: Send + Sync + Debug
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    fn evaluate(&self, ctx: &AchievementContext<Player, Metadata, Trigger>) -> bool;

    fn boxed(self) -> Box<dyn SingleEvaluator<Player, Metadata, Trigger>>
    where
        Self: Sized + 'static,
    {
        Box::new(self)
    }

    fn wrap(self) -> Evaluator<Player, Metadata, Trigger>
    where
        Self: Sized + 'static,
    {
        Evaluator::Single(Box::new(self))
    }
}

/// Evaluates whether multiple players qualify for an achievement at once.
///
/// This trait supports achievements that can be granted to more than one player
/// in response to a single triggering event. It returns a list of player IDs
/// who should receive the award.
///
/// # Examples
///
/// ```
/// use uuid::Uuid;
/// use bloop_server_framework::achievement::AchievementContext;
/// use bloop_server_framework::evaluator::MultiEvaluator;
///
/// #[derive(Debug)]
/// struct GiveToAll;
///
/// impl<P: Clone, M, T> MultiEvaluator<P, M, T> for GiveToAll
/// where
///     T: Copy + PartialEq + Eq + std::fmt::Debug,
/// {
///     fn evaluate(&self, ctx: &AchievementContext<P, M, T>) -> Option<Vec<Uuid>> {
///         Some(vec![ctx.current_bloop.player_id])
///     }
/// }
/// ```
pub trait MultiEvaluator<Player, Metadata, Trigger>: Send + Sync + Debug
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    fn evaluate(&self, ctx: &AchievementContext<Player, Metadata, Trigger>) -> Option<Vec<Uuid>>;

    fn boxed(self) -> Box<dyn MultiEvaluator<Player, Metadata, Trigger>>
    where
        Self: Sized + 'static,
    {
        Box::new(self)
    }

    fn wrap(self) -> Evaluator<Player, Metadata, Trigger>
    where
        Self: Sized + 'static,
    {
        Evaluator::Multi(Box::new(self))
    }
}

/// A unified wrapper around both [`SingleEvaluator`] and [`MultiEvaluator`]
/// types.
///
/// Used to register and evaluate achievements regardless of how many players
/// they apply to.
#[derive(Debug)]
pub enum Evaluator<Player, Metadata, Trigger>
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    Single(Box<dyn SingleEvaluator<Player, Metadata, Trigger>>),
    Multi(Box<dyn MultiEvaluator<Player, Metadata, Trigger>>),
}

impl<Player, Metadata, Trigger> Evaluator<Player, Metadata, Trigger>
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    /// Evaluates the given achievement in the provided context and applies it to
    /// the relevant players.
    pub fn evaluate(
        &self,
        achievement_id: Uuid,
        ctx: &AchievementContext<Player, Metadata, Trigger>,
    ) {
        match self {
            Evaluator::Single(evaluator) => {
                if evaluator.evaluate(ctx) {
                    ctx.award_achievement(achievement_id, ctx.current_bloop.player_id);
                }
            }
            Evaluator::Multi(evaluator) => {
                if let Some(player_ids) = evaluator.evaluate(ctx) {
                    for player_id in player_ids {
                        ctx.award_achievement(achievement_id, player_id);
                    }
                }
            }
        }
    }
}

impl<Player, Metadata, Trigger> From<Box<dyn SingleEvaluator<Player, Metadata, Trigger>>>
    for Evaluator<Player, Metadata, Trigger>
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    fn from(value: Box<dyn SingleEvaluator<Player, Metadata, Trigger>>) -> Self {
        Evaluator::Single(value)
    }
}

impl<Player, Metadata, Trigger> From<Box<dyn MultiEvaluator<Player, Metadata, Trigger>>>
    for Evaluator<Player, Metadata, Trigger>
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    fn from(value: Box<dyn MultiEvaluator<Player, Metadata, Trigger>>) -> Self {
        Evaluator::Multi(value)
    }
}

#[cfg(test)]
mod tests {
    use crate::achievement::AchievementContext;
    use crate::bloop::Bloop;
    use crate::evaluator::test_utils::{MockPlayer, TestCtxBuilder};
    use crate::evaluator::{Evaluator, MultiEvaluator, SingleEvaluator};
    use crate::test_utils::Utc;
    use std::fmt::Debug;
    use uuid::Uuid;

    #[derive(Debug)]
    struct FixedSingleEvaluator(bool);

    impl<Player, Metadata, Trigger> SingleEvaluator<Player, Metadata, Trigger> for FixedSingleEvaluator
    where
        Trigger: Copy + PartialEq + Eq + Debug,
    {
        fn evaluate(&self, _ctx: &AchievementContext<Player, Metadata, Trigger>) -> bool {
            self.0
        }
    }

    #[derive(Debug)]
    struct FixedMultiEvaluator(Option<Vec<Uuid>>);

    impl<Player, Metadata, Trigger> MultiEvaluator<Player, Metadata, Trigger> for FixedMultiEvaluator
    where
        Trigger: Copy + PartialEq + Eq + Debug,
    {
        fn evaluate(
            &self,
            _ctx: &AchievementContext<Player, Metadata, Trigger>,
        ) -> Option<Vec<Uuid>> {
            self.0.clone()
        }
    }

    #[test]
    fn single_evaluator_awards_achievement_when_true() {
        let (player, player_id) = MockPlayer::builder().build();
        let bloop = Bloop::new(player.clone(), "client1", Utc::now());
        let mut builder = TestCtxBuilder::new(bloop);
        let ctx = builder.build();

        let evaluator: Evaluator<_, _, _> = FixedSingleEvaluator(true).boxed().into();

        evaluator.evaluate(Uuid::new_v4(), &ctx);
        let tracker = ctx.take_awarded_tracker();

        assert!(!tracker.for_player(player_id).unwrap().is_empty());
    }

    #[test]
    fn single_evaluator_does_not_award_achievement_when_false() {
        let (player, player_id) = MockPlayer::builder().build();
        let bloop = Bloop::new(player.clone(), "client1", Utc::now());
        let mut builder = TestCtxBuilder::new(bloop);
        let ctx = builder.build();

        let evaluator: Evaluator<_, _, _> = FixedSingleEvaluator(false).boxed().into();

        evaluator.evaluate(Uuid::new_v4(), &ctx);
        let tracker = ctx.take_awarded_tracker();

        assert!(tracker.for_player(player_id).is_none());
    }

    #[test]
    fn multi_evaluator_awards_achievements_for_all() {
        let (player1, player1_id) = MockPlayer::builder().build();
        let player2_id = Uuid::new_v4();
        let bloop = Bloop::new(player1.clone(), "client1", Utc::now());
        let mut builder = TestCtxBuilder::new(bloop);
        let ctx = builder.build();

        let evaluator: Evaluator<_, _, _> = FixedMultiEvaluator(Some(vec![player1_id, player2_id]))
            .boxed()
            .into();

        evaluator.evaluate(Uuid::new_v4(), &ctx);
        let tracker = ctx.take_awarded_tracker();

        assert!(!tracker.for_player(player1_id).unwrap().is_empty());
        assert!(!tracker.for_player(player2_id).unwrap().is_empty());
    }

    #[test]
    fn multi_evaluator_does_not_award_when_none() {
        let (player, player_id) = MockPlayer::builder().build();
        let bloop = Bloop::new(player.clone(), "client1", Utc::now());
        let mut builder = TestCtxBuilder::new(bloop);
        let ctx = builder.build();

        let evaluator: Evaluator<_, _, _> = FixedMultiEvaluator(None).boxed().into();

        evaluator.evaluate(Uuid::new_v4(), &ctx);
        let tracker = ctx.take_awarded_tracker();

        assert!(tracker.for_player(player_id).is_none());
    }
}
