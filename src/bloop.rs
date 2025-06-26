use crate::event::Event;
use crate::player::PlayerInfo;
use async_trait::async_trait;
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
    pub fn new(player: &Arc<RwLock<Player>>, client_id: String) -> Self {
        Bloop {
            player_id: player.read().unwrap().id(),
            player: player.clone(),
            client_id,
            recorded_at: Utc::now(),
        }
    }
}

impl<Player> Bloop<Player> {
    /// Returns the player of the bloop.
    ///
    /// If you only need to access to player ID, consider using `bloop.player_id` instead. In case
    /// you need to access multiple properties, store the returned player in a variable to avoid
    /// multiple read locks.
    pub fn player(&self) -> RwLockReadGuard<Player> {
        self.player.read().unwrap()
    }
}

#[inline]
pub fn bloops_since<Player>(since: DateTime<Utc>) -> impl Fn(&&Arc<Bloop<Player>>) -> bool {
    move |bloop| bloop.recorded_at >= since
}

#[inline]
pub fn bloops_for_player<Player>(player_id: Uuid) -> impl Fn(&&Arc<Bloop<Player>>) -> bool {
    move |bloop| bloop.player_id == player_id
}

#[derive(Debug)]
pub struct BloopCollection<Player> {
    bloops: VecDeque<Arc<Bloop<Player>>>,
    max_age: Duration,
}

impl<Player> BloopCollection<Player> {
    pub fn new(max_age: Duration) -> Self {
        Self {
            bloops: VecDeque::new(),
            max_age,
        }
    }

    pub fn with_bloops(max_age: Duration, mut bloops: Vec<Arc<Bloop<Player>>>) -> Self {
        bloops.sort_by_key(|bloop| Reverse(bloop.recorded_at));

        let mut collection = Self::new(max_age);
        collection.bloops.extend(bloops);

        collection
    }

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

    /// Returns an iterator over all bloops
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

#[derive(Debug)]
pub struct BloopProvider<Player> {
    global: BloopCollection<Player>,
    per_client: HashMap<String, BloopCollection<Player>>,
    max_age: Duration,
    empty_collection: BloopCollection<Player>,
}

impl<Player> BloopProvider<Player> {
    pub fn new(max_age: Duration) -> Self {
        Self {
            global: BloopCollection::new(max_age),
            per_client: HashMap::new(),
            max_age,
            empty_collection: BloopCollection::new(max_age),
        }
    }

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

    pub fn add(&mut self, bloop: Arc<Bloop<Player>>) {
        self.global.add(bloop.clone());

        let client_collection = self
            .per_client
            .entry(bloop.client_id.clone())
            .or_insert_with(|| BloopCollection::new(self.max_age));

        client_collection.add(bloop.clone());
    }

    pub fn global(&self) -> &BloopCollection<Player> {
        &self.global
    }

    pub fn for_client(&self, client_id: &str) -> &BloopCollection<Player> {
        self.per_client
            .get(client_id)
            .unwrap_or(&self.empty_collection)
    }
}

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

#[async_trait]
pub trait BloopRepository {
    type Error: std::fmt::Debug + std::fmt::Display + Send + Sync + 'static;

    async fn persist_batch(&self, bloops: &[ProcessedBloop]) -> Result<(), Self::Error>;
}

#[derive(Debug, Error)]
enum SinkError {
    #[error("Repository error: {0}")]
    Repository(#[from] Box<dyn std::error::Error + Send + Sync + 'static>),
}

pub struct ProcessedBloopSink<R: BloopRepository> {
    repository: R,
    buffer: Vec<ProcessedBloop>,
    max_batch_size: usize,
    max_batch_duration: Duration,
    event_rx: broadcast::Receiver<Event>,
}

impl<R: BloopRepository> ProcessedBloopSink<R> {
    pub async fn process_events(&mut self) {
        let mut flush_interval = time::interval(self.max_batch_duration);

        loop {
            let should_continue = select! {
                _ = flush_interval.tick() => {
                    if !self.buffer.is_empty() {
                        self.flush().await;
                    }

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
#[async_trait]
impl<R> IntoSubsystem<NeverError> for ProcessedBloopSink<R>
where
    R: BloopRepository + Send + Sync + 'static,
{
    async fn run(mut self, subsys: SubsystemHandle) -> Result<(), NeverError> {
        let _ = self.process_events().cancel_on_shutdown(&subsys).await;
        self.flush().await;

        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum BuilderError {
    #[error("missing field: {0}")]
    MissingField(&'static str),
}

pub struct ProcessedBloopSinkBuilder<R: BloopRepository> {
    repository: R,
    max_batch_size: Option<usize>,
    max_batch_duration: Option<Duration>,
    event_rx: Option<broadcast::Receiver<Event>>,
}

impl<R: BloopRepository> ProcessedBloopSinkBuilder<R> {
    pub fn new(repository: R) -> Self {
        Self {
            repository,
            max_batch_size: None,
            max_batch_duration: None,
            event_rx: None,
        }
    }

    pub fn max_batch_size(mut self, size: usize) -> Self {
        self.max_batch_size = Some(size);
        self
    }

    pub fn max_batch_duration(mut self, duration: Duration) -> Self {
        self.max_batch_duration = Some(duration);
        self
    }

    pub fn event_rx(mut self, event_rx: broadcast::Receiver<Event>) -> Self {
        self.event_rx = Some(event_rx);
        self
    }

    pub fn build(self) -> Result<ProcessedBloopSink<R>, BuilderError> {
        Ok(ProcessedBloopSink {
            repository: self.repository,
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
