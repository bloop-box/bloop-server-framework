use crate::achievement::AchievementContext;
use crate::evaluator::SingleEvaluator;
use crate::player::PlayerInfo;
use std::fmt::Debug;

/// Evaluates that the player has at least a given number of total bloops.
///
/// This evaluator returns `true` if the total number of bloops associated with
/// the player meets or exceeds the configured minimum count. It can be used to
/// gate achievements based on accumulated activity.
#[derive(Debug)]
pub struct MinBloopsEvaluator {
    min_count: usize,
}

impl MinBloopsEvaluator {
    /// Creates a new [`MinBloopsEvaluator`] with a given min count.
    ///
    /// # Examples
    ///
    /// ```
    /// use bloop_server_framework::evaluator::min_bloops::MinBloopsEvaluator;
    ///
    /// let evaluator = MinBloopsEvaluator::new(10_000);
    /// ```
    pub fn new(min_count: usize) -> Self {
        Self { min_count }
    }
}

impl<Player: PlayerInfo, Metadata, Trigger> SingleEvaluator<Player, Metadata, Trigger>
    for MinBloopsEvaluator
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    fn evaluate(&self, ctx: &AchievementContext<Player, Metadata, Trigger>) -> bool {
        ctx.current_bloop.player().total_bloops() >= self.min_count
    }
}

#[cfg(test)]
mod tests {
    use crate::bloop::Bloop;
    use crate::evaluator::SingleEvaluator;
    use crate::evaluator::min_bloops::MinBloopsEvaluator;
    use crate::evaluator::test_utils::{MockPlayer, TestCtxBuilder};
    use crate::test_utils::Utc;

    #[test]
    fn returns_true_when_enough_bloops() {
        let (player, _) = MockPlayer::builder().bloops_count(5).build();
        let bloop = Bloop::new(player.clone(), "client1", Utc::now());
        let mut builder = TestCtxBuilder::new(bloop);
        let ctx = builder.build();

        let evaluator = MinBloopsEvaluator::new(3);
        assert!(evaluator.evaluate(&ctx));
    }

    #[test]
    fn returns_false_when_not_enough_bloops() {
        let (player, _) = MockPlayer::builder().bloops_count(2).build();
        let bloop = Bloop::new(player.clone(), "client1", Utc::now());
        let mut builder = TestCtxBuilder::new(bloop);
        let ctx = builder.build();

        let evaluator = MinBloopsEvaluator::new(3);
        assert!(!evaluator.evaluate(&ctx));
    }
}
