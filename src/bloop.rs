//! Module for handling NFC scan events ("bloops").
//!
//! This module defines:
//!
//! - [`Bloop`]: Represents a single NFC scan event tied to a player and client.
//! - [`BloopCollection`]: A time-limited collection of bloops with pruning of
//!   old entries.
//! - [`BloopProvider`]: Provides access to bloops globally and per client.
//! - [`ProcessedBloop`]: A simplified representation for persistence.
//! - [`BloopRepository`]: Async trait abstraction for persisting processed
//!   bloops.
//! - [`ProcessedBloopSink`]: Buffers and persists bloops in batches driven by
//!   event notifications.
//!
//! The system enables tracking and managing bloops efficiently with
//! asynchronous persistence. Features optional integration with graceful
//! shutdown via the `tokio-graceful-shutdown` crate.

use crate::event::Event;
use crate::player::PlayerInfo;
use chrono::{DateTime, Utc};
use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque, vec_deque};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::{select, time};
#[cfg(feature = "tokio-graceful-shutdown")]
use tokio_graceful_shutdown::{FutureExt, IntoSubsystem, SubsystemHandle};
use tracing::{debug, error, warn};
use uuid::Uuid;

/// Represents a single NFC tag scan ("bloop") event.
///
/// Each `Bloop` captures the details of an NFC scan performed by a player on a
/// specific client device, including when the scan was recorded.
#[derive(Debug, Clone)]
pub struct Bloop<Player> {
    /// The player who blooped,
    player: Arc<RwLock<Player>>,

    /// Player ID extracted from the assigned player.
    ///
    /// The player ID is accessible individually as it is immutable and thus does not require a read
    /// lock on the player to be obtained.
    pub player_id: Uuid,

    /// The client ID this bloop occurred at.
    pub client_id: String,

    /// The time this bloop was recorded,
    pub recorded_at: DateTime<Utc>,
}

impl<Player: PlayerInfo> Bloop<Player> {
    /// Creates a new `Bloop` instance, extracting the player ID from the provided
    /// player.
    pub fn new(
        player: Arc<RwLock<Player>>,
        client_id: impl Into<String>,
        recorded_at: DateTime<Utc>,
    ) -> Self {
        let player_id = player.read().unwrap().id();

        Bloop {
            player_id,
            player,
            client_id: client_id.into(),
            recorded_at,
        }
    }
}

impl<Player> Bloop<Player> {
    /// Returns a read lock guard to the player associated with this bloop.
    ///
    /// If only the player ID is needed, use the `player_id` field directly.
    /// For multiple accesses, cache the guard to minimize locking overhead.
    pub fn player(&self) -> RwLockReadGuard<'_, Player> {
        self.player.read().unwrap()
    }
}

/// Returns a closure to filter bloops recorded at or after a given timestamp.
#[inline]
pub fn bloops_since<Player>(since: DateTime<Utc>) -> impl Fn(&&Arc<Bloop<Player>>) -> bool {
    move |bloop| bloop.recorded_at >= since
}

/// Returns a closure to filter bloops belonging to a specific player.
#[inline]
pub fn bloops_for_player<Player>(player_id: Uuid) -> impl Fn(&&Arc<Bloop<Player>>) -> bool {
    move |bloop| bloop.player_id == player_id
}

/// A collection of bloops with a maximum age threshold.
///
/// Automatically prunes bloops which are older than the specified maximum age.
#[derive(Debug)]
pub struct BloopCollection<Player> {
    bloops: VecDeque<Arc<Bloop<Player>>>,
    max_age: Duration,
}

impl<Player> BloopCollection<Player> {
    /// Creates an empty collection with the specified maximum age.
    pub fn new(max_age: Duration) -> Self {
        Self {
            bloops: VecDeque::new(),
            max_age,
        }
    }

    /// Creates a collection pre-populated with bloops, sorted by most recent.
    pub fn with_bloops(max_age: Duration, mut bloops: Vec<Arc<Bloop<Player>>>) -> Self {
        bloops.sort_by_key(|bloop| Reverse(bloop.recorded_at));

        let mut collection = Self::new(max_age);
        collection.bloops.extend(bloops);

        collection
    }

    /// Adds a bloop to the collection, pruning old bloops exceeding the max age.
    pub fn add(&mut self, bloop: Arc<Bloop<Player>>) {
        let threshold = Utc::now() - self.max_age;

        while let Some(oldest) = self.bloops.back() {
            if oldest.recorded_at < threshold {
                self.bloops.pop_back();
            } else {
                break;
            }
        }

        self.bloops.push_front(bloop);
    }

    /// Returns an iterator over all bloops in the collection.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Bloop<Player>>> {
        self.bloops.iter()
    }
}

impl<'a, Player: 'a> IntoIterator for &'a BloopCollection<Player> {
    type Item = &'a Arc<Bloop<Player>>;
    type IntoIter = vec_deque::Iter<'a, Arc<Bloop<Player>>>;

    fn into_iter(self) -> Self::IntoIter {
        self.bloops.iter()
    }
}

/// Provides access to bloops globally and per client, respecting a maximum age.
#[derive(Debug)]
pub struct BloopProvider<Player> {
    global: BloopCollection<Player>,
    per_client: HashMap<String, BloopCollection<Player>>,
    max_age: Duration,
    empty_collection: BloopCollection<Player>,
}

impl<Player> BloopProvider<Player> {
    /// Creates a new provider with no bloops.
    pub fn new(max_age: Duration) -> Self {
        Self {
            global: BloopCollection::new(max_age),
            per_client: HashMap::new(),
            max_age,
            empty_collection: BloopCollection::new(max_age),
        }
    }

    /// Creates a provider initialized with a set of bloops.
    pub fn with_bloops(max_age: Duration, bloops: Vec<Bloop<Player>>) -> Self {
        let bloops: Vec<Arc<Bloop<Player>>> = bloops.into_iter().map(Arc::new).collect();
        let global_collection = BloopCollection::with_bloops(max_age, bloops.clone());

        let mut per_client: HashMap<String, BloopCollection<Player>> = HashMap::new();
        let mut client_groups: HashMap<String, Vec<Arc<Bloop<Player>>>> = HashMap::new();

        for bloop in bloops {
            client_groups
                .entry(bloop.client_id.clone())
                .or_default()
                .push(bloop.clone());
        }

        for (client_id, client_bloops) in client_groups {
            let collection = BloopCollection::with_bloops(max_age, client_bloops);
            per_client.insert(client_id, collection);
        }

        Self {
            global: global_collection,
            per_client,
            max_age,
            empty_collection: BloopCollection::new(max_age),
        }
    }

    /// Adds a bloop to the global and client-specific collections.
    pub fn add(&mut self, bloop: Arc<Bloop<Player>>) {
        self.global.add(bloop.clone());

        let client_collection = self
            .per_client
            .entry(bloop.client_id.clone())
            .or_insert_with(|| BloopCollection::new(self.max_age));

        client_collection.add(bloop.clone());
    }

    /// Returns a reference to the global collection of bloops.
    pub fn global(&self) -> &BloopCollection<Player> {
        &self.global
    }

    /// Returns a reference to the collection of bloops for a specific client.
    pub fn for_client(&self, client_id: &str) -> &BloopCollection<Player> {
        self.per_client
            .get(client_id)
            .unwrap_or(&self.empty_collection)
    }
}

/// A simplified representation of a bloop suitable for persistence.
#[derive(Debug, Clone)]
pub struct ProcessedBloop {
    /// Player ID associated with the bloop.
    pub player_id: Uuid,

    /// The client ID this bloop occurred at.
    pub client_id: String,

    /// The time this bloop was recorded.
    pub recorded_at: DateTime<Utc>,
}

impl<Player> From<&Bloop<Player>> for ProcessedBloop {
    fn from(bloop: &Bloop<Player>) -> Self {
        Self {
            player_id: bloop.player_id,
            client_id: bloop.client_id.clone(),
            recorded_at: bloop.recorded_at,
        }
    }
}

/// Interface for persisting processed bloops asynchronously.
pub trait BloopRepository {
    type Error: std::fmt::Debug + std::fmt::Display + Send + Sync + 'static;

    fn persist_batch(
        &self,
        bloops: &[ProcessedBloop],
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// A sink that buffers processed bloops and persists them in batches.
#[derive(Debug)]
pub struct ProcessedBloopSink<R: BloopRepository> {
    repository: R,
    buffer: Vec<ProcessedBloop>,
    max_batch_size: usize,
    max_batch_duration: Duration,
    event_rx: broadcast::Receiver<Event>,
}

impl<R: BloopRepository> ProcessedBloopSink<R> {
    /// Processes incoming events, buffering bloops and persisting in batches.
    pub async fn process_events(&mut self) {
        let mut flush_interval = time::interval(self.max_batch_duration);

        loop {
            let should_continue = select! {
                _ = flush_interval.tick() => {
                    self.flush().await;
                    true
                }

                result = self.event_rx.recv() => {
                    self.handle_recv(result).await
                }
            };

            if !should_continue {
                break;
            }
        }
    }

    /// Flushes the buffered bloops to the repository.
    pub async fn flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }

        let batch = std::mem::take(&mut self.buffer);

        if let Err(err) = self.repository.persist_batch(&batch).await {
            error!("Failed to persist bloop batch: {}", err);
            self.buffer.extend(batch);
        } else {
            debug!("Persisted {} bloops", batch.len());
        }
    }

    async fn handle_recv(&mut self, result: Result<Event, RecvError>) -> bool {
        match result {
            Ok(Event::BloopProcessed(bloop)) => {
                self.buffer.push(bloop);

                if self.buffer.len() >= self.max_batch_size {
                    debug!("Batch size reached, flushing");
                    self.flush().await;
                }
                true
            }
            Ok(_) => true,
            Err(RecvError::Lagged(n)) => {
                warn!("ProcessedBloopSink lagged by {n} messages, some bloops were missed");
                self.flush().await;
                true
            }
            Err(RecvError::Closed) => {
                debug!("ProcessedBloopSink event stream closed, exiting event loop");
                false
            }
        }
    }
}

#[cfg(feature = "tokio-graceful-shutdown")]
#[derive(Debug, Error)]
pub enum NeverError {}

#[cfg(feature = "tokio-graceful-shutdown")]
impl<R> IntoSubsystem<NeverError> for ProcessedBloopSink<R>
where
    R: BloopRepository + Send + Sync + 'static,
{
    async fn run(mut self, subsys: &mut SubsystemHandle) -> Result<(), NeverError> {
        let _ = self.process_events().cancel_on_shutdown(subsys).await;
        self.flush().await;

        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum BuilderError {
    #[error("missing field: {0}")]
    MissingField(&'static str),
}

/// Builder for constructing a [`ProcessedBloopSink`].
#[derive(Debug, Default)]
pub struct ProcessedBloopSinkBuilder<R: BloopRepository> {
    repository: Option<R>,
    max_batch_size: Option<usize>,
    max_batch_duration: Option<Duration>,
    event_rx: Option<broadcast::Receiver<Event>>,
}

impl<R: BloopRepository> ProcessedBloopSinkBuilder<R> {
    /// Creates a new, empty builder.
    pub fn new() -> Self {
        Self {
            repository: None,
            max_batch_size: None,
            max_batch_duration: None,
            event_rx: None,
        }
    }

    /// Sets the repository used by the sink.
    pub fn repository(mut self, repository: R) -> Self {
        self.repository = Some(repository);
        self
    }

    /// Sets the maximum number of bloops to buffer before flushing.
    pub fn max_batch_size(mut self, size: usize) -> Self {
        self.max_batch_size = Some(size);
        self
    }

    /// Sets the maximum duration to wait before flushing the buffered bloops.
    pub fn max_batch_duration(mut self, duration: Duration) -> Self {
        self.max_batch_duration = Some(duration);
        self
    }

    /// Sets the event receiver from which bloops are received.
    pub fn event_rx(mut self, event_rx: broadcast::Receiver<Event>) -> Self {
        self.event_rx = Some(event_rx);
        self
    }

    /// Attempts to build the sink, returning an error if any required fields are
    /// missing.
    pub fn build(self) -> Result<ProcessedBloopSink<R>, BuilderError> {
        Ok(ProcessedBloopSink {
            repository: self
                .repository
                .ok_or(BuilderError::MissingField("repository"))?,
            buffer: Vec::new(),
            max_batch_size: self
                .max_batch_size
                .ok_or(BuilderError::MissingField("max_batch_size"))?,
            max_batch_duration: self
                .max_batch_duration
                .ok_or(BuilderError::MissingField("max_batch_duration"))?,
            event_rx: self
                .event_rx
                .ok_or(BuilderError::MissingField("event_rx"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bloop::{Bloop, BloopCollection, BloopProvider};
    use crate::test_utils::MockPlayer;
    use ntest::timeout;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn create_mock_bloop(recorded_at: DateTime<Utc>) -> Arc<Bloop<MockPlayer>> {
        let (player, _) = MockPlayer::builder().build();
        Arc::new(Bloop::new(player, "client-1", recorded_at))
    }

    #[test]
    fn bloop_collection_adds_bloops_and_preserves_order() {
        let now = Utc::now();
        let bloop1 = create_mock_bloop(now);
        let bloop2 = create_mock_bloop(now + Duration::from_secs(1));

        let mut collection = BloopCollection::new(Duration::from_secs(60));
        collection.add(bloop2.clone());
        collection.add(bloop1.clone());

        let bloops: Vec<_> = collection.iter().collect();
        assert_eq!(bloops.len(), 2);
        assert_eq!(bloops[0].recorded_at, bloop1.recorded_at);
        assert_eq!(bloops[1].recorded_at, bloop2.recorded_at);
    }

    #[test]
    fn bloop_collection_prunes_bloops_older_than_max_age() {
        let mut collection = BloopCollection::new(Duration::from_secs(5));
        let now = Utc::now();

        let old_bloop = create_mock_bloop(now - Duration::from_secs(10));
        let recent_bloop = create_mock_bloop(now - Duration::from_secs(2));

        collection.add(old_bloop);
        collection.add(recent_bloop.clone());

        let bloops: Vec<_> = collection.iter().collect();
        assert_eq!(bloops.len(), 1);
        assert_eq!(bloops[0].recorded_at, recent_bloop.recorded_at);
    }

    #[test]
    fn bloop_provider_adds_bloops_to_global_and_per_client_collections() {
        let mut provider = BloopProvider::new(Duration::from_secs(60));
        let now = Utc::now();
        let bloop = create_mock_bloop(now);
        let client_id = bloop.client_id.clone();

        provider.add(bloop.clone());

        let global_bloops: Vec<_> = provider.global().iter().collect();
        assert_eq!(global_bloops.len(), 1);
        assert_eq!(global_bloops[0].recorded_at, bloop.recorded_at);

        let client_bloops: Vec<_> = provider.for_client(&client_id).iter().collect();
        assert_eq!(client_bloops.len(), 1);
        assert_eq!(client_bloops[0].recorded_at, bloop.recorded_at);
    }

    #[test]
    fn bloop_provider_correctly_initializes_from_existing_bloops() {
        let now = Utc::now();
        let (player1, _) = MockPlayer::builder().build();
        let (player2, _) = MockPlayer::builder().build();

        let bloop1 = Bloop::new(player1.clone(), "client-1", now);
        let bloop2 = Bloop::new(player1, "client-1", now + Duration::from_secs(1));
        let bloop3 = Bloop::new(player2, "client-2", now);

        let provider =
            BloopProvider::with_bloops(Duration::from_secs(60), vec![bloop1, bloop2, bloop3]);

        let global_bloops: Vec<_> = provider.global().iter().collect();
        assert_eq!(global_bloops.len(), 3);

        let client1_bloops: Vec<_> = provider.for_client("client-1").iter().collect();
        assert_eq!(client1_bloops.len(), 2);
        assert!(client1_bloops.iter().any(|b| b.recorded_at == now));
        assert!(
            client1_bloops
                .iter()
                .any(|b| b.recorded_at == now + Duration::from_secs(1))
        );

        let client2_bloops: Vec<_> = provider.for_client("client-2").iter().collect();
        assert_eq!(client2_bloops.len(), 1);
        assert_eq!(client2_bloops[0].recorded_at, now);
    }

    #[test]
    fn bloops_since_filter_correctly_filters_based_on_timestamp() {
        let now = Utc::now();
        let bloop1 = create_mock_bloop(now);
        let bloop2 = create_mock_bloop(now + Duration::from_secs(10));

        let bloops = vec![bloop1.clone(), bloop2.clone()];
        let since = now + Duration::from_secs(5);
        let filtered: Vec<_> = bloops.iter().filter(bloops_since(since)).collect();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].recorded_at, bloop2.recorded_at);
    }

    #[test]
    fn bloops_for_player_filter_correctly_filters_based_on_player_id() {
        let now = Utc::now();
        let bloop1 = create_mock_bloop(now);
        let (player2, player2_id) = MockPlayer::builder().build();
        let bloop2 = Arc::new(Bloop::new(player2, "client-1", now));

        let bloops = vec![bloop1.clone(), bloop2.clone()];
        let filtered: Vec<_> = bloops
            .iter()
            .filter(bloops_for_player(player2_id))
            .collect();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].player_id, player2_id);
    }

    #[derive(Clone)]
    struct DummyRepo {
        sender: mpsc::UnboundedSender<Vec<ProcessedBloop>>,
        fail_persist: Arc<Mutex<bool>>,
    }

    impl DummyRepo {
        fn new(sender: mpsc::UnboundedSender<Vec<ProcessedBloop>>) -> Self {
            Self {
                sender,
                fail_persist: Arc::new(Mutex::new(false)),
            }
        }

        fn set_fail(&self, fail: bool) {
            *self.fail_persist.lock().unwrap() = fail;
        }
    }

    impl BloopRepository for DummyRepo {
        type Error = &'static str;

        async fn persist_batch(&self, bloops: &[ProcessedBloop]) -> Result<(), Self::Error> {
            if *self.fail_persist.lock().unwrap() {
                return Err("fail");
            }
            self.sender.send(bloops.to_vec()).unwrap();
            Ok(())
        }
    }

    fn create_processed_bloop() -> ProcessedBloop {
        ProcessedBloop {
            player_id: Uuid::new_v4(),
            client_id: "client-1".to_string(),
            recorded_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn flush_persists_and_clears_buffer() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let repo = DummyRepo::new(tx);

        let (_evt_tx, evt_rx) = broadcast::channel(16);

        let mut sink = ProcessedBloopSinkBuilder::new()
            .repository(repo.clone())
            .max_batch_size(10)
            .max_batch_duration(Duration::from_secs(5))
            .event_rx(evt_rx)
            .build()
            .unwrap();

        sink.buffer.push(create_processed_bloop());
        sink.flush().await;

        let batch = rx.recv().await.unwrap();
        assert_eq!(batch.len(), 1);
        assert!(sink.buffer.is_empty());
    }

    #[tokio::test]
    async fn flush_retries_on_failure() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let repo = DummyRepo::new(tx);
        repo.set_fail(true);

        let (_evt_tx, evt_rx) = broadcast::channel(16);

        let mut sink = ProcessedBloopSinkBuilder::new()
            .repository(repo.clone())
            .max_batch_size(10)
            .max_batch_duration(Duration::from_secs(5))
            .event_rx(evt_rx)
            .build()
            .unwrap();

        sink.buffer.push(create_processed_bloop());
        sink.flush().await;
        assert_eq!(sink.buffer.len(), 1);

        repo.set_fail(false);
        sink.flush().await;

        let batch = rx.recv().await.unwrap();
        assert_eq!(batch.len(), 1);
        assert!(sink.buffer.is_empty());
    }

    #[tokio::test]
    #[timeout(1000)]
    async fn process_events_flushes_on_batch_size() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let repo = DummyRepo::new(tx);
        let (evt_tx, evt_rx) = broadcast::channel(16);

        let mut sink = ProcessedBloopSinkBuilder::new()
            .repository(repo.clone())
            .max_batch_size(2)
            .max_batch_duration(Duration::from_secs(10))
            .event_rx(evt_rx)
            .build()
            .unwrap();

        let bloop1 = create_processed_bloop();
        let bloop2 = create_processed_bloop();

        let handle = tokio::spawn(async move { sink.process_events().await });

        evt_tx.send(Event::BloopProcessed(bloop1.clone())).unwrap();
        evt_tx.send(Event::BloopProcessed(bloop2.clone())).unwrap();

        let batch = rx.recv().await.unwrap();
        assert_eq!(batch.len(), 2);

        drop(evt_tx);

        handle.await.unwrap();
    }

    #[tokio::test]
    async fn handle_recv_returns_false_on_closed() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let repo = DummyRepo::new(tx);
        let (evt_tx, evt_rx) = broadcast::channel(16);

        let mut sink = ProcessedBloopSinkBuilder::new()
            .repository(repo.clone())
            .max_batch_size(10)
            .max_batch_duration(Duration::from_secs(10))
            .event_rx(evt_rx)
            .build()
            .unwrap();

        drop(evt_tx);

        let recv_result = sink.event_rx.recv().await;
        let result = sink.handle_recv(recv_result).await;
        assert!(!result);
    }
}
