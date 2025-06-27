use crate::event::Event;
#[cfg(test)]
use crate::test_utils::Utc;
use async_trait::async_trait;
use chrono::DateTime;
#[cfg(not(test))]
use chrono::Utc;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::{select, time};
use tokio_graceful_shutdown::{FutureExt, IntoSubsystem, SubsystemHandle};
use tracing::{debug, warn};

#[cfg(feature = "health-monitor-telegram")]
pub mod telegram;

/// The kind of client event.
#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub enum ClientEventKind {
    Connect,
    Disconnect,
    ConnectionLoss,
}

/// Represents a client event with type, connection ID, and timestamp.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(Clone))]
pub struct ClientEvent {
    /// The type of event.
    pub kind: ClientEventKind,
    /// The identifier of the client connection.
    pub conn_id: usize,
    /// When the event occurred.
    pub timestamp: DateTime<chrono::Utc>,
}

/// The health status of a client.
#[derive(Debug, Serialize, Ord, PartialOrd, Eq, PartialEq, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub enum Health {
    Healthy,
    Unstable,
    Offline,
}

/// Current status information for a client.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(test, derive(Clone))]
pub struct ClientStatus {
    /// The current health status of the client.
    pub health: Health,
    /// The timestamp since this status is valid.
    pub since: DateTime<chrono::Utc>,
    /// The local IP address of the client, if known.
    pub local_ip: Option<IpAddr>,
    /// A queue of recent events for this client.
    pub events: VecDeque<ClientEvent>,
}

impl Default for ClientStatus {
    fn default() -> Self {
        Self {
            health: Health::Offline,
            since: Utc::now(),
            local_ip: None,
            events: VecDeque::new(),
        }
    }
}

/// A report containing the status of multiple clients.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(Clone))]
pub struct HealthReport {
    pub clients: HashMap<String, ClientStatus>,
}

impl HealthReport {
    fn new(client_ids: HashSet<String>) -> Self {
        Self {
            clients: client_ids
                .into_iter()
                .map(|client_id| (client_id, ClientStatus::default()))
                .collect(),
        }
    }

    /// Formats the health report as a human-readable text summary.
    ///
    /// Clients are sorted by health status, then by client ID. Displays an
    /// icon, client ID, IP address, health status, and duration.
    pub fn as_text_report(&self) -> String {
        let mut clients = self
            .clients
            .iter()
            .collect::<Vec<(&String, &ClientStatus)>>();

        clients.sort_by(|&a, &b| {
            if a.1.health == b.1.health {
                return a.0.cmp(b.0);
            }

            a.1.health.cmp(&b.1.health)
        });

        let now = Utc::now();

        clients
            .iter()
            .map(|(client_id, status)| {
                let mut text = String::new();
                let local_ip = match status.local_ip {
                    Some(local_ip) => local_ip.to_string(),
                    None => "unknown".to_string(),
                };
                let (emoji, label) = match status.health {
                    Health::Healthy => ('🟢', "Healthy"),
                    Health::Unstable => ('🔴', "Unhealthy"),
                    Health::Offline => ('⚫', "Offline"),
                };

                let status_duration = (now - status.since).num_minutes();
                text.push_str(&format!(
                    "{emoji} {client_id} ({local_ip}): {label} for {status_duration} minutes"
                ));

                text
            })
            .collect::<Vec<String>>()
            .join("\n")
    }
}

/// A trait for sending health reports asynchronously.
#[async_trait]
pub trait HealthReportSender {
    type Error: std::fmt::Debug + std::fmt::Display + Send + Sync + 'static;

    /// Sends a health report asynchronously.
    ///
    /// The `report` argument is the health report to send. If `silent` is true,
    /// notifications should be suppressed if possible.
    ///
    /// Returns `Ok(())` on success, or an error of type `Self::Error` on failure.
    async fn send(&mut self, report: &HealthReport, silent: bool) -> Result<(), Self::Error>;
}

#[derive(Debug, Error)]
pub enum BuilderError {
    #[error("missing field: {0}")]
    MissingField(&'static str),

    #[error("invalid value: {0}")]
    InvalidValue(&'static str),
}

/// Builder for creating a [`HealthMonitor`] instance.
///
/// Allows configuring optional parameters with sensible defaults. Required
/// parameters must be set before calling [`build()`].
///
/// # Examples
///
/// ```
/// use std::convert::Infallible;
/// use async_trait::async_trait;
/// use tokio::sync::broadcast;
/// use bloop_server_framework::health_monitor::{
///     HealthMonitorBuilder,
///     HealthReport,
///     HealthReportSender
/// };
///
/// struct DummySender;
///
/// #[async_trait]
/// impl HealthReportSender for DummySender {
///     type Error = Infallible;
///
///     async fn send(
///         &mut self,
///         report: &HealthReport,
///         silent: bool
///     ) -> Result<(), Self::Error> {
///          Ok(())
///     }
/// }
///
/// let (_, event_rx) = broadcast::channel(512);
///
/// let monitor = HealthMonitorBuilder::new()
///     .sender(DummySender)
///     .event_rx(event_rx)
///     .max_client_events(100)
///     .build()
///     .expect("failed to build HealthMonitor");
/// ```
#[derive(Debug, Default)]
pub struct HealthMonitorBuilder<T: HealthReportSender> {
    sender: Option<T>,
    event_rx: Option<broadcast::Receiver<Event>>,
    client_ids: HashSet<String>,
    max_client_events: usize,
    connection_loss_limit: usize,
    check_interval: Duration,
    reminder_interval: Duration,
    offline_grace_period: Duration,
    connection_loss_window: Duration,
    recovery_grace_period: Duration,
}

impl<T: HealthReportSender> HealthMonitorBuilder<T> {
    /// Creates a new [`HealthMonitorBuilder`] with default configuration values.
    ///
    /// You must provide at least a sender and an event receiver before calling
    /// [`build()`].
    pub fn new() -> Self {
        Self {
            sender: None,
            event_rx: None,
            client_ids: HashSet::new(),
            max_client_events: 50,
            connection_loss_limit: 3,
            check_interval: Duration::from_secs(30),
            reminder_interval: Duration::from_secs(1800),
            offline_grace_period: Duration::from_secs(120),
            connection_loss_window: Duration::from_secs(600),
            recovery_grace_period: Duration::from_secs(1200),
        }
    }

    /// Sets the report sender implementation.
    ///
    /// This is a required field. The sender is used to transmit health reports
    /// during monitoring.
    pub fn sender(mut self, sender: T) -> Self {
        self.sender = Some(sender);
        self
    }

    /// Sets the event receiver used to receive client health-related events.
    ///
    /// This is a required field. The monitor listens to this channel for incoming
    /// events.
    pub fn event_rx(mut self, event_rx: broadcast::Receiver<Event>) -> Self {
        self.event_rx = Some(event_rx);
        self
    }

    /// Sets the initial list of known client IDs to track in the health report.
    ///
    /// Defaults to an empty set if not provided.
    pub fn client_ids(mut self, client_ids: HashSet<String>) -> Self {
        self.client_ids = client_ids;
        self
    }

    /// Sets the maximum number of recent events to store per client.
    ///
    /// Defaults to `50`. Events beyond this limit are discarded (oldest first).
    pub fn max_client_events(mut self, max_client_events: usize) -> Self {
        self.max_client_events = max_client_events;
        self
    }

    /// Sets the number of recent `ConnectionLoss` events required to consider a
    /// client unstable.
    ///
    /// Defaults to `3`.
    pub fn connection_loss_limit(mut self, connection_loss_limit: usize) -> Self {
        self.connection_loss_limit = connection_loss_limit;
        self
    }

    /// Sets how frequently the system checks for health status updates.
    ///
    /// Defaults to `30 seconds`.
    pub fn check_interval(mut self, check_interval: Duration) -> Self {
        self.check_interval = check_interval;
        self
    }

    /// Sets the interval for sending health reminders if unhealthy clients are
    /// still present.
    ///
    /// Defaults to `30 minutes`.
    pub fn reminder_interval(mut self, reminder_interval: Duration) -> Self {
        self.reminder_interval = reminder_interval;
        self
    }

    /// Sets the grace period after the last connection loss before a client is
    /// considered offline.
    ///
    /// Defaults to `2 minutes`.
    pub fn offline_grace_period(mut self, offline_grace_period: Duration) -> Self {
        self.offline_grace_period = offline_grace_period;
        self
    }

    /// Sets the window of time used to count recent connection loss events.
    ///
    /// Defaults to `10 minutes`.
    pub fn connection_loss_window(mut self, connection_loss_window: Duration) -> Self {
        self.connection_loss_window = connection_loss_window;
        self
    }

    /// Sets the duration a client must remain stable before being promoted to
    /// healthy.
    ///
    /// Defaults to `20 minutes`.
    pub fn recovery_grace_period(mut self, recovery_grace_period: Duration) -> Self {
        self.recovery_grace_period = recovery_grace_period;
        self
    }

    /// Builds a [`HealthMonitor`] instance using the configured values.
    ///
    /// Returns an error if required fields like the sender or event receiver are
    /// missing.
    pub fn build(self) -> Result<HealthMonitor<T>, BuilderError> {
        let sender = self.sender.ok_or(BuilderError::MissingField("sender"))?;
        let event_rx = self.event_rx.ok_or(BuilderError::MissingField("event_r"))?;

        if self.max_client_events < 2 {
            return Err(BuilderError::InvalidValue(
                "max_client_events must be at least 2",
            ));
        }

        if self.connection_loss_limit < 1 {
            return Err(BuilderError::InvalidValue(
                "connection_loss_limit must be at least 1",
            ));
        }

        Ok(HealthMonitor {
            sender,
            event_rx,
            report: HealthReport::new(self.client_ids),
            max_client_events: self.max_client_events,
            connection_loss_limit: self.connection_loss_limit,
            check_interval: self.check_interval,
            reminder_interval: self.reminder_interval,
            offline_grace_period: self.offline_grace_period,
            connection_loss_window: self.connection_loss_window,
            recovery_grace_period: self.recovery_grace_period,
            #[cfg(test)]
            test_notify_event_processed: std::sync::Arc::new(tokio::sync::Notify::new()),
        })
    }
}

/// A runtime health monitoring service.
///
/// The `HealthMonitor` tracks client health statuses by processing events such
/// as connections, disconnections, and connection losses. It periodically
/// checks client states and sends health reports using the configured
/// [`HealthReportSender`].
pub struct HealthMonitor<T: HealthReportSender> {
    sender: T,
    event_rx: broadcast::Receiver<Event>,
    report: HealthReport,
    max_client_events: usize,
    connection_loss_limit: usize,
    check_interval: Duration,
    reminder_interval: Duration,
    offline_grace_period: Duration,
    connection_loss_window: Duration,
    recovery_grace_period: Duration,
    #[cfg(test)]
    pub test_notify_event_processed: std::sync::Arc<tokio::sync::Notify>,
}

impl<T: HealthReportSender> HealthMonitor<T> {
    /// Starts the health monitoring event loop.
    ///
    /// This async method continuously listens for incoming events, periodic checks,
    /// and reminders. It processes client health updates based on received events
    /// and predefined timing intervals.
    ///
    /// The loop will run until the event stream is closed or an exit condition is
    /// met.
    ///
    /// # Behavior
    ///
    /// - Sends the initial health report immediately upon start.
    /// - Periodically checks client health status every `check_interval`.
    /// - Sends reminder reports every `reminder_interval` if any clients are not
    ///   healthy.
    /// - Processes events received from the event receiver to update client states.
    pub async fn listen(&mut self) {
        let _ = self.sender.send(&self.report, false).await;
        let mut check_interval = time::interval(self.check_interval);
        let mut reminder_interval = time::interval(self.reminder_interval);

        check_interval.tick().await;
        reminder_interval.tick().await;

        loop {
            let should_continue = select! {
                _ = check_interval.tick() => {
                    self.check_clients().await;
                    true
                }
                _ = reminder_interval.tick() => {
                    self.send_reminder().await;
                    true
                }
                result = self.event_rx.recv() => {
                    self.handle_event_recv(result).await
                }
            };

            if !should_continue {
                break;
            }
        }
    }

    async fn check_clients(&mut self) {
        let now = Utc::now();
        let offline_cutoff = now - self.offline_grace_period;
        let connection_loss_cutoff = now - self.connection_loss_window;
        let recovery_cutoff = now - self.recovery_grace_period;

        let mut silent_updates: usize = 0;
        let mut alert_updates: usize = 0;

        for client in self.report.clients.values_mut() {
            if matches!(client.health, Health::Offline) {
                continue;
            }

            let mut recent_events: Vec<_> = client.events.iter().take(2).collect();
            recent_events.sort_by(|a, b| b.conn_id.cmp(&a.conn_id));

            let Some(latest_event) = recent_events.first() else {
                continue;
            };

            match (client.health, latest_event.kind) {
                (Health::Healthy | Health::Unstable, ClientEventKind::ConnectionLoss)
                    if latest_event.timestamp <= offline_cutoff =>
                {
                    client.health = Health::Offline;
                    client.since = now;
                    alert_updates += 1;
                }
                (Health::Unstable, ClientEventKind::Connect)
                    if latest_event.timestamp <= recovery_cutoff =>
                {
                    client.health = Health::Healthy;
                    client.since = now;
                    silent_updates += 1;
                }
                (Health::Healthy, _) => {
                    let mut connection_loss_count: usize = 0;

                    for event in client.events.iter() {
                        if event.timestamp < connection_loss_cutoff {
                            break;
                        }

                        if matches!(event.kind, ClientEventKind::ConnectionLoss) {
                            connection_loss_count += 1;
                        }
                    }

                    if connection_loss_count >= self.connection_loss_limit {
                        client.health = Health::Unstable;
                        client.since = now;
                        alert_updates += 1;
                    }
                }
                (_, _) => {}
            }
        }

        if alert_updates > 0 {
            self.send_report(false).await;
        } else if silent_updates > 0 {
            self.send_report(true).await;
        }
    }

    async fn send_reminder(&mut self) {
        if self
            .report
            .clients
            .values()
            .any(|client| !matches!(client.health, Health::Healthy))
        {
            self.send_report(false).await;
        }
    }

    async fn handle_event_recv(&mut self, result: Result<Event, RecvError>) -> bool {
        match result {
            Ok(Event::ClientConnect {
                client_id,
                local_ip,
                conn_id,
            }) => {
                let client = self.report.clients.entry(client_id).or_default();
                client.local_ip = Some(local_ip);
                let mut should_send_report = false;

                if matches!(client.health, Health::Offline) {
                    client.health = Health::Healthy;
                    client.since = Utc::now();
                    should_send_report = true;
                }

                client.events.push_front(ClientEvent {
                    kind: ClientEventKind::Connect,
                    conn_id,
                    timestamp: Utc::now(),
                });
                println!("connect");
                if client.events.len() > self.max_client_events {
                    client.events.pop_back();
                }

                if should_send_report {
                    self.send_report(true).await;
                }

                #[cfg(test)]
                self.test_notify_event_processed.notify_one();

                true
            }
            Ok(Event::ClientDisconnect { client_id, conn_id }) => {
                let client = self.report.clients.entry(client_id).or_default();
                client.health = Health::Offline;
                client.since = Utc::now();
                client.events.push_front(ClientEvent {
                    kind: ClientEventKind::Disconnect,
                    conn_id,
                    timestamp: Utc::now(),
                });

                if client.events.len() > self.max_client_events {
                    client.events.pop_back();
                }

                #[cfg(test)]
                self.test_notify_event_processed.notify_one();

                self.send_report(true).await;
                true
            }
            Ok(Event::ClientConnectionLoss { client_id, conn_id }) => {
                let client = self.report.clients.entry(client_id).or_default();
                client.events.push_front(ClientEvent {
                    kind: ClientEventKind::ConnectionLoss,
                    conn_id,
                    timestamp: Utc::now(),
                });

                if client.events.len() > self.max_client_events {
                    client.events.pop_back();
                }

                #[cfg(test)]
                self.test_notify_event_processed.notify_one();

                true
            }
            Ok(_) => true,
            Err(RecvError::Lagged(n)) => {
                warn!("HealthMonitor lagged by {n} messages, some events were missed");
                true
            }
            Err(RecvError::Closed) => {
                debug!("HealthMonitor event stream closed, exiting event loop");
                false
            }
        }
    }

    async fn send_report(&mut self, silent: bool) {
        if let Err(err) = self.sender.send(&self.report, silent).await {
            warn!("Failed to send health report: {:?}", err);
        }
    }
}

#[cfg(feature = "tokio-graceful-shutdown")]
#[derive(Debug, Error)]
pub enum NeverError {}

#[async_trait]
impl<T: HealthReportSender + Send + Sync + 'static> IntoSubsystem<NeverError> for HealthMonitor<T> {
    async fn run(mut self, subsys: SubsystemHandle) -> Result<(), NeverError> {
        let _ = self.listen().cancel_on_shutdown(&subsys).await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::event::Event;
    use crate::health_monitor::{Health, HealthMonitorBuilder, HealthReport, HealthReportSender};
    use async_trait::async_trait;
    use ntest::timeout;
    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::broadcast;
    use tokio::time::advance;

    #[derive(Default, Clone)]
    struct MockSender {
        last: Arc<Mutex<Option<(HealthReport, bool)>>>,
    }

    #[async_trait]
    impl HealthReportSender for MockSender {
        type Error = Infallible;

        async fn send(&mut self, report: &HealthReport, silent: bool) -> Result<(), Self::Error> {
            let mut last = self.last.lock().unwrap();
            *last = Some((report.clone(), silent));
            Ok(())
        }
    }

    async fn wait_for_report(sender: &MockSender) -> (HealthReport, bool) {
        loop {
            if let Some(report) = sender.last.lock().unwrap().take() {
                return report;
            }

            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    #[timeout(1000)]
    async fn sends_initial_report_on_start() {
        let (_tx, rx) = broadcast::channel(16);
        let sender = MockSender::default();

        let mut monitor = HealthMonitorBuilder::new()
            .sender(sender.clone())
            .event_rx(rx)
            .client_ids(Default::default())
            .build()
            .unwrap();

        let handle = tokio::spawn(async move {
            monitor.listen().await;
        });

        let (_, silent) = wait_for_report(&sender).await;
        assert!(!silent, "Initial report should not be silent");

        drop(handle);
    }

    #[tokio::test(start_paused = true)]
    #[timeout(1000)]
    async fn client_connect_sets_healthy() {
        let (tx, rx) = broadcast::channel(16);
        let sender = MockSender::default();

        let mut monitor = HealthMonitorBuilder::new()
            .sender(sender.clone())
            .event_rx(rx)
            .client_ids(["client1".to_string()].into_iter().collect())
            .build()
            .unwrap();
        let notify = monitor.test_notify_event_processed.clone();

        let handle = tokio::spawn(async move {
            monitor.listen().await;
        });
        wait_for_report(&sender).await;

        tx.send(Event::ClientConnect {
            client_id: "client1".to_string(),
            local_ip: "127.0.0.1".parse().unwrap(),
            conn_id: 1,
        })
        .unwrap();
        notify.notified().await;

        let (report, _) = wait_for_report(&sender).await;
        assert_eq!(report.clients["client1"].health, Health::Healthy);

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    #[timeout(1000)]
    async fn client_disconnect_sets_offline_immediately() {
        let (tx, rx) = broadcast::channel(16);
        let sender = MockSender::default();

        let mut monitor = HealthMonitorBuilder::new()
            .sender(sender.clone())
            .event_rx(rx)
            .client_ids(["client1".to_string()].into_iter().collect())
            .build()
            .unwrap();
        let notify = monitor.test_notify_event_processed.clone();

        let handle = tokio::spawn(async move {
            monitor.listen().await;
        });
        wait_for_report(&sender).await;

        tx.send(Event::ClientConnect {
            client_id: "client1".to_string(),
            local_ip: "127.0.0.1".parse().unwrap(),
            conn_id: 1,
        })
        .unwrap();
        notify.notified().await;
        wait_for_report(&sender).await;

        tx.send(Event::ClientDisconnect {
            client_id: "client1".to_string(),
            conn_id: 1,
        })
        .unwrap();
        notify.notified().await;

        let (report, _) = wait_for_report(&sender).await;
        assert_eq!(report.clients["client1"].health, Health::Offline);

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    #[timeout(1000)]
    async fn client_connection_loss_triggers_unstable_then_offline() {
        let (tx, rx) = broadcast::channel(16);
        let sender = MockSender::default();

        let mut monitor = HealthMonitorBuilder::new()
            .sender(sender.clone())
            .event_rx(rx)
            .client_ids(["client1".to_string()].into_iter().collect())
            .build()
            .unwrap();
        let notify = monitor.test_notify_event_processed.clone();

        let handle = tokio::spawn(async move {
            monitor.listen().await;
        });
        wait_for_report(&sender).await;

        tx.send(Event::ClientConnect {
            client_id: "client1".to_string(),
            local_ip: "127.0.0.1".parse().unwrap(),
            conn_id: 1,
        })
        .unwrap();
        notify.notified().await;
        wait_for_report(&sender).await;

        for i in 1..=3 {
            tx.send(Event::ClientConnectionLoss {
                client_id: "client1".to_string(),
                conn_id: i,
            })
            .unwrap();
            notify.notified().await;
        }

        advance(Duration::from_secs(30)).await;
        let (report, _) = wait_for_report(&sender).await;
        assert_eq!(report.clients["client1"].health, Health::Unstable);

        advance(Duration::from_secs(120)).await;
        let (report, _) = wait_for_report(&sender).await;
        assert_eq!(report.clients["client1"].health, Health::Offline);

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    #[timeout(1000)]
    async fn client_recovers_from_unstable_after_recovery_grace_period() {
        let (tx, rx) = broadcast::channel(16);
        let sender = MockSender::default();

        let mut monitor = HealthMonitorBuilder::new()
            .sender(sender.clone())
            .event_rx(rx)
            .client_ids(["client1".to_string()].into_iter().collect())
            .connection_loss_limit(1)
            .build()
            .unwrap();
        let notify = monitor.test_notify_event_processed.clone();

        let handle = tokio::spawn(async move {
            monitor.listen().await;
        });
        wait_for_report(&sender).await;

        tx.send(Event::ClientConnect {
            client_id: "client1".to_string(),
            local_ip: "127.0.0.1".parse().unwrap(),
            conn_id: 1,
        })
        .unwrap();
        notify.notified().await;
        wait_for_report(&sender).await;

        tx.send(Event::ClientConnectionLoss {
            client_id: "client1".to_string(),
            conn_id: 1,
        })
        .unwrap();
        notify.notified().await;
        advance(Duration::from_secs(30)).await;
        let (report, _) = wait_for_report(&sender).await;
        assert_eq!(report.clients["client1"].health, Health::Unstable);

        tx.send(Event::ClientConnect {
            client_id: "client1".to_string(),
            local_ip: "127.0.0.1".parse().unwrap(),
            conn_id: 2,
        })
        .unwrap();
        notify.notified().await;

        advance(Duration::from_secs(1199)).await;
        assert!(sender.last.lock().unwrap().is_none());

        advance(Duration::from_secs(1)).await;
        let (report, _) = wait_for_report(&sender).await;
        assert_eq!(report.clients["client1"].health, Health::Healthy);

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    #[timeout(1000)]
    async fn sends_reminder_reports_if_any_unhealthy() {
        let (tx, rx) = broadcast::channel(16);
        let sender = MockSender::default();

        let mut monitor = HealthMonitorBuilder::new()
            .sender(sender.clone())
            .event_rx(rx)
            .client_ids(["client1".to_string()].into_iter().collect())
            .connection_loss_limit(1)
            .build()
            .unwrap();
        let notify = monitor.test_notify_event_processed.clone();

        let handle = tokio::spawn(async move {
            monitor.listen().await;
        });
        wait_for_report(&sender).await;

        tx.send(Event::ClientConnect {
            client_id: "client1".to_string(),
            local_ip: "127.0.0.1".parse().unwrap(),
            conn_id: 1,
        })
        .unwrap();
        notify.notified().await;
        wait_for_report(&sender).await;

        tx.send(Event::ClientConnectionLoss {
            client_id: "client1".to_string(),
            conn_id: 1,
        })
        .unwrap();
        notify.notified().await;
        advance(Duration::from_secs(30)).await;
        let (report, _) = wait_for_report(&sender).await;
        assert_eq!(report.clients["client1"].health, Health::Unstable);

        advance(Duration::from_secs(1800)).await;
        let (report, _) = wait_for_report(&sender).await;
        assert_ne!(report.clients["client1"].health, Health::Healthy);

        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    #[timeout(1000)]
    async fn client_goes_offline_after_grace_period() {
        let (tx, rx) = broadcast::channel(16);
        let sender = MockSender::default();

        let mut monitor = HealthMonitorBuilder::new()
            .sender(sender.clone())
            .event_rx(rx)
            .client_ids(["client1".to_string()].into_iter().collect())
            .build()
            .unwrap();
        let notify = monitor.test_notify_event_processed.clone();

        let handle = tokio::spawn(async move {
            monitor.listen().await;
        });
        wait_for_report(&sender).await;

        tx.send(Event::ClientConnect {
            client_id: "client1".to_string(),
            local_ip: "127.0.0.1".parse().unwrap(),
            conn_id: 1,
        })
        .unwrap();
        notify.notified().await;

        let report = wait_for_report(&sender).await;
        assert_eq!(report.0.clients["client1"].health, Health::Healthy);

        tx.send(Event::ClientConnectionLoss {
            client_id: "client1".to_string(),
            conn_id: 1,
        })
        .unwrap();
        notify.notified().await;

        advance(Duration::from_secs(120)).await;
        let report = wait_for_report(&sender).await;
        assert_eq!(report.0.clients["client1"].health, Health::Offline);

        drop(tx);
        handle.await.unwrap();
    }
}
