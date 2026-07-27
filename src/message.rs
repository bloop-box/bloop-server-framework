//! Protocol messages, re-exported from [`bloop_protocol`].
//!
//! The wire format lives in the `bloop-protocol` crate since framework 2.0;
//! this module keeps the 1.x import paths working for the types the
//! framework's public API uses. Custom protocol extensions define their own
//! messages with the `bloop_protocol` derives and plug them into the
//! [`NetworkListener`](crate::network::NetworkListener) type parameters.

pub use bloop_protocol::capabilities::Capabilities;
pub use bloop_protocol::data_hash::DataHash;
pub use bloop_protocol::frame::RawMessage;
pub use bloop_protocol::message::{
    AchievementRecord, AudioData, Authentication, AuthenticationAccepted, Bloop, BloopAccepted,
    ClientHandshake, ClientMessage, ErrorResponse, Ping, Pong, PreloadCheck, PreloadMatch,
    PreloadMismatch, Quit, RetrieveAudio, ServerHandshake, ServerMessage,
};
