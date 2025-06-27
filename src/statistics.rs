use crate::event::Event;
use async_trait::async_trait;
use bytes::Bytes;
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

/// Server-wide statistics for tracking processed bloops.
///
/// This includes a global counter as well as per-client counters.
#[derive(Debug, Serialize)]
pub struct Statistics {
    /// Total number of bloops processed.
    total_bloops: usize,

    /// Number of bloops processed per client.
    per_client_bloops: HashMap<String, usize>,
}

impl Statistics {
    /// Creates a new statistics snapshot from a given per-client mapping.
    ///
    /// The total bloops count is inferred by summing all per-client counts.
    pub fn new(per_client_bloops: HashMap<String, usize>) -> Self {
        Self {
            total_bloops: per_client_bloops.values().sum(),
            per_client_bloops,
        }
    }

    /// Increments the total and per-client bloop counters.
    ///
    /// If the client ID is new, it will be inserted with an initial count of 1.
    fn increment(&mut self, client_id: &str) {
        self.total_bloops += 1;
        *self
            .per_client_bloops
            .entry(client_id.to_string())
            .or_default() += 1;
    }
}

/// A background service that collects statistics and exposes them over HTTP.
///
/// Tracks how many bloops have been processed in total and per client. Also exposes these
/// statistics over a simple JSON-based HTTP endpoint.
pub struct StatisticsServer {
    addr: SocketAddr,
    stats: Arc<RwLock<Statistics>>,
    event_rx: broadcast::Receiver<Event>,
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
                            let body = match serde_json::to_string(&*stats.read().await) {
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
    /// If the event is a `BloopProcessed`, the statistics are updated. Returns `false` when the
    /// channel is closed, indicating the server should shut down.
    async fn handle_recv(&self, result: Result<Event, RecvError>) -> bool {
        match result {
            Ok(Event::BloopProcessed(bloop)) => {
                self.stats.write().await.increment(&bloop.client_id);
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
        }
    }
}

#[cfg(feature = "tokio-graceful-shutdown")]
#[derive(Debug, Error)]
pub enum NeverError {}

#[cfg(feature = "tokio-graceful-shutdown")]
#[async_trait]
impl IntoSubsystem<NeverError> for StatisticsServer {
    async fn run(mut self, subsys: SubsystemHandle) -> Result<(), NeverError> {
        let _ = self.listen().cancel_on_shutdown(&subsys).await;

        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum BuilderError {
    #[error("missing field: {0}")]
    MissingField(&'static str),
}

/// Builder for creating a [StatisticsServer] instance.
#[derive(Debug, Default)]
pub struct StatisticsServerBuilder {
    addr: Option<SocketAddr>,
    stats: Option<Statistics>,
    event_rx: Option<broadcast::Receiver<Event>>,
}

impl StatisticsServerBuilder {
    /// Creates a new empty builder.
    pub fn new() -> Self {
        Self {
            addr: None,
            stats: None,
            event_rx: None,
        }
    }

    /// Sets the listening address for the statistics server.
    pub fn with_addr(mut self, addr: SocketAddr) -> Self {
        self.addr = Some(addr);
        self
    }

    /// Sets the initial statistics snapshot.
    ///
    /// This is typically loaded from persistent storage (e.g., database) at startup.
    pub fn with_statistics(mut self, stats: Statistics) -> Self {
        self.stats = Some(stats);
        self
    }

    /// Sets the event receiver the server listens to for tracking events.
    pub fn with_event_rx(mut self, event_rx: broadcast::Receiver<Event>) -> Self {
        self.event_rx = Some(event_rx);
        self
    }

    /// Consumes the builder and produces a configured [StatisticsServer].
    pub fn build(self) -> Result<StatisticsServer, BuilderError> {
        Ok(StatisticsServer {
            addr: self.addr.ok_or(BuilderError::MissingField("addr"))?,
            stats: Arc::new(RwLock::new(
                self.stats.ok_or(BuilderError::MissingField("stats"))?,
            )),
            event_rx: self
                .event_rx
                .ok_or(BuilderError::MissingField("event_rx"))?,
        })
    }
}
