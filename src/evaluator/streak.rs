use crate::achievement::AchievementContext;
use crate::builder::{NoValue, Value};
use crate::evaluator::{AwardMode, EvalResult, Evaluator};
use std::fmt;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::time::Duration;

/// Builder for [`StreakEvaluator`].
#[derive(Debug, Default)]
pub struct StreakEvaluatorBuilder<R, W> {
    min_required: R,
    max_window: W,
    award_mode: AwardMode,
}

impl StreakEvaluatorBuilder<NoValue, NoValue> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<W, R> StreakEvaluatorBuilder<R, W> {
    /// Award all involved players instead of just the current player.
    pub fn award_all(self) -> Self {
        Self {
            award_mode: AwardMode::All,
            ..self
        }
    }
}

impl<W> StreakEvaluatorBuilder<NoValue, W> {
    /// Set the minimum number of matching bloops required to award the achievement.
    pub fn min_required(self, min_required: usize) -> StreakEvaluatorBuilder<Value<usize>, W> {
        StreakEvaluatorBuilder {
            min_required: Value(min_required),
            max_window: self.max_window,
            award_mode: self.award_mode,
        }
    }
}

impl<R> StreakEvaluatorBuilder<R, NoValue> {
    /// Set the maximum time window in which the bloops must have occurred.
    pub fn max_window(self, max_window: Duration) -> StreakEvaluatorBuilder<R, Value<Duration>> {
        StreakEvaluatorBuilder {
            min_required: self.min_required,
            max_window: Value(max_window),
            award_mode: self.award_mode,
        }
    }
}

impl StreakEvaluatorBuilder<Value<usize>, Value<Duration>> {
    /// Build the evaluator.
    pub fn build<Player, State, Trigger, P>(
        self,
        predicate: P,
    ) -> impl Evaluator<Player, State, Trigger> + Debug
    where
        Player: 'static,
        State: 'static,
        Trigger: 'static,
        P: Fn(&Player) -> bool + Send + Sync + 'static,
    {
        fn derive_ctx<Player, State, Trigger>(_ctx: &AchievementContext<Player, State, Trigger>) {}
        let predicate_wrapper = move |player: &Player, _: &()| predicate(player);

        StreakEvaluator {
            min_required: self.min_required.0,
            max_window: self.max_window.0,
            award_mode: self.award_mode,
            derive_ctx,
            predicate: predicate_wrapper,
            _marker: PhantomData,
        }
    }

    /// Build the evaluator with additionally derived context.
    ///
    /// The derived context can be used to initially retrieve values from the
    /// achievement context and have them available in the extractor.
    pub fn build_with_derived_ctx<Player, State, Trigger, C, DC, P>(
        self,
        derive_ctx: DC,
        predicate: P,
    ) -> impl Evaluator<Player, State, Trigger> + Debug
    where
        DC: Fn(&AchievementContext<Player, State, Trigger>) -> C + Send + Sync + 'static,
        P: Fn(&Player, &C) -> bool + Send + Sync + 'static,
    {
        StreakEvaluator {
            min_required: self.min_required.0,
            max_window: self.max_window.0,
            award_mode: self.award_mode,
            derive_ctx,
            predicate,
            _marker: PhantomData,
        }
    }
}

/// Evaluator that counts recent bloops matching a predicate.
///
/// This evaluator awards the current or all participating players when the
/// matching bloops reach the `min_required` count.
pub struct StreakEvaluator<Player, State, Trigger, C, DC, P>
where
    DC: Fn(&AchievementContext<Player, State, Trigger>) -> C + Send + Sync + 'static,
    P: Fn(&Player, &C) -> bool + Send + Sync + 'static,
{
    min_required: usize,
    max_window: Duration,
    award_mode: AwardMode,
    derive_ctx: DC,
    predicate: P,
    _marker: PhantomData<(Player, State, Trigger)>,
}

impl<Player, State, Trigger, C, DC, P> Evaluator<Player, State, Trigger>
    for StreakEvaluator<Player, State, Trigger, C, DC, P>
where
    DC: Fn(&AchievementContext<Player, State, Trigger>) -> C + Send + Sync + 'static,
    P: Fn(&Player, &C) -> bool + Send + Sync + 'static,
{
    fn evaluate(&self, ctx: &AchievementContext<Player, State, Trigger>) -> impl Into<EvalResult> {
        let derived_ctx = (self.derive_ctx)(ctx);
        let mut player_ids = Vec::with_capacity(self.min_required + 1);
        player_ids.push(ctx.current_bloop.player_id);

        let bloops = ctx
            .client_bloops()
            .filter(ctx.filter_within_window(self.max_window))
            .take(self.min_required);

        for bloop in bloops {
            if player_ids.contains(&bloop.player_id)
                || !(self.predicate)(&bloop.player(), &derived_ctx)
            {
                return EvalResult::NoAward;
            }

            player_ids.push(bloop.player_id);
        }

        if player_ids.len() <= self.min_required {
            return EvalResult::NoAward;
        }

        match self.award_mode {
            AwardMode::Current => EvalResult::AwardSelf,
            AwardMode::All => EvalResult::AwardMultiple(player_ids),
        }
    }
}

impl<Player, State, Trigger, C, DC, P> Debug for StreakEvaluator<Player, State, Trigger, C, DC, P>
where
    DC: Fn(&AchievementContext<Player, State, Trigger>) -> C + Send + Sync + 'static,
    P: Fn(&Player, &C) -> bool + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreakEvaluator")
            .field("derive_ctx", &"<closure>")
            .field("predicate", &"<closure>")
            .field("min_required", &self.min_required)
            .field("max_window", &self.max_window)
            .field("award_mode", &self.award_mode)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bloop::Bloop;
    use crate::evaluator::{EvalResult, Evaluator};
    use crate::test_utils::{MockPlayer, TestCtxBuilder};
    use chrono::{DateTime, Utc};
    use std::time::SystemTime;

    fn make_bloop(client_id: &str, name: &str, seconds_ago: u64) -> Bloop<MockPlayer> {
        let now = SystemTime::now() - Duration::from_secs(seconds_ago);
        let timestamp: DateTime<Utc> = now.into();

        let (player, _) = MockPlayer::builder().name(name).build();

        Bloop::new(player, client_id, timestamp)
    }

    #[test]
    fn award_when_required_consecutive_matches_are_met() {
        let evaluator = StreakEvaluatorBuilder::new()
            .min_required(2)
            .max_window(Duration::from_secs(60))
            .build(|player: &MockPlayer| player.name == "foo");

        let current = make_bloop("client1", "foo", 0);
        let past = vec![
            make_bloop("client1", "foo", 10),
            make_bloop("client1", "foo", 20),
        ];

        let mut ctx_builder = TestCtxBuilder::new(current).bloops(past);
        let res: EvalResult = evaluator.evaluate(&ctx_builder.build()).into();

        assert_eq!(res, EvalResult::AwardSelf);
    }

    #[test]
    fn award_all_mode_returns_all_player_ids() {
        let evaluator = StreakEvaluatorBuilder::new()
            .min_required(3)
            .max_window(Duration::from_secs(60))
            .award_all()
            .build(|player: &MockPlayer| player.name == "foo");

        let current = make_bloop("client1", "foo", 0);
        let b1 = make_bloop("client1", "foo", 10);
        let b2 = make_bloop("client1", "foo", 20);
        let b3 = make_bloop("client1", "foo", 30);
        let b4 = make_bloop("client1", "foo", 40);

        let expected_ids = vec![current.player_id, b1.player_id, b2.player_id, b3.player_id];

        let mut ctx_builder = TestCtxBuilder::new(current).bloops(vec![b1, b2, b3, b4]);
        let res: EvalResult = evaluator.evaluate(&ctx_builder.build()).into();

        match res {
            EvalResult::AwardMultiple(ids) => {
                assert_eq!(ids, expected_ids);
            }
            other => panic!("expected AwardMultiple, got {:?}", other),
        };
    }

    #[test]
    fn predicate_mismatch_leads_to_no_award() {
        let evaluator = StreakEvaluatorBuilder::new()
            .min_required(1)
            .max_window(Duration::from_secs(60))
            .build(|_player: &MockPlayer| false);

        let current = make_bloop("client1", "alice", 0);
        let b1 = make_bloop("client1", "alice", 10);

        let mut ctx_builder = TestCtxBuilder::new(current).bloops(vec![b1]);
        let res: EvalResult = evaluator.evaluate(&ctx_builder.build()).into();

        assert_eq!(res, EvalResult::NoAward);
    }

    #[test]
    fn duplicate_player_in_recent_bloops_causes_no_award() {
        let evaluator = StreakEvaluatorBuilder::new()
            .min_required(2)
            .max_window(Duration::from_secs(60))
            .build(|player: &MockPlayer| player.name == "bob");

        let current = make_bloop("client1", "alice", 0);

        let b1 = make_bloop("client1", "bob", 10);
        let mut b2 = make_bloop("client1", "bob", 20);
        b2.player_id = b1.player_id;

        let mut ctx_builder = TestCtxBuilder::new(current).bloops(vec![b1, b2]);
        let res: EvalResult = evaluator.evaluate(&ctx_builder.build()).into();

        assert_eq!(res, EvalResult::NoAward);
    }

    #[test]
    fn ignores_bloops_outside_time_window() {
        let evaluator = StreakEvaluatorBuilder::new()
            .min_required(2)
            .max_window(Duration::from_secs(20))
            .build(|player: &MockPlayer| player.name == "foo");

        let current = make_bloop("client1", "foo", 0);
        let b1 = make_bloop("client1", "foo", 30);

        let mut ctx_builder = TestCtxBuilder::new(current).bloops(vec![b1]);
        let res: EvalResult = evaluator.evaluate(&ctx_builder.build()).into();

        assert_eq!(res, EvalResult::NoAward);
    }

    #[test]
    fn build_with_derived_ctx_supplies_context_to_predicate() {
        let evaluator = StreakEvaluatorBuilder::new()
            .min_required(1)
            .max_window(Duration::from_secs(60))
            .build_with_derived_ctx(
                |_ctx| vec!["carol"],
                |player: &MockPlayer, allowed: &Vec<&str>| allowed.contains(&player.name.as_str()),
            );

        let current = make_bloop("client1", "bob", 0);
        let past = vec![make_bloop("client1", "carol", 10)];

        let mut ctx_builder = TestCtxBuilder::new(current).bloops(past);
        let res: EvalResult = evaluator.evaluate(&ctx_builder.build()).into();

        assert_eq!(res, EvalResult::AwardSelf);
    }
}
