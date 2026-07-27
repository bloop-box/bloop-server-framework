//! TLS-based network server for handling authenticated client connections.
//!
//! This module provides a [`NetworkListener`] that manages client
//! authentication, request processing, and message dispatching over a secure
//! TCP connection.
//!
//! The listener is generic over a pair of extension message sets. Custom
//! client messages decode into `Req` and are forwarded, fully typed, over the
//! custom request channel; the handler answers with a [`CustomOutcome`]
//! carrying either a `Res` message or a protocol error. Servers without
//! protocol extensions use the default
//! [`NoExtension`](bloop_protocol::set::NoExtension) sets and never touch any
//! of this.

use std::collections::HashMap;
use std::fmt::Debug;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::result;
use std::sync::Arc;
use std::time::Duration;

use argon2::{Argon2, PasswordVerifier, password_hash::PasswordHashString};
use bloop_protocol::Capabilities;
use bloop_protocol::codec::{Encode, EncodeError};
use bloop_protocol::frame::{FrameError, RawMessage, read_frame, write_frame};
use bloop_protocol::message::{
    Authentication, AuthenticationAccepted, Bloop, ClientHandshake, ClientMessage, ErrorResponse,
    Pong, PreloadCheck, RetrieveAudio, ServerHandshake, ServerMessage,
};
use bloop_protocol::set::{MessageSet, MessageSetError, NoExtension, Payload, encode_message};
use rustls::ServerConfig;
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer,
    pem::{self, PemObject},
};
use thiserror::Error;
use tokio::io::{self, AsyncRead, AsyncWrite, BufReader, BufWriter};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{RwLock, broadcast, mpsc, oneshot};
use tokio::time::timeout;
#[cfg(feature = "tokio-graceful-shutdown")]
use tokio_graceful_shutdown::{FutureExt, IntoSubsystem, SubsystemHandle};
use tokio_rustls::TlsAcceptor;
use tracing::{info, instrument, warn};

use crate::engine::EngineRequest;
use crate::event::Event;

/// Maps client IDs to their stored password hashes (Argon2).
///
/// Used during client authentication.
pub type ClientRegistry = HashMap<String, PasswordHashString>;

/// Default maximum payload length accepted from clients.
///
/// Standard client-to-server payloads are tiny (authentication is the
/// largest); the default leaves generous room for custom extension messages
/// while bounding what a hostile length field can allocate.
pub const DEFAULT_MAX_PAYLOAD_LEN: u32 = 64 * 1024;

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Frame(#[from] FrameError),

    #[error(transparent)]
    Encode(#[from] EncodeError),

    #[error(transparent)]
    Oneshot(#[from] oneshot::error::RecvError),

    #[error(
        "client sent unexpected message with opcode 0x{:02x} and {} payload bytes",
        .0.message_type,
        .0.payload.len()
    )]
    UnexpectedMessage(RawMessage),

    #[error("client sent malformed message")]
    MalformedMessage(#[source] MessageSetError),

    #[error("client requested an unsupported version range: {0} - {1}")]
    UnsupportedVersion(u8, u8),

    #[error("client provided invalid credentials")]
    InvalidCredentials,
}

pub type Result<T> = result::Result<T, Error>;

/// The application's answer to a custom request.
#[derive(Debug)]
pub enum CustomOutcome<Res> {
    /// A custom response message.
    Response(Res),

    /// A protocol error response, e.g. a custom error code.
    Error(ErrorResponse),
}

/// A custom client request forwarded to the application layer.
///
/// The protocol is strictly request-response, so the handler must send an
/// outcome for every request; dropping the sender tears down the client
/// connection.
#[derive(Debug)]
pub struct CustomRequestMessage<Req, Res> {
    pub client_id: String,
    pub request: Req,
    pub response: oneshot::Sender<CustomOutcome<Res>>,
}

/// A TLS-secured TCP server that accepts client connections, authenticates
/// them, and dispatches their requests to the appropriate handlers.
///
/// This listener handles authentication, version negotiation, and supports
/// custom client messages through the `Req`/`Res` extension sets.
///
/// # Examples
///
/// ```no_run
/// use std::sync::Arc;
/// use tokio::sync::{mpsc, broadcast, RwLock};
/// use bloop_server_framework::network::NetworkListenerBuilder;
///
/// #[tokio::main]
/// async fn main() {
///   let clients = Arc::new(RwLock::new(Default::default()));
///   let (engine_tx, _) = mpsc::channel(10);
///   let (event_tx, _) = broadcast::channel(10);
///
///   let listener = NetworkListenerBuilder::new()
///       .address("127.0.0.1:12345")
///       .cert_path("server.crt")
///       .key_path("server.key")
///       .clients(clients)
///       .engine_tx(engine_tx)
///       .event_tx(event_tx)
///       .build()
///       .unwrap();
///
///   listener.listen().await.unwrap();
/// }
/// ```
pub struct NetworkListener<Req = NoExtension, Res = NoExtension> {
    clients: Arc<RwLock<ClientRegistry>>,
    addr: SocketAddr,
    tls_acceptor: TlsAcceptor,
    engine_tx: mpsc::Sender<(EngineRequest, oneshot::Sender<ServerMessage>)>,
    event_tx: broadcast::Sender<Event>,
    custom_req_tx: Option<mpsc::Sender<CustomRequestMessage<Req, Res>>>,
    max_payload_len: u32,
}

impl<Req, Res> Debug for NetworkListener<Req, Res> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkListener")
            .field("clients", &self.clients)
            .field("addr", &self.addr)
            .field("engine_tx", &self.engine_tx)
            .field("event_tx", &self.event_tx)
            .field("custom_req_tx", &self.custom_req_tx)
            .field("max_payload_len", &self.max_payload_len)
            .finish()
    }
}

impl<Req, Res> NetworkListener<Req, Res>
where
    Req: MessageSet + Send + 'static,
    Res: MessageSet + Send + 'static,
{
    /// Starts listening for incoming TCP connections.
    ///
    /// This method blocks indefinitely, accepting and processing new connections.
    /// Each connection is handled asynchronously in its own task.
    ///
    /// Returns an error only if the server fails to bind to the specified address.
    pub async fn listen(&self) -> Result<()> {
        let listener = TcpListener::bind(self.addr).await?;
        let mut con_counter: usize = 0;

        loop {
            let (stream, peer_addr) = listener.accept().await?;
            let conn_id = con_counter;
            con_counter += 1;
            let event_tx = self.event_tx.clone();

            self.handle_stream(stream, peer_addr, self.clients.clone(), conn_id, event_tx);
        }
    }

    #[instrument(skip(self, stream, peer_addr, clients, event_tx))]
    fn handle_stream(
        &self,
        stream: TcpStream,
        peer_addr: SocketAddr,
        clients: Arc<RwLock<ClientRegistry>>,
        conn_id: usize,
        event_tx: broadcast::Sender<Event>,
    ) {
        let acceptor = self.tls_acceptor.clone();
        let engine_tx = self.engine_tx.clone();
        let custom_req_tx = self.custom_req_tx.clone();
        let max_payload_len = self.max_payload_len;

        tokio::spawn(async move {
            info!("new connection from {}", peer_addr);

            let stream = match acceptor.accept(stream).await {
                Ok(stream) => stream,
                Err(error) => {
                    warn!("failed to accept stream: {}", error);
                    return;
                }
            };

            let (reader, writer) = io::split(stream);
            let mut reader = BufReader::new(reader);
            let mut writer = BufWriter::new(writer);

            let (client_id, local_ip, _version) = match timeout(
                Duration::from_secs(2),
                authenticate::<_, _, Req>(&mut reader, &mut writer, clients, max_payload_len),
            )
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    if let Some(response) = error_response(&error) {
                        warn!("client error: {}", error);
                        let _ = write_payload(&mut writer, &response).await;
                    } else {
                        warn!("client error: connection died: {:?}", error);
                    }

                    return;
                }
                Err(_) => {
                    warn!("client error: authentication timed out");
                    return;
                }
            };

            let _ = event_tx.send(Event::ClientConnect {
                client_id: client_id.clone(),
                conn_id,
                local_ip,
            });

            match handle_connection(
                &mut reader,
                &mut writer,
                &client_id,
                engine_tx,
                custom_req_tx,
                max_payload_len,
            )
            .await
            {
                Ok(()) => {
                    let _ = event_tx.send(Event::ClientDisconnect { client_id, conn_id });
                }
                Err(error) => {
                    if let Some(response) = error_response(&error) {
                        warn!("client error: {}", error);
                        let _ = write_payload(&mut writer, &response).await;
                    } else if matches!(error, Error::Oneshot(_) | Error::Encode(_)) {
                        warn!("internal error while serving client: {:?}", error);
                    } else {
                        warn!("client error: connection died: {:?}", error);
                    }

                    let _ = event_tx.send(Event::ClientConnectionLoss { client_id, conn_id });
                }
            }
        });
    }
}

#[cfg(feature = "tokio-graceful-shutdown")]
impl<Req, Res> IntoSubsystem<Error> for NetworkListener<Req, Res>
where
    Req: MessageSet + Send + 'static,
    Res: MessageSet + Send + 'static,
{
    async fn run(self, subsys: &mut SubsystemHandle) -> Result<()> {
        if let Ok(result) = self.listen().cancel_on_shutdown(subsys).await {
            result?
        }

        Ok(())
    }
}

/// Maps a connection error onto the error response owed to the client.
///
/// Returns `None` for transport errors, where the connection is gone and no
/// response can be delivered.
fn error_response(error: &Error) -> Option<ErrorResponse> {
    match error {
        Error::UnexpectedMessage(_) => Some(ErrorResponse::UnexpectedMessage),
        Error::MalformedMessage(_) => Some(ErrorResponse::MalformedMessage),
        Error::Frame(FrameError::PayloadTooLarge { .. }) => Some(ErrorResponse::MalformedMessage),
        Error::UnsupportedVersion(_, _) => Some(ErrorResponse::UnsupportedVersionRange),
        Error::InvalidCredentials => Some(ErrorResponse::InvalidCredentials),
        _ => None,
    }
}

async fn read_message<S, Req>(stream: &mut S, max_payload_len: u32) -> Result<ClientMessage<Req>>
where
    S: AsyncRead + Unpin,
    Req: MessageSet,
{
    let raw = read_frame(stream, max_payload_len).await?;

    ClientMessage::decode(&raw).map_err(|error| match error {
        MessageSetError::UnknownOpcode(_) => Error::UnexpectedMessage(raw),
        error => Error::MalformedMessage(error),
    })
}

async fn write_message<S, M>(stream: &mut S, message: M) -> Result<()>
where
    S: AsyncWrite + Unpin,
    M: MessageSet,
{
    write_frame(stream, &message.encode()?).await?;
    Ok(())
}

async fn write_payload<S, M>(stream: &mut S, message: &M) -> Result<()>
where
    S: AsyncWrite + Unpin,
    M: Payload + Encode,
{
    write_frame(stream, &encode_message(message)?).await?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum BuilderError {
    #[error("missing field: {0}")]
    MissingField(&'static str),

    #[error(transparent)]
    AddrParse(#[from] std::net::AddrParseError),

    #[error("failed to read PEM file at {path}: {source}")]
    Pem {
        path: PathBuf,
        #[source]
        source: pem::Error,
    },

    #[error(transparent)]
    Rustls(#[from] rustls::Error),
}

pub type BuilderResult<T> = result::Result<T, BuilderError>;

/// Builder for [`NetworkListener`].
///
/// This allows configuring the address, TLS certificates, client registry,
/// message channels, and custom request handlers. Registering a custom
/// request channel via [`custom_req_tx`](Self::custom_req_tx) selects the
/// listener's extension sets; without one the listener speaks the standard
/// protocol only.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use tokio::sync::{broadcast, RwLock};
/// use tokio::sync::mpsc;
/// use bloop_server_framework::network::NetworkListenerBuilder;
///
/// let (engine_tx, engine_rx) = mpsc::channel(512);
/// let (event_tx, event_rx) = broadcast::channel(512);
///
/// # let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
/// let builder = NetworkListenerBuilder::new()
///     .address("127.0.0.1:12345")
///     .clients(Arc::new(RwLock::new(Default::default())))
///     .engine_tx(engine_tx)
///     .event_tx(event_tx)
///     .cert_path("examples/cert.pem")
///     .key_path("examples/key.pem")
///     .build()
///     .unwrap();
/// ```
#[derive(Debug)]
pub struct NetworkListenerBuilder<Req = NoExtension, Res = NoExtension> {
    address: Option<String>,
    cert_path: Option<PathBuf>,
    key_path: Option<PathBuf>,
    clients: Option<Arc<RwLock<ClientRegistry>>>,
    engine_tx: Option<mpsc::Sender<(EngineRequest, oneshot::Sender<ServerMessage>)>>,
    event_tx: Option<broadcast::Sender<Event>>,
    custom_req_tx: Option<mpsc::Sender<CustomRequestMessage<Req, Res>>>,
    max_payload_len: u32,
}

impl NetworkListenerBuilder {
    /// Creates a builder for a listener without protocol extensions.
    pub fn new() -> Self {
        Self {
            address: None,
            cert_path: None,
            key_path: None,
            clients: None,
            engine_tx: None,
            event_tx: None,
            custom_req_tx: None,
            max_payload_len: DEFAULT_MAX_PAYLOAD_LEN,
        }
    }
}

impl Default for NetworkListenerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl<Req, Res> NetworkListenerBuilder<Req, Res> {
    /// Registers the custom request channel, selecting the listener's
    /// extension sets from the channel's message type.
    pub fn custom_req_tx<Req2, Res2>(
        self,
        custom_req_tx: mpsc::Sender<CustomRequestMessage<Req2, Res2>>,
    ) -> NetworkListenerBuilder<Req2, Res2> {
        NetworkListenerBuilder {
            clients: self.clients,
            address: self.address,
            cert_path: self.cert_path,
            key_path: self.key_path,
            engine_tx: self.engine_tx,
            event_tx: self.event_tx,
            custom_req_tx: Some(custom_req_tx),
            max_payload_len: self.max_payload_len,
        }
    }

    pub fn address(mut self, address: impl Into<String>) -> Self {
        self.address = Some(address.into());
        self
    }

    pub fn cert_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.cert_path = Some(path.into());
        self
    }

    pub fn key_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.key_path = Some(path.into());
        self
    }

    pub fn clients(mut self, clients: Arc<RwLock<ClientRegistry>>) -> Self {
        self.clients = Some(clients);
        self
    }

    pub fn engine_tx(
        mut self,
        tx: mpsc::Sender<(EngineRequest, oneshot::Sender<ServerMessage>)>,
    ) -> Self {
        self.engine_tx = Some(tx);
        self
    }

    pub fn event_tx(mut self, tx: broadcast::Sender<Event>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Overrides the maximum payload length accepted from clients.
    pub fn max_payload_len(mut self, max_payload_len: u32) -> Self {
        self.max_payload_len = max_payload_len;
        self
    }

    /// Builds the [`NetworkListener`] from the provided configuration.
    ///
    /// Returns an error if required fields are missing, or TLS setup fails.
    pub fn build(self) -> BuilderResult<NetworkListener<Req, Res>> {
        let addr: SocketAddr = self
            .address
            .ok_or_else(|| BuilderError::MissingField("address"))?
            .parse()?;

        let cert_path = self
            .cert_path
            .ok_or_else(|| BuilderError::MissingField("cert_path"))?;
        let key_path = self
            .key_path
            .ok_or_else(|| BuilderError::MissingField("key_path"))?;

        let certs = CertificateDer::pem_file_iter(&cert_path)
            .map_err(|err| BuilderError::Pem {
                path: cert_path.clone(),
                source: err,
            })?
            .collect::<result::Result<Vec<_>, _>>()
            .map_err(|err| BuilderError::Pem {
                path: cert_path,
                source: err,
            })?;
        let key = PrivateKeyDer::from_pem_file(&key_path).map_err(|err| BuilderError::Pem {
            path: key_path,
            source: err,
        })?;

        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
        let tls_acceptor = TlsAcceptor::from(Arc::new(config));

        Ok(NetworkListener {
            clients: self
                .clients
                .ok_or_else(|| BuilderError::MissingField("clients"))?,
            addr,
            tls_acceptor,
            engine_tx: self
                .engine_tx
                .ok_or_else(|| BuilderError::MissingField("engine_tx"))?,
            event_tx: self
                .event_tx
                .ok_or_else(|| BuilderError::MissingField("event_tx"))?,
            custom_req_tx: self.custom_req_tx,
            max_payload_len: self.max_payload_len,
        })
    }
}

/// Handles an authenticated client connection.
///
/// Reads client messages from the stream, dispatches them to the appropriate
/// handlers, and sends back server responses.
async fn handle_connection<R, W, Req, Res>(
    reader: &mut R,
    writer: &mut W,
    client_id: &str,
    engine_tx: mpsc::Sender<(EngineRequest, oneshot::Sender<ServerMessage>)>,
    custom_req_tx: Option<mpsc::Sender<CustomRequestMessage<Req, Res>>>,
    max_payload_len: u32,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    Req: MessageSet,
    Res: MessageSet,
{
    loop {
        let message = match timeout(
            Duration::from_secs(30),
            read_message::<_, Req>(reader, max_payload_len),
        )
        .await
        {
            Ok(Ok(message)) => message,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Ok(()),
        };

        let engine_request = match message {
            ClientMessage::Bloop(Bloop { nfc_uid }) => EngineRequest::Bloop {
                nfc_uid,
                client_id: client_id.to_string(),
            },
            ClientMessage::RetrieveAudio(RetrieveAudio { achievement_id }) => {
                EngineRequest::RetrieveAudio { id: achievement_id }
            }
            ClientMessage::PreloadCheck(PreloadCheck {
                audio_manifest_hash,
            }) => EngineRequest::PreloadCheck {
                manifest_hash: audio_manifest_hash,
            },
            ClientMessage::Ping(_) => {
                write_payload(writer, &Pong).await?;
                continue;
            }
            ClientMessage::Quit(_) => break,
            ClientMessage::Custom(request) => {
                let Some(sender) = custom_req_tx.as_ref() else {
                    return Err(Error::UnexpectedMessage(request.encode()?));
                };

                let (resp_tx, resp_rx) = oneshot::channel();

                let _ = sender
                    .send(CustomRequestMessage {
                        client_id: client_id.to_string(),
                        request,
                        response: resp_tx,
                    })
                    .await;

                match resp_rx.await? {
                    CustomOutcome::Response(response) => {
                        write_message(writer, response).await?;
                    }
                    CustomOutcome::Error(error) => {
                        write_payload(writer, &error).await?;
                    }
                }

                continue;
            }
            message => return Err(Error::UnexpectedMessage(message.encode()?)),
        };

        let (resp_tx, resp_rx) = oneshot::channel();
        let _ = engine_tx.send((engine_request, resp_tx)).await;
        let response = resp_rx.await?;

        write_message(writer, response).await?;
    }

    Ok(())
}

/// Authenticates a client by performing handshake and credential verification.
///
/// Returns the client ID, its IP address, and the negotiated protocol version
/// on success.
async fn authenticate<R, W, Req>(
    reader: &mut R,
    writer: &mut W,
    clients: Arc<RwLock<ClientRegistry>>,
    max_payload_len: u32,
) -> Result<(String, IpAddr, u8)>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    Req: MessageSet,
{
    let (min_version, max_version) = match read_message::<_, Req>(reader, max_payload_len).await? {
        ClientMessage::Handshake(ClientHandshake {
            min_version,
            max_version,
        }) => (min_version, max_version),
        message => return Err(Error::UnexpectedMessage(message.encode()?)),
    };

    if min_version > 3 || max_version < 3 {
        return Err(Error::UnsupportedVersion(min_version, max_version));
    }

    write_payload(
        writer,
        &ServerHandshake {
            accepted_version: 3,
            capabilities: Capabilities::PreloadCheck,
        },
    )
    .await?;

    let (client_id, client_secret, ip_address) =
        match read_message::<_, Req>(reader, max_payload_len).await? {
            ClientMessage::Authentication(Authentication {
                client_id,
                client_secret,
                ip_address,
            }) => (client_id, client_secret, ip_address),
            message => return Err(Error::UnexpectedMessage(message.encode()?)),
        };

    let clients = clients.read().await;
    let Some(secret_hash) = clients.get(&client_id) else {
        return Err(Error::InvalidCredentials);
    };

    if Argon2::default()
        .verify_password(client_secret.as_bytes(), &secret_hash.password_hash())
        .is_err()
    {
        return Err(Error::InvalidCredentials);
    }

    write_payload(writer, &AuthenticationAccepted).await?;

    Ok((client_id.to_string(), ip_address, 3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bloop_protocol::DataHash;
    use bloop_protocol::message::{AchievementRecord, AudioData, BloopAccepted, PreloadMatch};
    use std::fs;
    use tempfile::tempdir;
    use uuid::Uuid;

    #[tokio::test]
    async fn builder_fails_with_missing_fields() {
        let builder = NetworkListenerBuilder::new();
        let result = builder.build();
        assert!(matches!(result, Err(BuilderError::MissingField(_))));
    }

    #[tokio::test]
    async fn builder_fails_with_invalid_address() {
        let builder = NetworkListenerBuilder::new()
            .address("invalid-addr")
            .cert_path("cert.pem")
            .key_path("key.pem")
            .clients(Arc::new(RwLock::new(Default::default())))
            .engine_tx(dummy_engine_tx())
            .event_tx(dummy_event_tx());

        let result = builder.build();
        assert!(matches!(result, Err(BuilderError::AddrParse(_))));
    }

    #[tokio::test]
    async fn builder_fails_on_invalid_pem_files() {
        let dir = tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        fs::write(&cert_path, b"invalid-cert").unwrap();
        fs::write(&key_path, b"invalid-key").unwrap();

        let builder = NetworkListenerBuilder::new()
            .address("127.0.0.1:12345")
            .cert_path(&cert_path)
            .key_path(&key_path)
            .clients(Arc::new(RwLock::new(Default::default())))
            .engine_tx(dummy_engine_tx())
            .event_tx(dummy_event_tx());

        let result = builder.build();
        assert!(matches!(result, Err(BuilderError::Pem { .. })));
    }

    #[tokio::test]
    async fn builder_succeeds_with_valid_dummy_pem() {
        let dir = tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");

        let cert_data = include_bytes!("../examples/cert.pem");
        let key_data = include_bytes!("../examples/key.pem");

        fs::write(&cert_path, cert_data).unwrap();
        fs::write(&key_path, key_data).unwrap();

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let builder = NetworkListenerBuilder::new()
            .address("127.0.0.1:12345")
            .cert_path(&cert_path)
            .key_path(&key_path)
            .clients(Arc::new(RwLock::new(Default::default())))
            .engine_tx(dummy_engine_tx())
            .event_tx(dummy_event_tx());

        let result = builder.build();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn authentication_fails_with_wrong_client_id() {
        let clients = Arc::new(RwLock::new(Default::default()));

        let client_handshake = build_handshake(3, 3);
        let authentication = build_authentication("unknown-client", "password", "127.0.0.1");

        let mut reader = tokio_test::io::Builder::new()
            .read(&client_handshake)
            .read(&authentication)
            .build();
        let mut writer = tokio_test::io::Builder::new()
            .write(&frame_bytes(&ServerHandshake {
                accepted_version: 3,
                capabilities: Capabilities::PreloadCheck,
            }))
            .build();

        let result = authenticate::<_, _, NoExtension>(
            &mut reader,
            &mut writer,
            clients,
            DEFAULT_MAX_PAYLOAD_LEN,
        )
        .await;

        assert!(matches!(result, Err(Error::InvalidCredentials)));
    }

    #[tokio::test]
    async fn authentication_succeeds_with_correct_credentials() {
        let clients = test_clients().await;

        let client_handshake = build_handshake(3, 3);
        let authentication = build_authentication("client", "secret", "127.0.0.1");

        let mut reader = tokio_test::io::Builder::new()
            .read(&client_handshake)
            .read(&authentication)
            .build();
        let mut writer = tokio_test::io::Builder::new()
            .write(&frame_bytes(&ServerHandshake {
                accepted_version: 3,
                capabilities: Capabilities::PreloadCheck,
            }))
            .write(&frame_bytes(&AuthenticationAccepted))
            .build();

        let result = authenticate::<_, _, NoExtension>(
            &mut reader,
            &mut writer,
            clients,
            DEFAULT_MAX_PAYLOAD_LEN,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn authentication_fails_with_wrong_password() {
        let clients = test_clients().await;

        let client_handshake = build_handshake(3, 3);
        let authentication = build_authentication("client1", "wrong-secret", "127.0.0.1");

        let mut reader = tokio_test::io::Builder::new()
            .read(&client_handshake)
            .read(&authentication)
            .build();
        let mut writer = tokio_test::io::Builder::new()
            .write(&frame_bytes(&ServerHandshake {
                accepted_version: 3,
                capabilities: Capabilities::PreloadCheck,
            }))
            .build();

        let result = authenticate::<_, _, NoExtension>(
            &mut reader,
            &mut writer,
            clients,
            DEFAULT_MAX_PAYLOAD_LEN,
        )
        .await;

        assert!(matches!(result, Err(Error::InvalidCredentials)));
    }

    #[derive(
        Clone,
        Debug,
        PartialEq,
        bloop_protocol::Encode,
        bloop_protocol::Decode,
        bloop_protocol::Payload,
    )]
    #[bloop(opcode = 0x80)]
    struct EchoRequest {
        text: String,
    }

    #[derive(
        Clone,
        Debug,
        PartialEq,
        bloop_protocol::Encode,
        bloop_protocol::Decode,
        bloop_protocol::Payload,
    )]
    #[bloop(opcode = 0x81)]
    struct EchoResponse {
        text: String,
    }

    #[derive(Clone, Debug, PartialEq, bloop_protocol::MessageSet)]
    enum TestRequest {
        Echo(EchoRequest),
    }

    #[derive(Clone, Debug, PartialEq, bloop_protocol::MessageSet)]
    enum TestResponse {
        Echo(EchoResponse),
    }

    /// Scripted raw-byte session covering the whole v3 message flow.
    ///
    /// This is the load-bearing wire-compatibility test: the client side of
    /// the duplex writes literal protocol bytes and asserts literal response
    /// bytes, so codec regressions cannot cancel out.
    #[tokio::test]
    async fn full_session_speaks_v3_bytes() {
        use tokio::io::AsyncWriteExt;

        let achievement_id = Uuid::from_bytes([7; 16]);
        let audio_hash = DataHash::try_from(vec![9u8; 16]).unwrap();

        // Fake engine answering fixed responses.
        let (engine_tx, mut engine_rx) =
            mpsc::channel::<(EngineRequest, oneshot::Sender<ServerMessage>)>(8);
        let engine_audio_hash = audio_hash.clone();

        tokio::spawn(async move {
            while let Some((request, response)) = engine_rx.recv().await {
                let message: ServerMessage = match request {
                    EngineRequest::Bloop { .. } => BloopAccepted {
                        achievements: vec![AchievementRecord {
                            id: achievement_id,
                            audio_hash: Some(engine_audio_hash.clone()),
                        }],
                    }
                    .into(),
                    EngineRequest::RetrieveAudio { .. } => AudioData {
                        data: vec![1, 2, 3],
                    }
                    .into(),
                    EngineRequest::PreloadCheck { .. } => PreloadMatch.into(),
                };

                let _ = response.send(message);
            }
        });

        // Custom handler echoing the text reversed, or answering a custom
        // error code for the magic word.
        let (custom_tx, mut custom_rx) =
            mpsc::channel::<CustomRequestMessage<TestRequest, TestResponse>>(8);

        tokio::spawn(async move {
            while let Some(request) = custom_rx.recv().await {
                assert_eq!(request.client_id, "client");
                let TestRequest::Echo(echo) = request.request;

                let outcome = if echo.text == "fail" {
                    CustomOutcome::Error(ErrorResponse::Custom(0x90))
                } else {
                    CustomOutcome::Response(TestResponse::Echo(EchoResponse {
                        text: echo.text.chars().rev().collect(),
                    }))
                };

                let _ = request.response.send(outcome);
            }
        });

        let (mut client, server) = tokio::io::duplex(64 * 1024);

        let server_task = tokio::spawn(async move {
            let (reader, writer) = io::split(server);
            let mut reader = BufReader::new(reader);
            let mut writer = BufWriter::new(writer);

            let (client_id, _, _) = authenticate::<_, _, TestRequest>(
                &mut reader,
                &mut writer,
                test_clients().await,
                DEFAULT_MAX_PAYLOAD_LEN,
            )
            .await
            .unwrap();

            handle_connection(
                &mut reader,
                &mut writer,
                &client_id,
                engine_tx,
                Some(custom_tx),
                DEFAULT_MAX_PAYLOAD_LEN,
            )
            .await
            .unwrap();
        });

        // Handshake.
        client.write_all(&[0x01, 2, 0, 0, 0, 3, 3]).await.unwrap();
        let mut expected = vec![0x02, 9, 0, 0, 0, 3];
        expected.extend_from_slice(&1u64.to_le_bytes());
        assert_eq!(read_bytes(&mut client, expected.len()).await, expected);

        // Authentication.
        client
            .write_all(&build_authentication("client", "secret", "127.0.0.1"))
            .await
            .unwrap();
        assert_eq!(read_bytes(&mut client, 5).await, [0x04, 0, 0, 0, 0]);

        // Ping.
        client.write_all(&[0x05, 0, 0, 0, 0]).await.unwrap();
        assert_eq!(read_bytes(&mut client, 5).await, [0x06, 0, 0, 0, 0]);

        // Bloop.
        client
            .write_all(&[0x08, 5, 0, 0, 0, 4, 1, 2, 3, 4])
            .await
            .unwrap();
        let mut expected = vec![0x09, 34, 0, 0, 0, 1];
        expected.extend_from_slice(achievement_id.as_bytes());
        expected.push(16);
        expected.extend_from_slice(audio_hash.as_bytes());
        assert_eq!(read_bytes(&mut client, expected.len()).await, expected);

        // Retrieve audio.
        let mut request = vec![0x0a, 16, 0, 0, 0];
        request.extend_from_slice(achievement_id.as_bytes());
        client.write_all(&request).await.unwrap();
        assert_eq!(
            read_bytes(&mut client, 12).await,
            [0x0b, 7, 0, 0, 0, 3, 0, 0, 0, 1, 2, 3]
        );

        // Preload check without a stored hash.
        client.write_all(&[0x0c, 1, 0, 0, 0, 0]).await.unwrap();
        assert_eq!(read_bytes(&mut client, 5).await, [0x0d, 0, 0, 0, 0]);

        // Custom echo request.
        client
            .write_all(&[0x80, 3, 0, 0, 0, 2, b'h', b'i'])
            .await
            .unwrap();
        assert_eq!(
            read_bytes(&mut client, 8).await,
            [0x81, 3, 0, 0, 0, 2, b'i', b'h']
        );

        // Custom request answered with a custom error code.
        client
            .write_all(&[0x80, 5, 0, 0, 0, 4, b'f', b'a', b'i', b'l'])
            .await
            .unwrap();
        assert_eq!(read_bytes(&mut client, 6).await, [0x00, 1, 0, 0, 0, 0x90]);

        // Quit.
        client.write_all(&[0x07, 0, 0, 0, 0]).await.unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn errors_map_to_the_owed_protocol_responses() {
        assert!(matches!(
            error_response(&Error::UnexpectedMessage(RawMessage::new(0xff, vec![]))),
            Some(ErrorResponse::UnexpectedMessage)
        ));
        assert!(matches!(
            error_response(&Error::MalformedMessage(MessageSetError::UnknownOpcode(
                0xff
            ))),
            Some(ErrorResponse::MalformedMessage)
        ));
        assert!(matches!(
            error_response(&Error::Frame(FrameError::PayloadTooLarge {
                length: 100,
                max: 10,
            })),
            Some(ErrorResponse::MalformedMessage)
        ));
        assert!(matches!(
            error_response(&Error::UnsupportedVersion(1, 2)),
            Some(ErrorResponse::UnsupportedVersionRange)
        ));
        assert!(matches!(
            error_response(&Error::InvalidCredentials),
            Some(ErrorResponse::InvalidCredentials)
        ));

        let (tx, rx) = oneshot::channel::<()>();
        drop(tx);
        let recv_error = rx.await.unwrap_err();

        assert!(error_response(&Error::Io(io::Error::other("gone"))).is_none());
        assert!(error_response(&Error::Oneshot(recv_error)).is_none());
    }

    #[tokio::test]
    async fn error_responses_encode_to_v3_error_frames() {
        use tokio::io::AsyncWriteExt;

        let (mut client, mut server) = tokio::io::duplex(1024);

        write_payload(&mut client, &ErrorResponse::UnexpectedMessage)
            .await
            .unwrap();
        client.flush().await.unwrap();

        assert_eq!(read_bytes(&mut server, 6).await, [0x00, 1, 0, 0, 0, 0x00]);
    }

    #[tokio::test]
    async fn unknown_opcode_is_answered_with_unexpected_message() {
        let (engine_tx, _engine_rx) = mpsc::channel(1);
        let (mut client, server) = tokio::io::duplex(1024);

        let server_task = tokio::spawn(async move {
            let (reader, writer) = io::split(server);
            let mut reader = BufReader::new(reader);
            let mut writer = BufWriter::new(writer);

            let result = handle_connection::<_, _, NoExtension, NoExtension>(
                &mut reader,
                &mut writer,
                "client",
                engine_tx,
                None,
                DEFAULT_MAX_PAYLOAD_LEN,
            )
            .await;

            assert!(matches!(result, Err(Error::UnexpectedMessage(_))));
        });

        use tokio::io::AsyncWriteExt;
        client.write_all(&[0xff, 0, 0, 0, 0]).await.unwrap();
        server_task.await.unwrap();
    }

    async fn read_bytes<S: AsyncRead + Unpin>(stream: &mut S, count: usize) -> Vec<u8> {
        use tokio::io::AsyncReadExt;

        let mut bytes = vec![0; count];
        stream.read_exact(&mut bytes).await.unwrap();
        bytes
    }

    async fn test_clients() -> Arc<RwLock<ClientRegistry>> {
        let clients = Arc::new(RwLock::new(HashMap::default()));
        clients.write().await.insert(
            "client".into(),
            PasswordHashString::new(
                "$argon2id$v=19$m=10,t=1,p=1$THh0RHE5YWNkQUZNa2lqUA$dmB4X7J49jjCGA",
            )
            .unwrap(),
        );

        clients
    }

    fn frame_bytes<M: Payload + Encode>(message: &M) -> Vec<u8> {
        let raw = encode_message(message).unwrap();
        let mut bytes = vec![raw.message_type];
        bytes.extend_from_slice(&(raw.payload.len() as u32).to_le_bytes());
        bytes.extend(raw.payload);

        bytes
    }

    fn dummy_engine_tx() -> mpsc::Sender<(EngineRequest, oneshot::Sender<ServerMessage>)> {
        let (tx, _rx) = mpsc::channel(1);
        tx
    }

    fn dummy_event_tx() -> broadcast::Sender<Event> {
        let (tx, _rx) = broadcast::channel(1);
        tx
    }

    fn build_handshake(min_version: u8, max_version: u8) -> Vec<u8> {
        let mut buf = Vec::new();
        let payload = [min_version, max_version];

        buf.push(0x01);
        buf.extend(&(payload.len() as u32).to_le_bytes());
        buf.extend(&payload);

        buf
    }

    fn build_authentication(client_id: &str, password: &str, ip_addr: &str) -> Vec<u8> {
        use std::net::IpAddr;

        let mut buf = Vec::new();

        let client_id_bytes = client_id.as_bytes();
        let password_bytes = password.as_bytes();

        let mut payload = Vec::new();
        payload.push(client_id_bytes.len() as u8);
        payload.extend(client_id_bytes);

        payload.push(password_bytes.len() as u8);
        payload.extend(password_bytes);

        let ip: IpAddr = ip_addr.parse().expect("Invalid IP address");
        match ip {
            IpAddr::V4(v4) => {
                payload.push(4); // IPv4
                payload.extend(&v4.octets());
            }
            IpAddr::V6(v6) => {
                payload.push(6); // IPv6
                payload.extend(&v6.octets());
            }
        }

        buf.push(0x03);
        buf.extend(&(payload.len() as u32).to_le_bytes());
        buf.extend(payload);

        buf
    }
}
