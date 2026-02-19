use crate::bloop::ProcessedBloop;
use crate::event::Event;
use bytes::Bytes;
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::Serialize;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{RwLock, broadcast};
use tokio::{io, select, task};
#[cfg(feature = "tokio-graceful-shutdown")]
use tokio_graceful_shutdown::{FutureExt, IntoSubsystem, SubsystemHandle};
use tracing::{debug, error, warn};

const MINUTES_IN_DAY: usize = 60 * 24;

/// Stats collected per client to track "bloops" over time.
///
/// This structure holds aggregated counters for the last 24 hours and beyond,
/// to efficiently support queries like "how many bloops in the last hour" or
/// "bloops per day". It is designed to be seeded from a database on startup.
///
/// # Seeding from PostgreSQL
///
/// To efficiently load these stats from a Postgres database with potentially
/// thousands of bloops per client, the following SQL queries are recommended.
///
/// ## Recommended index:
///
/// ```sql
/// CREATE INDEX idx_bloops_client_recorded_at ON bloops (
///     client_id,
///     recorded_at
/// );
/// ```
///
/// This index speeds up queries filtering by client and time range.
///
/// ## Total bloops per client:
///
/// ```sql
/// SELECT client_id, COUNT(*) as total_bloops
/// FROM bloops
/// GROUP BY client_id;
/// ```
///
/// ## Bloops per minute for the last 24 hours (UTC):
///
/// ```sql
/// SELECT client_id,
///        DATE_TRUNC(
///            'minute',
///            recorded_at AT TIME ZONE 'UTC'
///        ) AS minute_bucket,
///        COUNT(*) AS count
/// FROM bloops
/// WHERE recorded_at >= NOW() - INTERVAL '24 hours'
/// GROUP BY client_id, minute_bucket;
/// ```
///
/// - `minute_bucket` contains the UTC timestamp truncated to the minute.
/// - You can map `minute_bucket` to your in-memory bucket index like this:
///
/// ### Mapping to In-Memory Buckets:
///
/// You can map the `minute_bucket` to your in-memory buckets like this:
///
/// ```no_run
/// use chrono::{NaiveDateTime, Timelike};
/// use bloop_server_framework::statistics::{minute_index, ClientStats};
/// use bloop_server_framework::test_utils::Utc;
///
/// // Load these from your database
/// let minute_bucket = Utc::now().naive_utc();
/// let count = 0;
///
/// let mut client_stats = ClientStats::new();
/// let idx = minute_index(minute_bucket);
/// let date = minute_bucket.date();
///
/// client_stats.per_minute_dates[idx] = date;
/// client_stats.per_minute_bloops[idx] = count as u8;
/// ```
///
/// ## Bloops per hour for the last 24 hours (in local time):
///
/// ```sql
/// SELECT client_id,
///        EXTRACT(hour FROM recorded_at AT TIME ZONE 'your_timezone') AS hour,
///        COUNT(*) AS count
/// FROM bloops
/// WHERE recorded_at >= NOW() - INTERVAL '24 hours'
/// GROUP BY client_id, hour;
/// ```
///
/// ## Bloops per day (in local time):
///
/// ```sql
/// SELECT client_id,
///        DATE(recorded_at AT TIME ZONE 'your_timezone') AS day,
///        COUNT(*) AS count
/// FROM bloops
/// GROUP BY client_id, day;
/// ```
///
/// After executing these queries, aggregate the counts per client into the
/// fields of this struct.
///
/// # Example usage:
///
/// ```no_run
/// use std::collections::HashMap;
/// use bloop_server_framework::statistics::ClientStats;
///
/// let mut stats_map: HashMap<String, ClientStats> = HashMap::new();
///
/// // For each row of minute-buckets query:
/// // 1. Parse `minute_bucket` into NaiveDate and index.
/// // 2. Insert count into `per_minute_bloops` and `per_minute_dates`.
///
/// // Similarly for other queries...
/// ```
#[derive(Debug)]
pub struct ClientStats {
    /// Total bloops observed for this client.
    pub total_bloops: u64,

    /// Count of bloops per minute in a rolling 24-hour buffer.
    ///
    /// Indexed by minute of day (0..1439).
    pub per_minute_bloops: [u8; MINUTES_IN_DAY],

    /// The date associated with each minute bucket, used to invalidate stale data.
    pub per_minute_dates: [NaiveDate; MINUTES_IN_DAY],

    /// Count of bloops per hour in the local timezone.
    pub bloops_per_hour: [u32; 24],

    /// Count of bloops per day, keyed by date in local timezone.
    pub bloops_per_day: HashMap<NaiveDate, u32>,
}

impl Default for ClientStats {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientStats {
    /// Creates a new empty statistics struct.
    pub fn new() -> Self {
        Self {
            total_bloops: 0,
            per_minute_bloops: [0; MINUTES_IN_DAY],
            per_minute_dates: [NaiveDate::MIN; MINUTES_IN_DAY],
            bloops_per_hour: [0; 24],
            bloops_per_day: HashMap::new(),
        }
    }

    fn record_bloop(&mut self, bloop: ProcessedBloop, tz: &Tz) {
        let utc_date = bloop.recorded_at.date_naive();
        let time = bloop.recorded_at.time();
        let idx = (time.hour() * 60 + time.minute()) as usize;

        if self.per_minute_dates[idx] != utc_date {
            self.per_minute_dates[idx] = utc_date;
            self.per_minute_bloops[idx] = 1;
        } else {
            self.per_minute_bloops[idx] = self.per_minute_bloops[idx].saturating_add(1);
        }

        let local_dt = bloop.recorded_at.with_timezone(tz);
        let local_hour = local_dt.hour() as usize;
        let local_date = local_dt.date_naive();

        self.total_bloops += 1;
        self.bloops_per_hour[local_hour] += 1;
        *self.bloops_per_day.entry(local_date).or_insert(0) += 1;
    }

    fn count_last_minutes(&self, now: DateTime<Utc>, minutes: usize) -> u32 {
        let now_minute = (now.hour() * 60 + now.minute()) as usize;
        let mut total = 0;

        for i in 0..minutes {
            let idx = (now_minute + MINUTES_IN_DAY - i) % MINUTES_IN_DAY;
            let expected_date = (now - chrono::Duration::minutes(i as i64)).date_naive();

            if self.per_minute_dates[idx] == expected_date {
                total += self.per_minute_bloops[idx] as u32;
            }
        }

        total
    }
}

/// Calculates the minute index within a day (0..1439) for a given `NaiveTime`.
///
/// This is used to map events into per-minute buckets.
///
/// # Examples
///
/// ```
/// use chrono::NaiveTime;
/// use bloop_server_framework::statistics::minute_index;
///
/// let time = NaiveTime::from_hms_opt(13, 45, 0).unwrap();
/// let idx = minute_index(time);
///
/// assert_eq!(idx, 13 * 60 + 45);
/// ```
#[inline]
pub fn minute_index(time: impl Timelike) -> usize {
    time.hour() as usize * 60 + time.minute() as usize
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatsSummary {
    pub total_bloops: u64,
    pub bloops_last_hour: u32,
    pub bloops_last_24_hours: u32,
    pub bloops_per_hour: [u32; 24],
    pub bloops_per_day: HashMap<NaiveDate, u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatsSnapshot {
    pub created_at: DateTime<Utc>,
    pub clients: HashMap<String, StatsSummary>,
    pub global: StatsSummary,
}

#[derive(Debug, Default)]
pub struct StatsTracker {
    clients: HashMap<String, ClientStats>,
    cached_snapshot: RwLock<Option<StatsSnapshot>>,
}

impl From<HashMap<String, ClientStats>> for StatsTracker {
    fn from(clients: HashMap<String, ClientStats>) -> Self {
        Self::new(clients)
    }
}

impl StatsTracker {
    pub fn new(clients: HashMap<String, ClientStats>) -> Self {
        Self {
            clients,
            cached_snapshot: RwLock::new(None),
        }
    }

    fn record_bloop(&mut self, bloop: ProcessedBloop, tz: &Tz) {
        let client = self.clients.entry(bloop.client_id.clone()).or_default();

        client.record_bloop(bloop, tz);
    }

    async fn snapshot(&self, now: DateTime<Utc>) -> StatsSnapshot {
        if let Some(snapshot) = self.cached_snapshot.read().await.as_ref()
            && snapshot.created_at > now - chrono::Duration::minutes(1)
        {
            return snapshot.clone();
        }

        let snapshot = self.compute_snapshot(now);
        self.cached_snapshot.write().await.replace(snapshot.clone());

        snapshot
    }

    fn compute_snapshot(&self, now: DateTime<Utc>) -> StatsSnapshot {
        let mut clients_snapshots = HashMap::new();
        let mut global = StatsSummary {
            total_bloops: 0,
            bloops_last_hour: 0,
            bloops_last_24_hours: 0,
            bloops_per_hour: [0; 24],
            bloops_per_day: HashMap::new(),
        };

        for (client_id, client) in &self.clients {
            let bloops_last_hour = client.count_last_minutes(now, 60);
            let bloops_last_24_hours = client.count_last_minutes(now, MINUTES_IN_DAY);

            let snapshot = StatsSummary {
                total_bloops: client.total_bloops,
                bloops_last_hour,
                bloops_last_24_hours,
                bloops_per_hour: client.bloops_per_hour,
                bloops_per_day: client.bloops_per_day.clone(),
            };

            global.total_bloops += snapshot.total_bloops;
            global.bloops_last_hour += snapshot.bloops_last_hour;
            global.bloops_last_24_hours += snapshot.bloops_last_24_hours;

            for hour in 0..24 {
                global.bloops_per_hour[hour] += snapshot.bloops_per_hour[hour];
            }

            for (day, count) in snapshot.bloops_per_day.iter() {
                *global.bloops_per_day.entry(*day).or_insert(0) += count;
            }

            clients_snapshots.insert(client_id.clone(), snapshot);
        }

        StatsSnapshot {
            created_at: now,
            clients: clients_snapshots,
            global,
        }
    }
}

/// A background service that collects statistics and exposes them over HTTP.
#[derive(Debug)]
pub struct StatisticsServer {
    addr: SocketAddr,
    stats: Arc<RwLock<StatsTracker>>,
    event_rx: broadcast::Receiver<Event>,
    tz: Tz,
    #[cfg(test)]
    pub test_notify_event_processed: Arc<tokio::sync::Notify>,
}

impl StatisticsServer {
    /// Starts the statistics server and begins listening for events and HTTP requests.
    ///
    /// This runs the main event loop until the broadcast channel is closed.
    pub async fn listen(&mut self) -> Result<(), io::Error> {
        let listener = TcpListener::bind(self.addr).await?;

        loop {
            let should_continue = select! {
                conn = listener.accept() => {
                    if let Ok((stream, _)) = conn {
                        self.handle_connection(stream).await;
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

        Ok(())
    }

    /// Handles a single incoming HTTP connection and serves current stats as JSON.
    async fn handle_connection(&self, stream: TcpStream) {
        let io = TokioIo::new(stream);
        let stats = self.stats.clone();

        task::spawn(async move {
            let result = http1::Builder::new()
                .timer(TokioTimer::new())
                .serve_connection(
                    io,
                    service_fn(move |_: Request<_>| {
                        let stats = stats.clone();

                        async move {
                            let snapshot = stats.read().await.snapshot(Utc::now()).await;

                            let body = match serde_json::to_string(&snapshot) {
                                Ok(body) => body,
                                Err(err) => {
                                    error!("failed to serialize statistics: {:?}", err);
                                    return Ok::<_, Infallible>(
                                        Response::builder()
                                            .status(500)
                                            .body(Full::default())
                                            .unwrap(),
                                    );
                                }
                            };

                            Ok::<_, Infallible>(
                                Response::builder()
                                    .header("Content-Type", "application/json")
                                    .body(Full::new(Bytes::from(body)))
                                    .unwrap(),
                            )
                        }
                    }),
                )
                .await;

            if let Err(err) = result {
                error!("failed to serve statistics request: {:?}", err);
            }
        });
    }

    /// Handles a received event from the broadcast channel.
    ///
    /// If the event is a `BloopProcessed`, the statistics are updated. Returns
    /// `false` when the channel is closed, indicating the server should shut down.
    async fn handle_recv(&self, result: Result<Event, RecvError>) -> bool {
        let should_continue = match result {
            Ok(Event::BloopProcessed(bloop)) => {
                self.stats.write().await.record_bloop(bloop, &self.tz);
                true
            }
            Ok(_) => true,
            Err(RecvError::Lagged(n)) => {
                warn!("StatisticsServer lagged by {n} messages, some bloops were missed");
                true
            }
            Err(RecvError::Closed) => {
                debug!("StatisticsServer event stream closed, exiting event loop");
                false
            }
        };

        #[cfg(test)]
        self.test_notify_event_processed.notify_one();

        should_continue
    }
}

#[cfg(feature = "tokio-graceful-shutdown")]
#[derive(Debug, Error)]
pub enum NeverError {}

#[cfg(feature = "tokio-graceful-shutdown")]
impl IntoSubsystem<NeverError> for StatisticsServer {
    async fn run(mut self, subsys: &mut SubsystemHandle) -> Result<(), NeverError> {
        let _ = self.listen().cancel_on_shutdown(subsys).await;

        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum BuilderError {
    #[error("missing field: {0}")]
    MissingField(&'static str),

    #[error(transparent)]
    AddrParse(#[from] std::net::AddrParseError),
}

/// Builder for creating a [StatisticsServer] instance.
#[derive(Debug, Default)]
pub struct StatisticsServerBuilder {
    address: Option<String>,
    tz: Option<Tz>,
    stats: Option<HashMap<String, ClientStats>>,
    event_rx: Option<broadcast::Receiver<Event>>,
}

impl StatisticsServerBuilder {
    /// Creates a new empty builder.
    pub fn new() -> Self {
        Self {
            address: None,
            tz: None,
            stats: None,
            event_rx: None,
        }
    }

    /// Sets the listening address for the statistics server.
    pub fn address(mut self, address: impl Into<String>) -> Self {
        self.address = Some(address.into());
        self
    }

    /// Sets the timezone to time specific statistics.
    pub fn tz(mut self, tz: Tz) -> Self {
        self.tz = Some(tz);
        self
    }

    /// Sets the initial statistics.
    ///
    /// This is typically loaded from persistent storage (e.g., database) at startup.
    pub fn stats(mut self, stats: HashMap<String, ClientStats>) -> Self {
        self.stats = Some(stats);
        self
    }

    /// Sets the event receiver the server listens to for tracking events.
    pub fn event_rx(mut self, event_rx: broadcast::Receiver<Event>) -> Self {
        self.event_rx = Some(event_rx);
        self
    }

    /// Consumes the builder and produces a configured [StatisticsServer].
    pub fn build(self) -> Result<StatisticsServer, BuilderError> {
        let addr: SocketAddr = self
            .address
            .ok_or(BuilderError::MissingField("address"))?
            .parse()?;

        Ok(StatisticsServer {
            addr,
            tz: self.tz.unwrap_or(Tz::UTC),
            stats: Arc::new(RwLock::new(StatsTracker::new(
                self.stats.unwrap_or_default(),
            ))),
            event_rx: self
                .event_rx
                .ok_or(BuilderError::MissingField("event_rx"))?,
            #[cfg(test)]
            test_notify_event_processed: Arc::new(tokio::sync::Notify::new()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveTime, TimeZone};
    use chrono_tz::UTC;
    use ntest::timeout;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use uuid::Uuid;

    fn make_bloop(client_id: &str, recorded_at: DateTime<Utc>) -> ProcessedBloop {
        ProcessedBloop {
            client_id: client_id.to_string(),
            player_id: Uuid::new_v4(),
            recorded_at,
        }
    }

    #[test]
    fn minute_index_calculation() {
        let time = NaiveTime::from_hms_opt(13, 45, 0).unwrap();
        assert_eq!(minute_index(time), 13 * 60 + 45);

        let time = NaiveTime::from_hms_opt(0, 0, 0).unwrap();
        assert_eq!(minute_index(time), 0);
    }

    #[test]
    fn record_bloop_basic() {
        let tz = UTC;
        let mut stats = ClientStats::new();
        let now = Utc.with_ymd_and_hms(2025, 7, 5, 12, 30, 0).unwrap();

        let bloop = make_bloop("client1", now);
        stats.record_bloop(bloop, &tz);

        assert_eq!(stats.total_bloops, 1);

        let idx = minute_index(now.time());
        assert_eq!(stats.per_minute_bloops[idx], 1);
        assert_eq!(stats.per_minute_dates[idx], now.date_naive());

        let hour = now.hour() as usize;
        assert_eq!(stats.bloops_per_hour[hour], 1);
        assert_eq!(stats.bloops_per_day[&now.date_naive()], 1);
    }

    #[test]
    fn record_bloop_accumulation_and_wraparound() {
        let tz = UTC;
        let mut stats = ClientStats::new();
        let now = Utc.with_ymd_and_hms(2025, 7, 5, 0, 1, 0).unwrap();

        let bloop1 = make_bloop("client1", now);
        stats.record_bloop(bloop1, &tz);
        stats.record_bloop(make_bloop("client1", now), &tz);

        let idx = minute_index(now.time());
        assert_eq!(stats.per_minute_bloops[idx], 2);
        assert_eq!(stats.per_minute_dates[idx], now.date_naive());

        let yesterday = now - Duration::days(1);
        let idx_wrap = minute_index(yesterday.time());
        stats.record_bloop(make_bloop("client1", yesterday), &tz);

        assert_eq!(stats.per_minute_dates[idx_wrap], yesterday.date_naive());
        assert_eq!(stats.per_minute_bloops[idx_wrap], 1);
    }

    #[test]
    fn count_last_minutes() {
        let tz = UTC;
        let mut stats = ClientStats::new();
        let now = Utc.with_ymd_and_hms(2025, 7, 5, 10, 0, 0).unwrap();

        stats.record_bloop(make_bloop("c", now), &tz);
        stats.record_bloop(make_bloop("c", now - Duration::minutes(1)), &tz);
        stats.record_bloop(make_bloop("c", now - Duration::minutes(2)), &tz);

        let count = stats.count_last_minutes(now, 3);
        assert_eq!(count, 3);

        let count_short = stats.count_last_minutes(now, 1);
        assert_eq!(count_short, 1);
    }

    #[test]
    fn stats_tracker_snapshot() {
        let tz = UTC;
        let mut tracker = StatsTracker::default();

        let now = Utc.with_ymd_and_hms(2025, 7, 5, 15, 0, 0).unwrap();
        let bloop = make_bloop("client-a", now);
        tracker.record_bloop(bloop, &tz);

        let snapshot = tracker.compute_snapshot(now);

        assert!(snapshot.clients.contains_key("client-a"));
        let client_stats = &snapshot.clients["client-a"];

        assert_eq!(client_stats.total_bloops, 1);
        assert!(client_stats.bloops_last_hour >= 1);
        assert!(client_stats.bloops_last_24_hours >= 1);
        assert!(client_stats.bloops_per_hour.iter().any(|&x| x >= 1));
        assert!(client_stats.bloops_per_day.contains_key(&now.date_naive()));

        assert_eq!(snapshot.global.total_bloops, 1);
    }

    #[test]
    fn count_last_minutes_across_midnight() {
        // now_minute = 1 (00:01 UTC), so counting back 3 minutes wraps across midnight
        let tz = UTC;
        let mut stats = ClientStats::new();
        let now = Utc.with_ymd_and_hms(2025, 7, 6, 0, 1, 0).unwrap();

        // 00:01 today
        stats.record_bloop(make_bloop("c", now), &tz);
        // 00:00 today
        stats.record_bloop(make_bloop("c", now - Duration::minutes(1)), &tz);
        // 23:59 yesterday
        stats.record_bloop(make_bloop("c", now - Duration::minutes(2)), &tz);

        let count = stats.count_last_minutes(now, 3);
        assert_eq!(count, 3, "should count all 3 minutes even when wrapping past midnight");
    }

    #[test]
    fn record_bloop_uses_local_timezone_for_bloops_per_day() {
        // Use a timezone that is ahead of UTC (e.g. UTC+2) so that a bloop
        // recorded at 23:30 UTC on day D is actually local day D+1.
        let tz = chrono_tz::Europe::Helsinki; // UTC+2 / UTC+3
        let mut stats = ClientStats::new();

        // 2025-07-05 23:30 UTC == 2025-07-06 01:30 Helsinki (EEST, UTC+3)
        let recorded_at = Utc.with_ymd_and_hms(2025, 7, 5, 23, 30, 0).unwrap();
        let bloop = make_bloop("client1", recorded_at);
        stats.record_bloop(bloop, &tz);

        let local_date = recorded_at.with_timezone(&tz).date_naive();
        let utc_date = recorded_at.date_naive();

        // The local date must differ from the UTC date for this test to be meaningful
        assert_ne!(local_date, utc_date);
        // bloops_per_day must be keyed by local date, not UTC date
        assert_eq!(stats.bloops_per_day.get(&local_date), Some(&1));
        assert_eq!(stats.bloops_per_day.get(&utc_date), None);
    }


    fn dummy_stats() -> HashMap<String, ClientStats> {
        let mut map = HashMap::new();
        map.insert("client1".to_string(), Default::default());
        map
    }

    fn dummy_event_rx() -> broadcast::Receiver<Event> {
        // Create a broadcast channel and take a receiver for testing
        let (_tx, rx) = broadcast::channel(16);
        rx
    }

    #[test]
    fn build_succeeds_with_all_fields() {
        let builder = StatisticsServerBuilder::new()
            .address("127.0.0.1:12345")
            .tz(chrono_tz::Europe::London)
            .stats(dummy_stats())
            .event_rx(dummy_event_rx());

        let server = builder.build().unwrap();
        assert_eq!(server.addr, "127.0.0.1:12345".parse().unwrap());
        assert_eq!(server.tz, chrono_tz::Europe::London);
    }

    #[test]
    fn build_fails_if_addr_missing() {
        let builder = StatisticsServerBuilder::new()
            .stats(dummy_stats())
            .event_rx(dummy_event_rx());

        let err = builder.build().unwrap_err();
        assert!(matches!(err, BuilderError::MissingField(field) if field == "address"));
    }
    #[test]
    fn build_fails_if_event_rx_missing() {
        let builder = StatisticsServerBuilder::new()
            .address("127.0.0.1:12345")
            .stats(dummy_stats());

        let err = builder.build().unwrap_err();
        assert!(matches!(err, BuilderError::MissingField(field) if field == "event_rx"));
    }

    #[test]
    fn build_defaults_tz_to_utc() {
        let builder = StatisticsServerBuilder::new()
            .address("127.0.0.1:12345")
            .stats(dummy_stats())
            .event_rx(dummy_event_rx());

        let server = builder.build().unwrap();
        assert_eq!(server.tz, UTC);
    }

    #[tokio::test]
    #[timeout(1000)]
    async fn server_handles_bloop_processed_event() {
        let tz = Tz::UTC;
        let stats_map = HashMap::<String, ClientStats>::new();

        let (sender, event_rx) = broadcast::channel(16);

        let mut server = StatisticsServerBuilder::new()
            .address("127.0.0.1:12345")
            .tz(tz)
            .stats(stats_map)
            .event_rx(event_rx)
            .build()
            .unwrap();
        let notify = server.test_notify_event_processed.clone();
        let stats = server.stats.clone();

        tokio::spawn(async move {
            let _ = server.listen().await;
        });

        let bloop = Event::BloopProcessed(make_bloop("client", Utc::now()));
        sender.send(bloop).unwrap();
        notify.notified().await;

        let stats = stats.read().await;
        let snapshot = stats.snapshot(Utc::now()).await;

        assert_eq!(snapshot.clients["client"].total_bloops, 1);
    }

    #[tokio::test]
    #[timeout(1000)]
    async fn server_serves_stats_over_http() {
        let (sender, event_rx) = broadcast::channel(16);

        let local_addr = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();

        let mut server = StatisticsServerBuilder::new()
            .address(local_addr.to_string())
            .event_rx(event_rx)
            .build()
            .unwrap();
        let notify = server.test_notify_event_processed.clone();

        tokio::spawn(async move {
            let _ = server.listen().await;
        });

        let bloop = Event::BloopProcessed(ProcessedBloop {
            player_id: Uuid::new_v4(),
            client_id: "client".to_string(),
            recorded_at: Utc::now(),
        });
        sender.send(bloop).unwrap();
        notify.notified().await;

        let mut client = TcpStream::connect(local_addr).await.unwrap();
        let request = b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n";
        client.write_all(request).await.unwrap();

        let mut buffer = vec![0; 1024];
        let bytes_read = client.read(&mut buffer).await.unwrap();

        let response = String::from_utf8_lossy(&buffer[..bytes_read]);
        assert!(response.contains("200 OK"));
        assert!(response.contains("application/json"));
    }
}
