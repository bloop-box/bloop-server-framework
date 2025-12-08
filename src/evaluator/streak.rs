use crate::achievement::AchievementContext;
use crate::evaluator::{EvalResult, Evaluator};
use std::collections::HashSet;
use std::fmt;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::time::Duration;

/// Evaluates if recent bloops meet a predicate.
///
/// Checks if at least `min_required` recent bloops within `max_window` satisfy the given predicate
/// on player.
pub struct StreakEvaluator<Player, F>
where
    F: Fn(&Player) -> bool + Send + Sync + 'static,
{
    /// Closure or function to test a player's registration number.
    predicate: F,

    /// Minimum number of bloops that must pass the test.
    min_required: usize,

    /// Time window in which to evaluate recent bloops, counting backward from the current one.
    max_window: Duration,

    _marker: PhantomData<Player>,
}

impl<Player, F> StreakEvaluator<Player, F>
where
    F: Fn(&Player) -> bool + Send + Sync + 'static,
{
    /// Creates a new [`StreakEvaluator`] with the given predicate, count, and window.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::time::Duration;
    /// use bloop_server_framework::evaluator::streak::StreakEvaluator;
    ///
    /// struct Player {
    ///     sponsor: bool,
    /// }
    ///
    /// let evaluator = StreakEvaluator::new(|player: &Player| player.sponsor, 3, Duration::from_secs(60));
    /// ```
    pub fn new(predicate: F, min_required: usize, max_window: Duration) -> Self {
        Self {
            predicate,
            min_required,
            max_window,
            _marker: PhantomData,
        }
    }
}

impl<Player, Metadata, Trigger, F> Evaluator<Player, Metadata, Trigger>
    for StreakEvaluator<Player, F>
where
    F: Fn(&Player) -> bool + Send + Sync + 'static,
{
    fn evaluate(
        &self,
        ctx: &AchievementContext<Player, Metadata, Trigger>,
    ) -> impl Into<EvalResult> {
        let mut seen_players = HashSet::new();
        seen_players.insert(ctx.current_bloop.player_id);
        let mut streak = 0;

        let bloops = ctx
            .client_bloops()
            .filter(ctx.filter_within_window(self.max_window))
            .take(self.min_required);

        for bloop in bloops {
            if seen_players.contains(&bloop.player_id) || !(self.predicate)(&bloop.player()) {
                break;
            }

            seen_players.insert(bloop.player_id);
            streak += 1;

            if streak >= self.min_required {
                return true;
            }
        }

        false
    }
}

impl<Player, F> Debug for StreakEvaluator<Player, F>
where
    F: Fn(&Player) -> bool + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreakEvaluator")
            .field("predicate", &"<closure>")
            .field("min_required", &self.min_required)
            .field("max_window", &self.max_window)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bloop::Bloop;
    use crate::test_utils::{MockPlayer, TestCtxBuilder};
    use chrono::{DateTime, Utc};
    use std::time::{Duration, SystemTime};

    fn make_bloop(client_id: &str, name: &str, seconds_ago: u64) -> Bloop<MockPlayer> {
        let now = SystemTime::now() - Duration::from_secs(seconds_ago);
        let timestamp: DateTime<Utc> = now.into();

        let (player, _) = MockPlayer::builder().name(name).build();

        Bloop::new(player, client_id, timestamp)
    }

    #[test]
    fn award_when_all_recent_bloops_match_predicate() {
        let evaluator = StreakEvaluator::new(
            |player: &MockPlayer| player.name == "foo",
            3,
            Duration::from_secs(60),
        );

        let current = make_bloop("client1", "foo", 0);
        let past = vec![
            make_bloop("client1", "foo", 10),
            make_bloop("client1", "foo", 20),
            make_bloop("client1", "foo", 30),
        ];

        let mut builder = TestCtxBuilder::new(current).bloops(past);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::AwardSelf
        );
    }

    #[test]
    fn no_award_if_not_enough_match_predicate() {
        let evaluator = StreakEvaluator::new(
            |player: &MockPlayer| player.name == "foo",
            3,
            Duration::from_secs(60),
        );

        let current = make_bloop("client1", "foo", 0);
        let past = vec![
            make_bloop("client1", "bar", 10),
            make_bloop("client1", "foo", 20),
            make_bloop("client1", "bar", 30),
        ];

        let mut builder = TestCtxBuilder::new(current).bloops(past);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::NoAward
        );
    }

    #[test]
    fn ignores_bloops_outside_time_window() {
        let evaluator = StreakEvaluator::new(
            |player: &MockPlayer| player.name == "foo",
            2,
            Duration::from_secs(20),
        );

        let current = make_bloop("client1", "foo", 0);
        let past = vec![
            make_bloop("client1", "foo", 10),
            make_bloop("client1", "foo", 30), // outside window
        ];

        let mut builder = TestCtxBuilder::new(current).bloops(past);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::NoAward
        );
    }

    #[test]
    fn counts_only_min_required_recent_matches() {
        let evaluator = StreakEvaluator::new(
            |player: &MockPlayer| player.name == "foo",
            2,
            Duration::from_secs(60),
        );

        let current = make_bloop("client1", "foo", 0);
        let past = vec![
            make_bloop("client1", "foo", 10),
            make_bloop("client1", "foo", 15),
            make_bloop("client1", "bar", 20),
        ];

        let mut builder = TestCtxBuilder::new(current).bloops(past);
        assert_eq!(
            evaluator.evaluate(&builder.build()).into(),
            EvalResult::AwardSelf
        );
    }
}
