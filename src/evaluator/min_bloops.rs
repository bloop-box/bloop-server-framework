use crate::achievement::AchievementContext;
use crate::evaluator::SingleEvaluator;
use crate::player::PlayerInfo;
use std::fmt::Debug;

/// Evaluates that the player has at least a given number of total bloops.
///
/// This evaluator returns `true` if the total number of bloops associated with the player meets or
/// exceeds the configured minimum count. It can be used to gate achievements based on accumulated
/// activity.
#[derive(Debug)]
pub struct MinBloopsEvaluator {
    min_count: usize,
}

impl MinBloopsEvaluator {
    /// Creates a new [MinBloopsEvaluator] with a given min count.
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
