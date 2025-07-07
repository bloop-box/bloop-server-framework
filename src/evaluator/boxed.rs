use crate::achievement::AchievementContext;
use crate::evaluator::{EvalResult, Evaluator};
use std::fmt;

pub trait DynEvaluator<Player, State, Trigger>: fmt::Debug + Send + Sync {
    fn evaluate(&self, ctx: &AchievementContext<Player, State, Trigger>) -> EvalResult;
}

struct ErasedEvaluator<T: fmt::Debug + Send + Sync>(T);

impl<T: fmt::Debug + Send + Sync> fmt::Debug for ErasedEvaluator<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<Player, Metadata, Trigger, T> DynEvaluator<Player, Metadata, Trigger> for ErasedEvaluator<T>
where
    T: Evaluator<Player, Metadata, Trigger> + fmt::Debug + Send + Sync,
{
    fn evaluate(&self, ctx: &AchievementContext<Player, Metadata, Trigger>) -> EvalResult {
        self.0.evaluate(ctx).into()
    }
}

#[allow(dead_code)]
struct OpaqueEvaluator<T>(T);

impl<Player, Metadata, Trigger, T> Evaluator<Player, Metadata, Trigger> for OpaqueEvaluator<T>
where
    T: Evaluator<Player, Metadata, Trigger>,
{
    fn evaluate(
        &self,
        ctx: &AchievementContext<Player, Metadata, Trigger>,
    ) -> impl Into<EvalResult> {
        self.0.evaluate(ctx)
    }
}

impl<T> fmt::Debug for OpaqueEvaluator<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OpaqueEvaluator").finish()
    }
}

pub trait IntoDynEvaluator<Player, State, Trigger> {
    fn into_dyn_evaluator(self) -> Box<dyn DynEvaluator<Player, State, Trigger>>;
}

#[cfg(feature = "evaluator-debug")]
impl<Player, State, Trigger, T> IntoDynEvaluator<Player, State, Trigger> for T
where
    T: Evaluator<Player, State, Trigger> + fmt::Debug + Send + Sync + 'static,
{
    fn into_dyn_evaluator(self) -> Box<dyn DynEvaluator<Player, State, Trigger>> {
        Box::new(ErasedEvaluator(self))
    }
}

#[cfg(not(feature = "evaluator-debug"))]
impl<Player, State, Trigger, T> IntoDynEvaluator<Player, State, Trigger> for T
where
    T: Evaluator<Player, State, Trigger> + Send + Sync + 'static,
{
    fn into_dyn_evaluator(self) -> Box<dyn DynEvaluator<Player, State, Trigger>> {
        Box::new(ErasedEvaluator(OpaqueEvaluator(self)))
    }
}
