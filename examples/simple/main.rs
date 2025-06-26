mod bloop;
mod player;

use crate::bloop::DummyBloopRepository;
use crate::player::{AwardedAchievementsPersister, DummyPlayerRepository, Player};
use argon2::password_hash::PasswordHashString;
use bloop_server_framework::achievement::AchievementDefinition;
use bloop_server_framework::bloop::ProcessedBloopSinkBuilder;
use bloop_server_framework::engine::EngineBuilder;
use bloop_server_framework::evaluator::SingleEvaluator;
use bloop_server_framework::evaluator::min_bloops::MinBloopsEvaluator;
use bloop_server_framework::network::NetworkListenerBuilder;
use bloop_server_framework::player::PlayerRegistry;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio_graceful_shutdown::{IntoSubsystem, SubsystemBuilder, Toplevel};
use uuid::uuid;

/// Shortcut to make it easier to define a bunch of achievements without having to repeat generics.
pub type Achievement = bloop_server_framework::achievement::Achievement<Player, (), ()>;

const BLOOP_RETENTION: Duration = Duration::from_secs(60 * 30);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let achievement = Achievement::new(
        AchievementDefinition {
            id: uuid!("565fd1d0-d973-4870-b9db-9574fa6d81d2"),
            title: "First Bloop".to_string(),
            description: "You gained your first bloop!".to_string(),
            points: 1,
            is_hidden: true,
        },
        "audio.mp3",
        MinBloopsEvaluator::new(1).into_evaluator(),
    );
    let achievements = vec![achievement];

    let player_repository = DummyPlayerRepository;
    let players = player_repository.load_all().await?;
    let player_registry = PlayerRegistry::new(players);

    let bloop_repository = DummyBloopRepository;
    let bloops = bloop_repository
        .load_recent(&player_registry, BLOOP_RETENTION)
        .await?;

    let player_registry = Arc::new(Mutex::new(player_registry));

    let mut clients = HashMap::new();
    // Secret is "test"
    clients.insert(
        "test".to_string(),
        PasswordHashString::new(
            "$argon2id$v=19$m=16,t=2,p=1$NEZFVERXYlFGVFA2SW1CVQ$9YXEyfXaMMr8XmP6D3x2pg",
        )
        .unwrap(),
    );
    let clients = Arc::new(RwLock::new(clients));

    let (engine_tx, engine_rx) = mpsc::channel(512);
    let (event_tx, event_rx) = broadcast::channel(512);

    let engine = EngineBuilder::new()
        .player_registry(player_registry)
        .achievements(achievements)
        .bloops(bloops)
        .bloop_retention(Duration::from_secs(60 * 30))
        .audio_base_path("./audio")
        .network_rx(engine_rx)
        .event_tx(event_tx.clone())
        .build()?;

    let network_listener = NetworkListenerBuilder::new()
        .address("[::]:4500")
        .cert_path("examples/cert.pem")
        .key_path("examples/key.pem")
        .clients(clients)
        .event_tx(event_tx.clone())
        .engine_tx(engine_tx)
        .build()?;

    let processed_bloop_sink = ProcessedBloopSinkBuilder::new(bloop_repository)
        .max_batch_size(500)
        .max_batch_duration(Duration::from_secs(60))
        .event_rx(event_rx)
        .build()?;

    let awarded_achievements_persister = AwardedAchievementsPersister {
        event_rx: event_tx.subscribe(),
    };

    Toplevel::new(|s| async move {
        s.start(SubsystemBuilder::new("Engine", engine.into_subsystem()));
        s.start(SubsystemBuilder::new(
            "NetworkListener",
            network_listener.into_subsystem(),
        ));
        s.start(SubsystemBuilder::new(
            "ProcessedBloopSink",
            processed_bloop_sink.into_subsystem(),
        ));
        s.start(SubsystemBuilder::new(
            "AwardedAchievementsPersister",
            awarded_achievements_persister.into_subsystem(),
        ));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_secs(1))
    .await
    .map_err(Into::into)
}
