use crate::achievement::AchievementContext;
use std::fmt::Debug;
use uuid::Uuid;

pub mod min_bloops;
pub mod registration_number;
pub mod spelling_bee;
pub mod time;
pub mod trigger;

pub trait SingleEvaluator<Player, Metadata, Trigger>: Send + Sync + Debug
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    fn evaluate(&self, ctx: &AchievementContext<Player, Metadata, Trigger>) -> bool;

    fn into_evaluator(self) -> Evaluator<Player, Metadata, Trigger>
    where
        Self: Sized + 'static,
    {
        Evaluator::Single(Box::new(self))
    }
}

pub trait MultiEvaluator<Player, Metadata, Trigger>: Send + Sync + Debug
where
    Trigger: Copy + PartialEq + Eq + Debug,
{
    fn evaluate(&self, ctx: &AchievementContext<Player, Metadata, Trigger>) -> Option<Vec<Uuid>>;

    fn into_evaluator(self) -> Evaluator<Player, Metadata, Trigger>
    where
        Self: Sized + 'static,
    {
        Evaluator::Multi(Box::new(self))
    }
}

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
