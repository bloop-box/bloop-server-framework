use crate::player::Player;
use async_trait::async_trait;
use bloop_server_framework::bloop::{Bloop, BloopRepository, ProcessedBloop};
use bloop_server_framework::player::PlayerRegistry;
use std::convert::Infallible;
use std::time::Duration;

pub struct DummyBloopRepository;

impl DummyBloopRepository {
    pub async fn load_recent(
        &self,
        _players: &PlayerRegistry<Player>,
        _duration: Duration,
    ) -> Result<Vec<Bloop<Player>>, Infallible> {
        // Load all players from your database, backfilling the actual player from the player
        // registry.
        Ok(Vec::new())
    }
}

#[async_trait]
impl BloopRepository for DummyBloopRepository {
    type Error = anyhow::Error;

    async fn persist_batch(&self, _bloops: &[ProcessedBloop]) -> Result<(), Self::Error> {
        // Persist all bloops into your database, ideally with a single atomic action.
        Ok(())
    }
}
