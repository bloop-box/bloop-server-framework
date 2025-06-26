use crate::engine::EngineRequest;
use crate::event::Event;
use crate::nfc_uid::NfcUid;
use argon2::{Argon2, PasswordVerifier, password_hash::PasswordHashString};
use async_trait::async_trait;
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
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{RwLock, broadcast, mpsc, oneshot};
use tokio::time::timeout;
use tokio::{io, pin};
#[cfg(feature = "tokio-graceful-shutdown")]
use tokio_graceful_shutdown::{FutureExt, IntoSubsystem, SubsystemHandle};
use tokio_io_timeout::TimeoutStream;
use tokio_rustls::TlsAcceptor;
use tracing::{info, warn};
use uuid::Uuid;

/// A list of clients mapping client ID to secret hash.
///
/// The hash must be a valid argon2id hash.
pub type ClientRegistry = HashMap<String, PasswordHashString>;

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Oneshot(#[from] oneshot::error::RecvError),

    #[error("request failed to parse: {0}")]
    MalformedRequest(String),

    #[error("client sent unexpected message")]
    UnexpectedMessage,

    #[error("client requested an unsupported version: {0}")]
    UnsupportedVersion(u8),

    #[error("unrecognized request code: {0}")]
    UnknownRequest(u8),

    #[error("client provided invalid credentials")]
    InvalidCredentials,
}

pub type Result<T> = result::Result<T, Error>;

#[allow(dead_code)]
pub struct CustomRequestMessage<T> {
    client_id: String,
    request: T,
    response: oneshot::Sender<Box<dyn WriteToStream>>,
}

pub struct NetworkListener<CustomRequest>
where
    CustomRequest: ReadFromStream + Send + 'static,
{
    clients: Arc<RwLock<ClientRegistry>>,
    addr: SocketAddr,
    tls_acceptor: TlsAcceptor,
    engine_tx: mpsc::Sender<(EngineRequest, oneshot::Sender<StandardResponse>)>,
    event_tx: broadcast::Sender<Event>,
    custom_req_tx: Option<mpsc::Sender<CustomRequestMessage<CustomRequest>>>,
}

impl<CustomRequest> NetworkListener<CustomRequest>
where
    CustomRequest: ReadFromStream + Send + 'static,
{
    pub async fn listen(&self) -> Result<()> {
        let listener = TcpListener::bind(self.addr).await?;
        let mut con_counter: usize = 0;

        loop {
            let (stream, peer_addr) = listener.accept().await?;
            let con_id = con_counter;
            con_counter += 1;
            let event_tx = self.event_tx.clone();

            self.handle_stream(stream, peer_addr, self.clients.clone(), con_id, event_tx);
        }
    }

    fn handle_stream(
        &self,
        stream: TcpStream,
        peer_addr: SocketAddr,
        clients: Arc<RwLock<ClientRegistry>>,
        con_id: usize,
        event_tx: broadcast::Sender<Event>,
    ) {
        let acceptor = self.tls_acceptor.clone();
        let engine_tx = self.engine_tx.clone();
        let custom_req_tx = self.custom_req_tx.clone();

        tokio::spawn(async move {
            info!("new connection from {}", peer_addr);

            let mut stream = match acceptor.accept(stream).await {
                Ok(stream) => stream,
                Err(error) => {
                    warn!("failed to accept stream: {}", error);
                    return;
                }
            };

            let (client_id, local_ip, _version) =
                match timeout(Duration::from_secs(2), authenticate(&mut stream, clients)).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(Error::MalformedRequest(reason))) => {
                        warn!("client sent malformed request: {}", reason);
                        let _ = StandardResponse::Error(ErrorResponse::MalformedMessage)
                            .send(&mut stream)
                            .await;
                        return;
                    }
                    Ok(Err(Error::UnexpectedMessage)) => {
                        warn!("client sent unexpected message");
                        let _ = StandardResponse::Error(ErrorResponse::UnexpectedMessage)
                            .send(&mut stream)
                            .await;
                        return;
                    }
                    Ok(Err(Error::UnsupportedVersion(version))) => {
                        warn!("client requested unsupported version: {}", version);
                        let _ = StandardResponse::Error(ErrorResponse::UnsupportedVersion)
                            .send(&mut stream)
                            .await;
                        return;
                    }
                    Ok(Err(Error::InvalidCredentials)) => {
                        warn!("client provided invalid credentials");
                        let _ = StandardResponse::Error(ErrorResponse::InvalidCredentials)
                            .send(&mut stream)
                            .await;
                        return;
                    }
                    Ok(Err(_)) => return,
                    Err(_) => {
                        warn!("client authentication timed out");
                        return;
                    }
                };

            let _ = event_tx.send(Event::ClientConnect {
                client_id: client_id.clone(),
                con_id,
                local_ip,
            });

            match handle_connection(&mut stream, client_id.clone(), engine_tx, custom_req_tx).await
            {
                Ok(()) => {
                    let _ = event_tx.send(Event::ClientDisconnect {
                        client_id: client_id.clone(),
                        con_id,
                    });
                    return;
                }
                Err(Error::MalformedRequest(reason)) => {
                    warn!("client sent malformed request: {}", reason);
                    let _ = StandardResponse::Error(ErrorResponse::MalformedMessage)
                        .send(&mut stream)
                        .await;
                }
                Err(Error::UnexpectedMessage) => {
                    warn!("client sent unexpected message");
                    let _ = StandardResponse::Error(ErrorResponse::UnexpectedMessage)
                        .send(&mut stream)
                        .await;
                }
                Err(err) => {
                    warn!("connection failed: {}", err);
                }
            }

            let _ = event_tx.send(Event::ClientConnectionLoss {
                client_id: client_id.clone(),
                con_id,
            });
        });
    }
}

#[cfg(feature = "tokio-graceful-shutdown")]
#[async_trait]
impl<CustomRequest> IntoSubsystem<Error> for NetworkListener<CustomRequest>
where
    CustomRequest: ReadFromStream + Send + 'static,
{
    async fn run(mut self, subsys: SubsystemHandle) -> Result<()> {
        if let Ok(result) = self.listen().cancel_on_shutdown(&subsys).await {
            result?
        }

        Ok(())
    }
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

#[derive(Debug, Default)]
pub struct NetworkListenerBuilder<CustomRequest>
where
    CustomRequest: ReadFromStream + Send + 'static,
{
    address: Option<String>,
    cert_path: Option<PathBuf>,
    key_path: Option<PathBuf>,
    clients: Option<Arc<RwLock<ClientRegistry>>>,
    engine_tx: Option<mpsc::Sender<(EngineRequest, oneshot::Sender<StandardResponse>)>>,
    event_tx: Option<broadcast::Sender<Event>>,
    custom_req_tx: Option<mpsc::Sender<CustomRequestMessage<CustomRequest>>>,
}

impl NetworkListenerBuilder<NoopCustomRequest> {
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

    pub fn custom_req_tx<C>(
        self,
        custom_req_tx: mpsc::Sender<CustomRequestMessage<C>>,
    ) -> NetworkListenerBuilder<C>
    where
        C: ReadFromStream + Send + 'static,
    {
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
}

impl<CustomRequest> NetworkListenerBuilder<CustomRequest>
where
    CustomRequest: ReadFromStream + Send + 'static,
{
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
        tx: mpsc::Sender<(EngineRequest, oneshot::Sender<StandardResponse>)>,
    ) -> Self {
        self.engine_tx = Some(tx);
        self
    }

    pub fn event_tx(mut self, tx: broadcast::Sender<Event>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    pub fn build(self) -> BuilderResult<NetworkListener<CustomRequest>> {
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

async fn handle_connection<S, C>(
    stream: &mut S,
    client_id: String,
    engine_tx: mpsc::Sender<(EngineRequest, oneshot::Sender<StandardResponse>)>,
    custom_req_tx: Option<mpsc::Sender<CustomRequestMessage<C>>>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
    C: ReadFromStream,
{
    let mut stream = TimeoutStream::new(stream);
    stream.set_read_timeout(Some(Duration::from_secs(30)));
    stream.set_write_timeout(Some(Duration::from_secs(30)));
    pin!(stream);

    loop {
        let request = match StandardRequest::from_stream(&mut stream).await {
            Ok(request) => request,
            Err(Error::UnknownRequest(code)) => {
                let request = C::from_stream(code, &mut stream).await?;

                if let Some(sender) = custom_req_tx.as_ref() {
                    let (resp_tx, resp_rx) = oneshot::channel();

                    let _ = sender
                        .send(CustomRequestMessage {
                            client_id: client_id.clone(),
                            request,
                            response: resp_tx,
                        })
                        .await;

                    let response = resp_rx.await?;
                    response.send(&mut stream).await?;
                }

                continue;
            }
            Err(err) => return Err(err),
        };

        let engine_request = match request {
            StandardRequest::Bloop { nfc_uid } => EngineRequest::Bloop {
                nfc_uid,
                client_id: client_id.clone(),
            },
            StandardRequest::PreloadCheck { state_hash } => {
                EngineRequest::PreloadCheck { state_hash }
            }
            StandardRequest::RetrieveAudio { id } => EngineRequest::RetrieveAudio { id },
            StandardRequest::Ping => {
                StandardResponse::Pong.send(&mut stream).await?;
                continue;
            }
            StandardRequest::Quit => break,
            _ => return Err(Error::UnexpectedMessage),
        };

        let (resp_tx, resp_rx) = oneshot::channel();
        let _ = engine_tx.send((engine_request, resp_tx)).await;
        let response = resp_rx.await?;
        response.send(&mut stream).await?;
    }

    Ok(())
}

async fn authenticate<S: AsyncRead + AsyncWrite + Unpin + Send>(
    stream: &mut S,
    clients: Arc<RwLock<ClientRegistry>>,
) -> Result<(String, IpAddr, u8)> {
    StandardResponse::VersionNegotiation { min: 3, max: 3 }
        .send(stream)
        .await?;

    let StandardRequest::VersionNegotiation { version } =
        StandardRequest::from_stream(stream).await?
    else {
        return Err(Error::UnexpectedMessage);
    };

    if version != 3 {
        return Err(Error::UnsupportedVersion(version));
    }

    StandardResponse::VersionAccepted { version }
        .send(stream)
        .await?;

    let StandardRequest::Authentication {
        client_id,
        secret,
        ip_address,
    } = StandardRequest::from_stream(stream).await?
    else {
        return Err(Error::UnexpectedMessage);
    };

    let clients = clients.read().await;
    let Some(secret_hash) = clients.get(&client_id) else {
        return Err(Error::InvalidCredentials);
    };

    if Argon2::default()
        .verify_password(secret.as_bytes(), &secret_hash.password_hash())
        .is_err()
    {
        return Err(Error::InvalidCredentials);
    }

    StandardResponse::AuthenticationAccepted
        .send(stream)
        .await?;
    Ok((client_id.to_string(), ip_address, version))
}

#[async_trait]
pub trait WriteToStream: Send + Sync + Debug + 'static {
    async fn send(self: Box<Self>, stream: &mut (dyn AsyncWrite + Unpin + Send)) -> io::Result<()>;
}

#[async_trait]
pub trait ReadFromStream
where
    Self: Sized,
{
    async fn from_stream<S: AsyncRead + Unpin + Send>(code: u8, stream: &mut S) -> Result<Self>;
}

pub struct NoopCustomRequest;

#[async_trait]
impl ReadFromStream for NoopCustomRequest {
    async fn from_stream<S: AsyncRead + Unpin + Send>(code: u8, _stream: &mut S) -> Result<Self> {
        Err(Error::UnknownRequest(code))
    }
}

#[derive(Debug)]
pub enum StandardRequest {
    VersionNegotiation {
        version: u8,
    },
    Authentication {
        client_id: String,
        secret: String,
        ip_address: IpAddr,
    },
    Bloop {
        nfc_uid: NfcUid,
    },
    RetrieveAudio {
        id: Uuid,
    },
    PreloadCheck {
        state_hash: Option<Vec<u8>>,
    },
    Ping,
    Quit,
    Unknown(u8),
    Malformed,
}

impl StandardRequest {
    async fn from_stream<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Self> {
        let code = stream.read_u8().await?;

        match code {
            0x01 => {
                let version = stream.read_u8().await?;
                Ok(Self::VersionNegotiation { version })
            }
            0x03 => {
                let client_id_length = stream.read_u8().await? as usize;
                let mut client_id = vec![0u8; client_id_length];
                stream.read_exact(&mut client_id).await?;
                let Ok(client_id) = String::from_utf8(client_id) else {
                    return Err(Error::MalformedRequest(
                        "invalid UTF-8 client ID".to_string(),
                    ));
                };

                let secret_length = stream.read_u8().await? as usize;
                let mut secret = vec![0u8; secret_length];
                stream.read_exact(&mut secret).await?;
                let Ok(secret) = String::from_utf8(secret) else {
                    return Err(Error::MalformedRequest(
                        "invalid UTF-8 client secret".to_string(),
                    ));
                };

                let inet_version = stream.read_u8().await?;
                let ip_address = match inet_version {
                    4 => {
                        let mut local_ip = [0; 4];
                        stream.read_exact(&mut local_ip).await?;
                        IpAddr::from(local_ip)
                    }
                    6 => {
                        let mut local_ip = [0; 16];
                        stream.read_exact(&mut local_ip).await?;
                        IpAddr::from(local_ip)
                    }
                    _ => {
                        return Err(Error::MalformedRequest(format!(
                            "unknown INET version: {}",
                            inet_version
                        )));
                    }
                };

                Ok(Self::Authentication {
                    client_id,
                    secret,
                    ip_address,
                })
            }
            0x04 => {
                let mut uid = [0; 7];
                stream.read_exact(&mut uid).await?;

                Ok(Self::Bloop {
                    nfc_uid: NfcUid(uid),
                })
            }
            0x05 => {
                let mut id = [0; 16];
                stream.read_exact(&mut id).await?;

                Ok(Self::RetrieveAudio {
                    id: Uuid::from_bytes(id),
                })
            }
            0x06 => {
                let hash_length = stream.read_u32_le().await? as usize;

                if hash_length == 0 {
                    return Ok(Self::PreloadCheck { state_hash: None });
                }

                let mut hash = vec![0u8; hash_length];
                stream.read_exact(&mut hash).await?;

                Ok(Self::PreloadCheck {
                    state_hash: Some(hash),
                })
            }
            0x07 => Ok(Self::Ping),
            0x08 => Ok(Self::Quit),
            code => Err(Error::UnknownRequest(code)),
        }
    }
}

#[derive(Debug)]
pub struct AchievementResponse {
    pub id: Uuid,
    pub audio_hash: md5::Digest,
}

impl AchievementResponse {
    async fn send<S: AsyncWrite + Unpin + Send>(self, stream: &mut S) -> io::Result<()> {
        stream.write_all(self.id.as_bytes()).await?;
        stream.write_all(self.audio_hash.as_slice()).await?;

        Ok(())
    }
}

#[derive(Debug)]
pub enum ErrorResponse {
    UnexpectedMessage,
    MalformedMessage,
    UnsupportedVersion,
    InvalidCredentials,
    UnknownNfcUid,
    NfcUidThrottled,
    UnknownAchievementId,
    Custom(u8),
}

impl ErrorResponse {
    async fn send<S: AsyncWrite + Unpin + Send>(self, stream: &mut S) -> io::Result<()> {
        match self {
            Self::UnexpectedMessage => stream.write_u8(0).await?,
            Self::MalformedMessage => stream.write_u8(1).await?,
            Self::UnsupportedVersion => stream.write_u8(2).await?,
            Self::InvalidCredentials => stream.write_u8(3).await?,
            Self::UnknownNfcUid => stream.write_u8(4).await?,
            Self::NfcUidThrottled => stream.write_u8(5).await?,
            Self::UnknownAchievementId => stream.write_u8(6).await?,
            Self::Custom(code) => stream.write_u8(code).await?,
        }

        Ok(())
    }
}

#[derive(Debug)]
pub enum StandardResponse {
    Error(ErrorResponse),
    VersionNegotiation {
        min: u8,
        max: u8,
    },
    VersionAccepted {
        version: u8,
    },
    AuthenticationAccepted,
    BloopRecorded {
        achievements: Vec<AchievementResponse>,
    },
    Audio {
        data: Vec<u8>,
    },
    PreloadStateMatch,
    PreloadStateMismatch {
        state_hash: Vec<u8>,
        achievements: Vec<AchievementResponse>,
    },
    Pong,
    Custom(Box<dyn WriteToStream>),
}

impl StandardResponse {
    pub fn new_custom<T: WriteToStream>(response: T) -> StandardResponse {
        StandardResponse::Custom(Box::new(response))
    }

    async fn send<S: AsyncWrite + Unpin + Send>(self, stream: &mut S) -> io::Result<()> {
        match self {
            Self::Error(error) => {
                stream.write_u8(0x00).await?;
                error.send(stream).await?
            }
            Self::VersionNegotiation { min, max } => {
                stream.write_u8(0x01).await?;
                stream.write_u8(min).await?;
                stream.write_u8(max).await?;
            }
            Self::VersionAccepted { version } => {
                stream.write_u8(0x02).await?;
                stream.write_u8(version).await?;
            }
            Self::AuthenticationAccepted => {
                stream.write_u8(0x03).await?;
            }
            Self::BloopRecorded { achievements } => {
                stream.write_u8(0x04).await?;
                stream.write_u8(achievements.len() as u8).await?;

                for achievement in achievements {
                    achievement.send(stream).await?;
                }
            }
            Self::Audio { data } => {
                stream.write_u8(0x05).await?;
                stream.write_u32_le(data.len() as u32).await?;
                stream.write_all(&data).await?;
            }
            Self::PreloadStateMatch => stream.write_u8(0x06).await?,
            Self::PreloadStateMismatch {
                state_hash,
                achievements,
            } => {
                stream.write_u8(0x07).await?;
                stream.write_u32_le(state_hash.len() as u32).await?;
                stream.write_all(&state_hash).await?;

                stream.write_u32_le(achievements.len() as u32).await?;

                for achievement in achievements {
                    achievement.send(stream).await?;
                }
            }
            Self::Pong => {
                stream.write_u8(0x08).await?;
            }
            Self::Custom(response) => {
                response.send(stream).await?;
            }
        }

        stream.flush().await?;
        Ok(())
    }
}
