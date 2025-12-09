//! TLS-based network server for handling authenticated client connections.
//!
//! This module provides a [`NetworkListener`] that manages client authentication,
//! request processing, and message dispatching over a secure TCP connection.

use crate::engine::EngineRequest;
use crate::event::Event;
use crate::message::{Capabilities, ClientMessage, ErrorResponse, Message, ServerMessage};
use argon2::{Argon2, PasswordVerifier, password_hash::PasswordHashString};
use rustls::ServerConfig;
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer,
    pem::{self, PemObject},
};
use std::collections::HashMap;
use std::fmt::Debug;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::result;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{RwLock, broadcast, mpsc, oneshot};
use tokio::time::timeout;
#[cfg(feature = "tokio-graceful-shutdown")]
use tokio_graceful_shutdown::{FutureExt, IntoSubsystem, SubsystemHandle};
use tokio_rustls::TlsAcceptor;
use tracing::{info, instrument, warn};

/// Maps client IDs to their stored password hashes (Argon2).
///
/// Used during client authentication.
pub type ClientRegistry = HashMap<String, PasswordHashString>;

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Oneshot(#[from] oneshot::error::RecvError),

    #[error("client sent unexpected message: {0:?}")]
    UnexpectedMessage(ClientMessage),

    #[error("client requested an unsupported version range: {0} - {1}")]
    UnsupportedVersion(u8, u8),

    #[error("unrecognized request code: {0}")]
    UnknownRequest(u8),

    #[error("client provided invalid credentials")]
    InvalidCredentials,
}

pub type Result<T> = result::Result<T, Error>;

/// A wrapper for custom client requests that are forwarded to the application
/// layer.
#[derive(Debug)]
#[allow(dead_code)]
pub struct CustomRequestMessage {
    client_id: String,
    message: Message,
    response: oneshot::Sender<Option<Message>>,
}

/// A TLS-secured TCP server that accepts client connections, authenticates
/// them, and dispatches their requests to the appropriate handlers.
///
/// This listener handles authentication, version negotiation, and supports
/// custom client messages.
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
pub struct NetworkListener {
    clients: Arc<RwLock<ClientRegistry>>,
    addr: SocketAddr,
    tls_acceptor: TlsAcceptor,
    engine_tx: mpsc::Sender<(EngineRequest, oneshot::Sender<ServerMessage>)>,
    event_tx: broadcast::Sender<Event>,
    custom_req_tx: Option<mpsc::Sender<CustomRequestMessage>>,
}

impl Debug for NetworkListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkListener")
            .field("clients", &self.clients)
            .field("addr", &self.addr)
            .field("engine_tx", &self.engine_tx)
            .field("event_tx", &self.event_tx)
            .field("custom_req_tx", &self.custom_req_tx)
            .finish()
    }
}

impl NetworkListener {
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
                authenticate(&mut reader, &mut writer, clients),
            )
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(Error::UnexpectedMessage(message))) => {
                    warn!("client error: unexpected message: {:?}", message);
                    let _ = write_to_stream(
                        &mut writer,
                        ServerMessage::Error(ErrorResponse::UnexpectedMessage),
                    )
                    .await;
                    return;
                }
                Ok(Err(Error::UnsupportedVersion(min_version, max_version))) => {
                    warn!("client error: unsupported version range: {min_version} - {max_version}");
                    let _ = write_to_stream(
                        &mut writer,
                        ServerMessage::Error(ErrorResponse::UnsupportedVersionRange),
                    )
                    .await;
                    return;
                }
                Ok(Err(Error::InvalidCredentials)) => {
                    warn!("client error: invalid credentials");
                    let _ = write_to_stream(
                        &mut writer,
                        ServerMessage::Error(ErrorResponse::InvalidCredentials),
                    )
                    .await;
                    return;
                }
                Ok(Err(Error::Io(error)))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData
                    ) =>
                {
                    warn!("client error: malformed message: {:?}", error);
                    let _ = write_to_stream(
                        &mut writer,
                        ServerMessage::Error(ErrorResponse::MalformedMessage),
                    )
                    .await;
                    return;
                }
                Ok(Err(error)) => {
                    warn!("client error: connection died: {:?}", error);
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
            )
            .await
            {
                Ok(()) => {
                    let _ = event_tx.send(Event::ClientDisconnect { client_id, conn_id });
                    return;
                }
                Err(Error::UnexpectedMessage(message)) => {
                    warn!("client error: unexpected message: {:?}", message);
                    let _ = write_to_stream(
                        &mut writer,
                        ServerMessage::Error(ErrorResponse::UnexpectedMessage),
                    )
                    .await;
                }
                Err(Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData
                    ) =>
                {
                    warn!("client error: malformed message: {:?}", error);
                    let _ = write_to_stream(
                        &mut writer,
                        ServerMessage::Error(ErrorResponse::MalformedMessage),
                    )
                    .await;
                    return;
                }
                Err(error) => {
                    warn!("client error: connection died: {:?}", error);
                    return;
                }
            }

            let _ = event_tx.send(Event::ClientConnectionLoss { client_id, conn_id });
        });
    }
}

#[cfg(feature = "tokio-graceful-shutdown")]
impl IntoSubsystem<Error> for NetworkListener {
    async fn run(self, subsys: &mut SubsystemHandle) -> Result<()> {
        if let Ok(result) = self.listen().cancel_on_shutdown(subsys).await {
            result?
        }

        Ok(())
    }
}

async fn read_from_stream<S: AsyncRead + Unpin + Send>(stream: &mut S) -> Result<ClientMessage> {
    let message_type = stream.read_u8().await?;
    let payload_length = stream.read_u32_le().await?;

    if payload_length == 0 {
        return Ok(Message::new(message_type, vec![]).try_into()?);
    }

    let mut message = vec![0; payload_length as usize];
    stream.read_exact(&mut message).await?;

    Ok(Message::new(message_type, message).try_into()?)
}

async fn write_to_stream<S: AsyncWrite + Unpin + Send>(
    stream: &mut S,
    message: impl Into<Message>,
) -> Result<()> {
    let message: Message = message.into();
    stream.write_all(&message.into_bytes()).await?;
    stream.flush().await?;

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
/// message channels, and custom request handlers.
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
#[derive(Debug, Default)]
pub struct NetworkListenerBuilder {
    address: Option<String>,
    cert_path: Option<PathBuf>,
    key_path: Option<PathBuf>,
    clients: Option<Arc<RwLock<ClientRegistry>>>,
    engine_tx: Option<mpsc::Sender<(EngineRequest, oneshot::Sender<ServerMessage>)>>,
    event_tx: Option<broadcast::Sender<Event>>,
    custom_req_tx: Option<mpsc::Sender<CustomRequestMessage>>,
}

impl NetworkListenerBuilder {
    pub fn new() -> Self {
        Self {
            address: None,
            cert_path: None,
            key_path: None,
            clients: None,
            engine_tx: None,
            event_tx: None,
            custom_req_tx: None,
        }
    }

    pub fn custom_req_tx(
        self,
        custom_req_tx: mpsc::Sender<CustomRequestMessage>,
    ) -> NetworkListenerBuilder {
        NetworkListenerBuilder {
            clients: self.clients,
            address: self.address,
            cert_path: self.cert_path,
            key_path: self.key_path,
            engine_tx: self.engine_tx,
            event_tx: self.event_tx,
            custom_req_tx: Some(custom_req_tx),
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

    /// Builds the [`NetworkListener`] from the provided configuration.
    ///
    /// Returns an error if required fields are missing, or TLS setup fails.
    pub fn build(self) -> BuilderResult<NetworkListener> {
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
        })
    }
}

/// Handles an authenticated client connection.
///
/// Reads client messages from the stream, dispatches them to the appropriate
/// handlers, and sends back server responses.
async fn handle_connection<R, W>(
    reader: &mut R,
    writer: &mut W,
    client_id: &str,
    engine_tx: mpsc::Sender<(EngineRequest, oneshot::Sender<ServerMessage>)>,
    custom_req_tx: Option<mpsc::Sender<CustomRequestMessage>>,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    loop {
        let message = match timeout(Duration::from_secs(30), read_from_stream(reader)).await {
            Ok(Ok(message)) => message,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Ok(()),
        };

        let engine_request = match message {
            ClientMessage::Bloop { nfc_uid } => EngineRequest::Bloop {
                nfc_uid,
                client_id: client_id.to_string(),
            },
            ClientMessage::RetrieveAudio { achievement_id } => {
                EngineRequest::RetrieveAudio { id: achievement_id }
            }
            ClientMessage::PreloadCheck {
                audio_manifest_hash,
            } => EngineRequest::PreloadCheck {
                manifest_hash: audio_manifest_hash,
            },
            ClientMessage::Ping => {
                write_to_stream(writer, ServerMessage::Pong).await?;
                continue;
            }
            ClientMessage::Quit => break,
            ClientMessage::Unknown(message) => {
                if let Some(sender) = custom_req_tx.as_ref() {
                    let (resp_tx, resp_rx) = oneshot::channel();

                    let _ = sender
                        .send(CustomRequestMessage {
                            client_id: client_id.to_string(),
                            message,
                            response: resp_tx,
                        })
                        .await;

                    if let Some(message) = resp_rx.await? {
                        write_to_stream(writer, message).await?;
                    }
                }

                continue;
            }
            message => return Err(Error::UnexpectedMessage(message)),
        };

        let (resp_tx, resp_rx) = oneshot::channel();
        let _ = engine_tx.send((engine_request, resp_tx)).await;
        let response = resp_rx.await?;

        write_to_stream(writer, response).await?;
    }

    Ok(())
}

/// Authenticates a client by performing handshake and credential verification.
///
/// Returns the client ID, its IP address, and the negotiated protocol version
/// on success.
async fn authenticate<R, W>(
    reader: &mut R,
    writer: &mut W,
    clients: Arc<RwLock<ClientRegistry>>,
) -> Result<(String, IpAddr, u8)>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let (min_version, max_version) = match read_from_stream(reader).await? {
        ClientMessage::ClientHandshake {
            min_version,
            max_version,
        } => (min_version, max_version),
        message => return Err(Error::UnexpectedMessage(message)),
    };

    if min_version > 3 || max_version < 3 {
        return Err(Error::UnsupportedVersion(min_version, max_version));
    }

    write_to_stream(
        writer,
        ServerMessage::ServerHandshake {
            accepted_version: 3,
            capabilities: Capabilities::PreloadCheck,
        },
    )
    .await?;

    let (client_id, client_secret, ip_addr) = match read_from_stream(reader).await? {
        ClientMessage::Authentication {
            client_id,
            client_secret,
            ip_addr,
        } => (client_id, client_secret, ip_addr),
        message => return Err(Error::UnexpectedMessage(message)),
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

    write_to_stream(writer, ServerMessage::AuthenticationAccepted).await?;

    Ok((client_id.to_string(), ip_addr, 3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

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

        let server_handshake: Message = ServerMessage::ServerHandshake {
            accepted_version: 3,
            capabilities: Capabilities::PreloadCheck,
        }
        .into();

        let mut reader = tokio_test::io::Builder::new()
            .read(&client_handshake)
            .read(&authentication)
            .build();
        let mut writer = tokio_test::io::Builder::new()
            .write(&server_handshake.into_bytes())
            .build();

        let result = authenticate(&mut reader, &mut writer, clients).await;

        assert!(matches!(result, Err(Error::InvalidCredentials)));
    }

    #[tokio::test]
    async fn authentication_succeeds_with_correct_credentials() {
        let clients = Arc::new(RwLock::new(HashMap::default()));
        clients.write().await.insert(
            "client".into(),
            PasswordHashString::new(
                "$argon2id$v=19$m=10,t=1,p=1$THh0RHE5YWNkQUZNa2lqUA$dmB4X7J49jjCGA",
            )
            .unwrap(),
        );

        let client_handshake = build_handshake(3, 3);
        let authentication = build_authentication("client", "secret", "127.0.0.1");

        let server_handshake: Message = ServerMessage::ServerHandshake {
            accepted_version: 3,
            capabilities: Capabilities::PreloadCheck,
        }
        .into();

        let authentication_accepted: Message = ServerMessage::AuthenticationAccepted.into();

        let mut reader = tokio_test::io::Builder::new()
            .read(&client_handshake)
            .read(&authentication)
            .build();
        let mut writer = tokio_test::io::Builder::new()
            .write(&server_handshake.into_bytes())
            .write(&authentication_accepted.into_bytes())
            .build();

        let result = authenticate(&mut reader, &mut writer, clients).await;
        println!("{:?}", result);

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn authentication_fails_with_wrong_password() {
        let clients = Arc::new(RwLock::new(HashMap::default()));
        clients.write().await.insert(
            "client".into(),
            PasswordHashString::new(
                "$argon2id$v=19$m=10,t=1,p=1$THh0RHE5YWNkQUZNa2lqUA$dmB4X7J49jjCGA",
            )
            .unwrap(),
        );

        let client_handshake = build_handshake(3, 3);
        let authentication = build_authentication("client1", "wrong-secret", "127.0.0.1");

        let server_handshake: Message = ServerMessage::ServerHandshake {
            accepted_version: 3,
            capabilities: Capabilities::PreloadCheck,
        }
        .into();

        let mut reader = tokio_test::io::Builder::new()
            .read(&client_handshake)
            .read(&authentication)
            .build();
        let mut writer = tokio_test::io::Builder::new()
            .write(&server_handshake.into_bytes())
            .build();

        let result = authenticate(&mut reader, &mut writer, clients).await;

        assert!(matches!(result, Err(Error::InvalidCredentials)));
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
