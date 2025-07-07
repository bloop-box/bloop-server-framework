//! Module handling the protocol messages exchanged between client and server.
//!
//! It defines message structures, serialization/deserialization logic, and
//! error handling used in the communication protocol.
//!
//! This module supports parsing raw messages into typed enums for client and
//! server messages, managing data hashes, server feature flags, and achievement
//! records.

use crate::nfc_uid::NfcUid;
use bitmask_enum::bitmask;
use byteorder::ReadBytesExt;
use md5::Digest;
use std::io::{self, Cursor, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use uuid::Uuid;

/// Represents a raw protocol message with a type byte and a payload.
#[derive(Debug, Clone)]
pub struct Message {
    /// Message type identifier (opcode).
    message_type: u8,
    /// Payload bytes associated with this message.
    payload: Vec<u8>,
}

impl Message {
    /// Constructs a new [`Message`] with the specified type and payload.
    pub fn new(message_type: u8, payload: Vec<u8>) -> Self {
        Self {
            message_type,
            payload,
        }
    }

    /// Serializes the [`Message`] into bytes:
    ///
    /// - 1 byte for message_type
    /// - 4 bytes little-endian length of payload
    /// - payload bytes
    pub fn into_bytes(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(1 + 4 + self.payload.len());
        bytes.push(self.message_type);
        bytes.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        bytes.extend(self.payload);

        bytes
    }
}

/// Wrapper around an MD5 `Digest` representing a data hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataHash(Digest);

impl From<Digest> for DataHash {
    fn from(value: Digest) -> Self {
        Self(value)
    }
}

impl DataHash {
    /// Returns the hash bytes as a slice.
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_ref()
    }

    /// Converts the [`DataHash`] into a 17-byte array tagged with
    /// length prefix (16).
    fn into_tagged_bytes(self) -> [u8; 17] {
        let mut bytes = [0u8; 17];
        bytes[0] = 16;
        bytes[1..].copy_from_slice(self.0.as_slice());

        bytes
    }

    /// Attempts to read an optional [`DataHash`] from a byte cursor.
    fn from_cursor_opt(cursor: &mut Cursor<Vec<u8>>) -> Result<Option<Self>, io::Error> {
        let length = cursor.read_u8()?;

        if length == 0 {
            return Ok(None);
        }

        if length != 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid data hash length: {length}"),
            ));
        }

        let mut bytes = [0u8; 16];
        cursor.read_exact(&mut bytes)?;

        Ok(Some(Self(Digest(bytes))))
    }
}

/// Bitmask enum representing features supported by the server.
#[bitmask(u64)]
#[bitmask_config(vec_debug)]
pub enum ServerFeatures {
    /// Indicates support for preload checks.
    PreloadCheck = 0x1,
}

/// Enum of messages sent from the client to the server.
#[derive(Debug)]
pub enum ClientMessage {
    /// Handshake message specifying supported protocol version range.
    ClientHandshake { min_version: u8, max_version: u8 },

    /// Authentication message including client ID, secret, and IP address.
    Authentication {
        client_id: String,
        client_secret: String,
        ip_addr: IpAddr,
    },

    /// Ping message (keep-alive).
    Ping,

    /// Quit message (disconnect).
    Quit,

    /// "Bloop" message containing an NFC UID.
    Bloop { nfc_uid: NfcUid },

    /// Request to retrieve audio data associated with an achievement ID.
    RetrieveAudio { achievement_id: Uuid },

    /// Preload check optionally including a hash of the audio manifest.
    PreloadCheck {
        audio_manifest_hash: Option<DataHash>,
    },

    /// Unknown or unsupported message variant, carrying raw message data.
    Unknown(Message),
}

impl TryFrom<Message> for ClientMessage {
    type Error = io::Error;

    /// Tries to parse a raw [`Message`] into a typed [`ClientMessage`] variant.
    ///
    /// Returns an error if the message payload is malformed or incomplete.
    fn try_from(message: Message) -> Result<Self, Self::Error> {
        let mut cursor = Cursor::new(message.payload);

        match message.message_type {
            0x01 => {
                let min_version = cursor.read_u8()?;
                let max_version = cursor.read_u8()?;

                Ok(Self::ClientHandshake {
                    min_version,
                    max_version,
                })
            }
            0x03 => {
                let client_id = read_string(&mut cursor)?;
                let client_secret = read_string(&mut cursor)?;
                let ip_addr = read_ip_addr(&mut cursor)?;

                Ok(Self::Authentication {
                    client_id,
                    client_secret,
                    ip_addr,
                })
            }
            0x05 => Ok(Self::Ping),
            0x07 => Ok(Self::Quit),
            0x08 => {
                let length = cursor.read_u8()? as usize;
                let mut buffer = vec![0u8; length];
                cursor.read_exact(&mut buffer)?;

                let nfc_uid = NfcUid::try_from(buffer.as_slice()).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "Invalid NFC UID length or data")
                })?;

                Ok(Self::Bloop { nfc_uid })
            }
            0x0a => {
                let mut uuid = [0u8; 16];
                cursor.read_exact(&mut uuid)?;

                Ok(Self::RetrieveAudio {
                    achievement_id: Uuid::from_bytes(uuid),
                })
            }
            0x0c => {
                let hash = DataHash::from_cursor_opt(&mut cursor)?;

                Ok(Self::PreloadCheck {
                    audio_manifest_hash: hash,
                })
            }
            code => Ok(Self::Unknown(Message::new(code, cursor.into_inner()))),
        }
    }
}

/// Reads a length-prefixed UTF-8 string from the cursor.
///
/// Returns an IO error if the string is invalid UTF-8 or reading fails.
fn read_string(cursor: &mut Cursor<Vec<u8>>) -> Result<String, io::Error> {
    let length = cursor.read_u8()? as usize;
    let mut buffer = vec![0; length];
    cursor.read_exact(&mut buffer)?;

    String::from_utf8(buffer)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8 string"))
}

/// Reads an IP address from the cursor.
///
/// Format: 1 byte kind (4 for IPv4, 6 for IPv6), followed by bytes of the
/// address. Returns an IO error if the kind is invalid or reading fails.
fn read_ip_addr(cursor: &mut Cursor<Vec<u8>>) -> Result<IpAddr, io::Error> {
    let kind = cursor.read_u8()?;

    match kind {
        4 => {
            let mut bytes = [0u8; 4];
            cursor.read_exact(&mut bytes)?;
            Ok(IpAddr::V4(Ipv4Addr::from(bytes)))
        }
        6 => {
            let mut bytes = [0u8; 16];
            cursor.read_exact(&mut bytes)?;
            Ok(IpAddr::V6(Ipv6Addr::from(bytes)))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid IP address type: {kind}"),
        )),
    }
}

/// Record representing an achievement, including its UUID and optional audio
/// file hash.
#[derive(Debug)]
pub struct AchievementRecord {
    /// UUID of the achievement.
    pub id: Uuid,
    /// Optional hash of the associated audio file.
    pub audio_file_hash: Option<DataHash>,
}

impl AchievementRecord {
    /// Serializes the `AchievementRecord` into bytes:
    ///
    /// - 16 bytes for UUID
    /// - 17 bytes tagged DataHash if present, or a zero byte if absent.
    fn into_bytes(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(16 + 17);
        bytes.extend_from_slice(&self.id.into_bytes());

        match self.audio_file_hash {
            Some(hash) => bytes.extend_from_slice(&hash.into_tagged_bytes()),
            None => bytes.push(0),
        }

        bytes
    }
}

/// Enum representing error responses from the server.
#[derive(Debug)]
pub enum ErrorResponse {
    UnexpectedMessage,
    MalformedMessage,
    UnsupportedVersionRange,
    InvalidCredentials,
    UnknownNfcUid,
    NfcUidThrottled,
    AudioUnavailable,
    /// Custom error code with arbitrary value.
    Custom(u8),
}

impl From<ErrorResponse> for u8 {
    fn from(error: ErrorResponse) -> u8 {
        error.into_error_code()
    }
}

impl ErrorResponse {
    /// Converts the `ErrorResponse` into the corresponding numeric error code.
    fn into_error_code(self) -> u8 {
        match self {
            Self::UnexpectedMessage => 0,
            Self::MalformedMessage => 1,
            Self::UnsupportedVersionRange => 2,
            Self::InvalidCredentials => 3,
            Self::UnknownNfcUid => 4,
            Self::NfcUidThrottled => 5,
            Self::AudioUnavailable => 6,
            Self::Custom(code) => code,
        }
    }
}

/// Enum of messages sent from the server to the client.
#[derive(Debug)]
pub enum ServerMessage {
    /// Error response message.
    Error(ErrorResponse),

    /// Server handshake response with accepted protocol version and features.
    ServerHandshake {
        accepted_version: u8,
        features: ServerFeatures,
    },

    /// Authentication was accepted.
    AuthenticationAccepted,

    /// Pong response to client's ping.
    Pong,

    /// Response to a Bloop message containing achievement records.
    BloopAccepted {
        achievements: Vec<AchievementRecord>,
    },

    /// Audio data response carrying raw bytes.
    AudioData { data: Vec<u8> },

    /// Indicates preload data matched on the server.
    PreloadMatch,

    /// Indicates preload data mismatched, includes hash and achievements.
    PreloadMismatch {
        audio_manifest_hash: DataHash,
        achievements: Vec<AchievementRecord>,
    },

    /// Custom server message.
    Custom(Message),
}

impl From<ServerMessage> for Message {
    /// Converts a `ServerMessage` enum into a raw `Message` suitable for
    /// transmission.
    fn from(server_message: ServerMessage) -> Message {
        match server_message {
            ServerMessage::Error(error) => Message::new(0x00, vec![error.into_error_code()]),
            ServerMessage::ServerHandshake {
                accepted_version,
                features,
            } => {
                let mut payload = Vec::with_capacity(9);
                payload.push(accepted_version);
                payload.extend_from_slice(&features.bits().to_le_bytes());
                Message::new(0x02, payload)
            }
            ServerMessage::AuthenticationAccepted => Message::new(0x04, vec![]),
            ServerMessage::Pong => Message::new(0x06, vec![]),
            ServerMessage::BloopAccepted { achievements } => {
                let mut payload = Vec::with_capacity(1 + achievements.len() * (16 + 17));
                payload.push(achievements.len() as u8);

                for achievement in achievements {
                    payload.extend(achievement.into_bytes())
                }

                Message::new(0x09, payload)
            }
            ServerMessage::AudioData { data } => {
                let mut payload = Vec::with_capacity(4 + data.len());
                payload.extend_from_slice(&(data.len() as u32).to_le_bytes());
                payload.extend(data);

                Message::new(0x0b, payload)
            }
            ServerMessage::PreloadMatch => Message::new(0x0d, vec![]),
            ServerMessage::PreloadMismatch {
                audio_manifest_hash,
                achievements,
            } => {
                let mut payload = Vec::with_capacity(17 + 1 + achievements.len() * (16 + 17));
                payload.extend_from_slice(&audio_manifest_hash.into_tagged_bytes());
                payload.extend_from_slice(&(achievements.len() as u32).to_le_bytes());

                for achievement in achievements {
                    payload.extend(achievement.into_bytes())
                }

                Message::new(0x0e, payload)
            }
            ServerMessage::Custom(message) => message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;

    fn make_message(msg_type: u8, payload: &[u8]) -> Message {
        Message::new(msg_type, payload.to_vec())
    }

    #[test]
    fn client_handshake_parses_correctly_from_message() {
        let payload = [1u8, 5];
        let msg = make_message(0x01, &payload);
        let client_msg = ClientMessage::try_from(msg).unwrap();

        match client_msg {
            ClientMessage::ClientHandshake {
                min_version,
                max_version,
            } => {
                assert_eq!(min_version, 1);
                assert_eq!(max_version, 5);
            }
            _ => panic!("Expected ClientHandshake variant"),
        }
    }

    #[test]
    fn authentication_parses_correctly_from_message() {
        let mut payload = vec![];
        payload.push(3);
        payload.extend(b"foo");
        payload.push(3);
        payload.extend(b"bar");
        payload.push(4);
        payload.extend(&[127, 0, 0, 1]);

        let msg = make_message(0x03, &payload);
        let client_msg = ClientMessage::try_from(msg).unwrap();

        match client_msg {
            ClientMessage::Authentication {
                client_id,
                client_secret,
                ip_addr,
            } => {
                assert_eq!(client_id, "foo");
                assert_eq!(client_secret, "bar");
                assert_eq!(ip_addr, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
            }
            _ => panic!("Expected Authentication variant"),
        }
    }

    #[test]
    fn ping_message_parses_as_ping_variant() {
        let msg = make_message(0x05, &[]);
        let client_msg = ClientMessage::try_from(msg).unwrap();
        assert!(matches!(client_msg, ClientMessage::Ping));
    }

    #[test]
    fn quit_message_parses_as_quit_variant() {
        let msg = make_message(0x07, &[]);
        let client_msg = ClientMessage::try_from(msg).unwrap();
        assert!(matches!(client_msg, ClientMessage::Quit));
    }

    #[test]
    fn bloop_message_parses_single_nfc_uid_correctly() {
        let payload = [4u8, 1, 2, 3, 4];
        let msg = make_message(0x08, &payload);
        let client_msg = ClientMessage::try_from(msg).unwrap();

        match client_msg {
            ClientMessage::Bloop { nfc_uid } => {
                let expected = NfcUid::try_from(&[1, 2, 3, 4][..]).unwrap();
                assert_eq!(nfc_uid, expected);
            }
            _ => panic!("Expected Bloop variant"),
        }
    }

    #[test]
    fn retrieve_audio_message_parses_uuid_correctly() {
        let uuid = Uuid::new_v4();
        let payload = uuid.as_bytes();
        let msg = make_message(0x0a, payload);
        let client_msg = ClientMessage::try_from(msg).unwrap();

        match client_msg {
            ClientMessage::RetrieveAudio { achievement_id } => {
                assert_eq!(achievement_id, uuid);
            }
            _ => panic!("Expected RetrieveAudio variant"),
        }
    }

    #[test]
    fn preload_check_message_parses_with_some_hash() {
        let mut payload = vec![16];
        payload.extend_from_slice(&[0u8; 16]);
        let msg = make_message(0x0c, &payload);
        let client_msg = ClientMessage::try_from(msg).unwrap();

        match client_msg {
            ClientMessage::PreloadCheck {
                audio_manifest_hash,
            } => {
                assert!(audio_manifest_hash.is_some());
                let hash = audio_manifest_hash.unwrap();
                assert_eq!(hash.0.as_slice(), &[0u8; 16]);
            }
            _ => panic!("Expected PreloadCheck variant"),
        }
    }

    #[test]
    fn preload_check_message_parses_with_none_hash() {
        let payload = [0];
        let msg = make_message(0x0c, &payload);
        let client_msg = ClientMessage::try_from(msg).unwrap();

        match client_msg {
            ClientMessage::PreloadCheck {
                audio_manifest_hash,
            } => {
                assert!(audio_manifest_hash.is_none());
            }
            _ => panic!("Expected PreloadCheck variant"),
        }
    }

    #[test]
    fn unknown_message_parses_correctly_with_payload_preserved() {
        let payload = [1, 2, 3];
        let msg = make_message(0xFF, &payload);
        let client_msg = ClientMessage::try_from(msg).unwrap();

        match client_msg {
            ClientMessage::Unknown(m) => {
                assert_eq!(m.message_type, 0xFF);
                assert_eq!(m.payload, payload);
            }
            _ => panic!("Expected Unknown variant"),
        }
    }

    #[test]
    fn client_handshake_fails_if_payload_too_short() {
        let msg = make_message(0x01, &[1]);
        assert!(ClientMessage::try_from(msg).is_err());
    }

    #[test]
    fn authentication_fails_on_invalid_utf8_client_id() {
        let mut payload = vec![2];
        payload.extend(&[0xff, 0xff]);
        payload.push(3);
        payload.extend(b"bar");
        payload.push(4);
        payload.extend(&[127, 0, 0, 1]);

        let msg = make_message(0x03, &payload);
        assert!(ClientMessage::try_from(msg).is_err());
    }

    #[test]
    fn authentication_fails_on_invalid_utf8_client_secret() {
        let mut payload = vec![3];
        payload.extend(b"foo");
        payload.push(2);
        payload.extend(&[0xff, 0xff]);
        payload.push(4);
        payload.extend(&[127, 0, 0, 1]);

        let msg = make_message(0x03, &payload);
        assert!(ClientMessage::try_from(msg).is_err());
    }

    #[test]
    fn authentication_fails_on_invalid_ip_kind() {
        let mut payload = vec![3];
        payload.extend(b"foo");
        payload.push(3);
        payload.extend(b"bar");
        payload.push(0xff);
        payload.extend(&[1, 2, 3, 4]);

        let msg = make_message(0x03, &payload);
        assert!(ClientMessage::try_from(msg).is_err());
    }

    #[test]
    fn bloop_fails_if_nfc_uid_length_mismatch() {
        let payload = [5u8, 1, 2, 3, 4];
        let msg = make_message(0x08, &payload);
        assert!(ClientMessage::try_from(msg).is_err());
    }

    #[test]
    fn retrieve_audio_fails_if_uuid_too_short() {
        let payload = [0u8; 15];
        let msg = make_message(0x0a, &payload);
        assert!(ClientMessage::try_from(msg).is_err());
    }

    #[test]
    fn preload_check_fails_on_invalid_length() {
        let payload = [1, 0];
        let msg = make_message(0x0c, &payload);
        assert!(ClientMessage::try_from(msg).is_err());
    }

    #[test]
    fn server_message_error_serializes_correctly() {
        let server_msg = ServerMessage::Error(ErrorResponse::InvalidCredentials);
        let message: Message = server_msg.into();
        assert_eq!(message.message_type, 0x00);
        assert_eq!(message.payload, vec![3]);
    }

    #[test]
    fn server_handshake_serializes_correctly() {
        let features = ServerFeatures::none();
        let server_msg = ServerMessage::ServerHandshake {
            accepted_version: 7,
            features,
        };
        let message: Message = server_msg.into();
        assert_eq!(message.message_type, 0x02);
        assert_eq!(message.payload.len(), 9);
        assert_eq!(message.payload[0], 7);
        assert_eq!(&message.payload[1..], &features.bits().to_le_bytes());
    }

    #[test]
    fn authentication_accepted_serializes_to_empty_payload() {
        let server_msg = ServerMessage::AuthenticationAccepted;
        let message: Message = server_msg.into();
        assert_eq!(message.message_type, 0x04);
        assert!(message.payload.is_empty());
    }

    #[test]
    fn pong_serializes_to_empty_payload() {
        let server_msg = ServerMessage::Pong;
        let message: Message = server_msg.into();
        assert_eq!(message.message_type, 0x06);
        assert!(message.payload.is_empty());
    }

    #[test]
    fn bloop_accepted_serializes_with_achievements() {
        let uuid = Uuid::new_v4();
        let record = AchievementRecord {
            id: uuid,
            audio_file_hash: None,
        };

        let server_msg = ServerMessage::BloopAccepted {
            achievements: vec![record],
        };

        let message: Message = server_msg.into();
        assert_eq!(message.message_type, 0x09);
        assert_eq!(message.payload[0], 1);

        assert_eq!(&message.payload[1..17], uuid.as_bytes());
        assert_eq!(message.payload.len(), 1 + 16 + 1);
    }

    #[test]
    fn audio_data_serializes_correctly() {
        let data = vec![1, 2, 3, 4, 5];
        let server_msg = ServerMessage::AudioData { data: data.clone() };
        let message: Message = server_msg.into();

        assert_eq!(message.message_type, 0x0b);
        assert_eq!(&message.payload[4..], &data[..]);

        let length = u32::from_le_bytes(message.payload[0..4].try_into().unwrap());
        assert_eq!(length as usize, data.len());
    }

    #[test]
    fn preload_match_serializes_to_empty_payload() {
        let server_msg = ServerMessage::PreloadMatch;
        let message: Message = server_msg.into();

        assert_eq!(message.message_type, 0x0d);
        assert!(message.payload.is_empty());
    }

    #[test]
    fn preload_mismatch_serializes_with_hash_and_achievements() {
        let hash = DataHash(Digest([1u8; 16]));
        let uuid = Uuid::new_v4();
        let record = AchievementRecord {
            id: uuid,
            audio_file_hash: None,
        };

        let server_msg = ServerMessage::PreloadMismatch {
            audio_manifest_hash: hash,
            achievements: vec![record],
        };

        let message: Message = server_msg.into();
        assert_eq!(message.message_type, 0x0e);

        // Check the first byte is length 16 for DataHash (per into_bytes)
        assert_eq!(message.payload[0], 16);
        assert_eq!(&message.payload[1..17], &hash.0.as_slice()[..]);

        // Number of achievements (4 bytes little-endian)
        let count = u32::from_le_bytes(message.payload[17..21].try_into().unwrap());
        assert_eq!(count, 1);

        // UUID bytes start at 21
        assert_eq!(&message.payload[21..37], uuid.as_bytes());
    }

    #[test]
    fn custom_server_message_passes_through_as_is() {
        let original = Message::new(0xAB, vec![9, 8, 7]);
        let server_msg = ServerMessage::Custom(original.clone());
        let message: Message = server_msg.into();

        assert_eq!(message.message_type, 0xAB);
        assert_eq!(message.payload, vec![9, 8, 7]);
    }
}
