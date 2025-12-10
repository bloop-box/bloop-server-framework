use crate::achievement::AchievementContext;
use crate::bloop::Bloop;
use crate::builder::{NoValue, Value};
use crate::evaluator::{AwardMode, EvalResult, Evaluator};
use std::collections::HashSet;
use std::fmt;
use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;
use std::time::Duration;

/// Result of extracting zero, one or more values from a bloop.
#[derive(Debug)]
pub enum ExtractResult<V> {
    Single(V),
    Multiple(Vec<V>),
    Abort,
}

/// Builder for [`DistinctValuesEvaluator`].
#[derive(Debug, Default)]
pub struct DistinctValuesEvaluatorBuilder<R, W> {
    min_required: R,
    max_window: W,
    award_mode: AwardMode,
}

impl DistinctValuesEvaluatorBuilder<NoValue, NoValue> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<W, R> DistinctValuesEvaluatorBuilder<R, W> {
    /// Award all involved players instead of just the current player.
    pub fn award_all(self) -> Self {
        Self {
            award_mode: AwardMode::All,
            ..self
        }
    }
}

impl<W> DistinctValuesEvaluatorBuilder<NoValue, W> {
    /// Set the minimum number of distinct values required to award the achievement.
    pub fn min_required(
        self,
        min_required: usize,
    ) -> DistinctValuesEvaluatorBuilder<Value<usize>, W> {
        DistinctValuesEvaluatorBuilder {
            min_required: Value(min_required),
            max_window: self.max_window,
            award_mode: self.award_mode,
        }
    }
}

impl<R> DistinctValuesEvaluatorBuilder<R, NoValue> {
    /// Set the maximum time window in which the bloops must have occurred.
    pub fn max_window(
        self,
        max_window: Duration,
    ) -> DistinctValuesEvaluatorBuilder<R, Value<Duration>> {
        DistinctValuesEvaluatorBuilder {
            min_required: self.min_required,
            max_window: Value(max_window),
            award_mode: self.award_mode,
        }
    }
}

impl DistinctValuesEvaluatorBuilder<Value<usize>, Value<Duration>> {
    /// Build the evaluator.
    pub fn build<Player, State, Trigger, V, E>(
        self,
        extract: E,
    ) -> impl Evaluator<Player, State, Trigger> + Debug
    where
        Player: 'static,
        State: 'static,
        Trigger: 'static,
        E: Fn(&Bloop<Player>) -> ExtractResult<V> + Send + Sync + 'static,
        V: Eq + Hash + 'static,
    {
        fn derive_ctx<Player, State, Trigger>(_ctx: &AchievementContext<Player, State, Trigger>) {}
        let extract_wrapper = move |bloop: &Bloop<Player>, _: &()| extract(bloop);

        DistinctValuesEvaluator {
            min_required: self.min_required.0,
            max_window: self.max_window.0,
            award_mode: self.award_mode,
            derive_ctx,
            extract: extract_wrapper,
            _marker: PhantomData,
        }
    }

    /// Build the evaluator with additionally derived context.
    ///
    /// The derived context can be used to initially retrieve values from the
    /// achievement context and have them available in the extractor.
    pub fn build_with_derived_ctx<Player, State, Trigger, V, C, DC, E>(
        self,
        derive_ctx: DC,
        extract: E,
    ) -> impl Evaluator<Player, State, Trigger> + Debug
    where
        DC: Fn(&AchievementContext<Player, State, Trigger>) -> C + Send + Sync + 'static,
        E: Fn(&Bloop<Player>, &C) -> ExtractResult<V> + Send + Sync + 'static,
        V: Eq + Hash + 'static,
    {
        DistinctValuesEvaluator {
            min_required: self.min_required.0,
            max_window: self.max_window.0,
            award_mode: self.award_mode,
            derive_ctx,
            extract,
            _marker: PhantomData,
        }
    }
}

/// Evaluator that collects distinct `V` values from recent client bloops.
///
/// This evaluator awards the current or all participating players when the
/// collected values reach the `min_required` count.
pub struct DistinctValuesEvaluator<Player, State, Trigger, V, C, DC, E>
where
    DC: Fn(&AchievementContext<Player, State, Trigger>) -> C + Send + Sync + 'static,
    E: Fn(&Bloop<Player>, &C) -> ExtractResult<V> + Send + Sync + 'static,
    V: Eq + Hash + 'static,
{
    min_required: usize,
    max_window: Duration,
    award_mode: AwardMode,
    derive_ctx: DC,
    extract: E,
    _marker: PhantomData<(Player, State, Trigger, V)>,
}

impl<Player, State, Trigger, V, C, DC, E>
    DistinctValuesEvaluator<Player, State, Trigger, V, C, DC, E>
where
    DC: Fn(&AchievementContext<Player, State, Trigger>) -> C + Send + Sync + 'static,
    E: Fn(&Bloop<Player>, &C) -> ExtractResult<V> + Send + Sync + 'static,
    V: Eq + Hash + 'static,
{
    pub fn builder() -> DistinctValuesEvaluatorBuilder<NoValue, NoValue> {
        DistinctValuesEvaluatorBuilder::new()
    }
}

impl<Player, State, Trigger, V, C, DC, E> Evaluator<Player, State, Trigger>
    for DistinctValuesEvaluator<Player, State, Trigger, V, C, DC, E>
where
    DC: Fn(&AchievementContext<Player, State, Trigger>) -> C + Send + Sync + 'static,
    E: Fn(&Bloop<Player>, &C) -> ExtractResult<V> + Send + Sync + 'static,
    V: Eq + Hash + 'static,
{
    fn evaluate(&self, ctx: &AchievementContext<Player, State, Trigger>) -> impl Into<EvalResult> {
        let derived_ctx = (self.derive_ctx)(ctx);
        let mut seen_values = HashSet::with_capacity(self.min_required);
        let mut player_ids = Vec::with_capacity(self.min_required + 1);
        player_ids.push(ctx.current_bloop.player_id);

        let bloops = ctx
            .client_bloops()
            .filter(ctx.filter_within_window(self.max_window))
            .take(self.min_required);

        for bloop in bloops {
            if player_ids.contains(&bloop.player_id) {
                return EvalResult::NoAward;
            }

            player_ids.push(bloop.player_id);

            let extract_result = (self.extract)(bloop, &derived_ctx);

            match extract_result {
                ExtractResult::Single(value) => {
                    seen_values.insert(value);
                }
                ExtractResult::Multiple(values) => seen_values.extend(values),
                ExtractResult::Abort => return EvalResult::NoAward,
            };

            if seen_values.len() >= self.min_required {
                break;
            }
        }

        if seen_values.len() < self.min_required {
            return EvalResult::NoAward;
        }

        match self.award_mode {
            AwardMode::Current => EvalResult::AwardSelf,
            AwardMode::All => EvalResult::AwardMultiple(player_ids),
        }
    }
}

impl<Player, State, Trigger, V, C, DC, E> Debug
    for DistinctValuesEvaluator<Player, State, Trigger, V, C, DC, E>
where
    DC: Fn(&AchievementContext<Player, State, Trigger>) -> C + Send + Sync + 'static,
    E: Fn(&Bloop<Player>, &C) -> ExtractResult<V> + Send + Sync + 'static,
    V: Eq + Hash + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DistinctValuesEvaluator")
            .field("derive_ctx", &"<closure>")
            .field("extract", &"<closure>")
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
    use std::vec;

    fn make_bloop(client_id: &str, name: &str, seconds_ago: u64) -> Bloop<MockPlayer> {
        let now = SystemTime::now() - Duration::from_secs(seconds_ago);
        let timestamp: DateTime<Utc> = now.into();

        let (player, _) = MockPlayer::builder().name(name).build();

        Bloop::new(player, client_id, timestamp)
    }

    #[test]
    fn award_when_required_distinct_values_are_met() {
        let evaluator = DistinctValuesEvaluatorBuilder::new()
            .min_required(3)
            .max_window(Duration::from_secs(60))
            .build(|bloop: &Bloop<MockPlayer>| ExtractResult::Single(bloop.player().name.clone()));

        let current = make_bloop("client1", "alice", 0);
        let past = vec![
            make_bloop("client1", "bob", 10),
            make_bloop("client1", "carol", 20),
            make_bloop("client1", "dave", 30),
        ];

        let mut ctx_builder = TestCtxBuilder::new(current).bloops(past);
        let res: EvalResult = evaluator.evaluate(&ctx_builder.build()).into();

        assert_eq!(res, EvalResult::AwardSelf);
    }

    #[test]
    fn award_all_mode_returns_all_player_ids() {
        let evaluator = DistinctValuesEvaluatorBuilder::new()
            .min_required(3)
            .max_window(Duration::from_secs(60))
            .award_all()
            .build(|bloop: &Bloop<MockPlayer>| ExtractResult::Single(bloop.player().name.clone()));

        let current = make_bloop("client1", "alice", 0);
        let b1 = make_bloop("client1", "bob", 10);
        let b2 = make_bloop("client1", "carol", 20);
        let b3 = make_bloop("client1", "dave", 30);
        let b4 = make_bloop("client1", "joe", 40);

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
    fn abort_from_extractor_leads_to_no_award() {
        let evaluator = DistinctValuesEvaluatorBuilder::new()
            .min_required(1)
            .max_window(Duration::from_secs(60))
            .build(|_bloop: &Bloop<MockPlayer>| ExtractResult::<()>::Abort);

        let current = make_bloop("client1", "alice", 0);
        let b1 = make_bloop("client1", "bob", 10);

        let mut ctx_builder = TestCtxBuilder::new(current).bloops(vec![b1]);
        let res: EvalResult = evaluator.evaluate(&ctx_builder.build()).into();

        assert_eq!(res, EvalResult::NoAward);
    }

    #[test]
    fn duplicate_player_in_recent_bloops_causes_no_award() {
        let evaluator = DistinctValuesEvaluatorBuilder::new()
            .min_required(2)
            .max_window(Duration::from_secs(60))
            .build(|bloop: &Bloop<MockPlayer>| ExtractResult::Single(bloop.player().name.clone()));

        let current = make_bloop("client1", "alice", 0);

        let b1 = make_bloop("client1", "bob", 10);
        let mut b2 = make_bloop("client1", "carol", 10);
        b2.player_id = b1.player_id;

        let mut ctx_builder = TestCtxBuilder::new(current).bloops(vec![b1, b2]);
        let res: EvalResult = evaluator.evaluate(&ctx_builder.build()).into();

        assert_eq!(res, EvalResult::NoAward);
    }

    #[test]
    fn ignores_bloops_outside_time_window() {
        let evaluator = DistinctValuesEvaluatorBuilder::new()
            .min_required(2)
            .max_window(Duration::from_secs(20))
            .build(|bloop: &Bloop<MockPlayer>| ExtractResult::Single(bloop.player().name.clone()));

        let current = make_bloop("client1", "alice", 0);
        let b1 = make_bloop("client1", "bob", 10);
        let b2 = make_bloop("client1", "carol", 30);

        let mut ctx_builder = TestCtxBuilder::new(current).bloops(vec![b1, b2]);
        let res: EvalResult = evaluator.evaluate(&ctx_builder.build()).into();

        assert_eq!(res, EvalResult::NoAward);
    }

    #[test]
    fn build_with_derived_ctx_supplies_context_to_extractor() {
        let evaluator = DistinctValuesEvaluatorBuilder::new()
            .min_required(2)
            .max_window(Duration::from_secs(60))
            .build_with_derived_ctx(
                |_ctx| vec!["bob", "carol"],
                |bloop: &Bloop<MockPlayer>, allowed: &Vec<&str>| {
                    if allowed.contains(&bloop.player().name.as_str()) {
                        ExtractResult::Single(bloop.player().name.clone())
                    } else {
                        ExtractResult::Abort
                    }
                },
            );

        let current = make_bloop("client1", "alice", 0);
        let past = vec![
            make_bloop("client1", "bob", 10),
            make_bloop("client1", "carol", 20),
        ];

        let mut ctx_builder = TestCtxBuilder::new(current).bloops(past);
        let res: EvalResult = evaluator.evaluate(&ctx_builder.build()).into();

        assert_eq!(res, EvalResult::AwardSelf);
    }
}
