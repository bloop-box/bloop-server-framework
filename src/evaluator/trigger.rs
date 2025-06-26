use crate::achievement::AchievementContext;
use crate::evaluator::SingleEvaluator;
use std::fmt::Debug;

#[derive(Debug)]
pub struct TriggerEvaluator<Trigger>
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    trigger: Trigger,
}

impl<Trigger> TriggerEvaluator<Trigger>
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    pub fn new(trigger: Trigger) -> Self {
        Self { trigger }
    }
}

impl<Player, Metadata, Trigger> SingleEvaluator<Player, Metadata, Trigger>
    for TriggerEvaluator<Trigger>
where
    Trigger: Copy + PartialEq + Eq + Debug + Send + Sync,
{
    fn evaluate(&self, ctx: &AchievementContext<Player, Metadata, Trigger>) -> bool {
        ctx.has_trigger(self.trigger)
    }
}
