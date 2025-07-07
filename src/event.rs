use crate::achievement::AchievementAwardBatch;
use crate::bloop::ProcessedBloop;
use std::net::IpAddr;

/// Represents a significant event within the system.
///
/// Events capture changes in client connections or system actions such as
/// processing bloops or awarding achievements. They can be used for logging,
/// auditing, or triggering additional behavior.
#[derive(Debug, Clone)]
pub enum Event {
    /// A client successfully connected to the system.
    ClientConnect {
        client_id: String,
        conn_id: usize,
        local_ip: IpAddr,
    },

    /// A client has disconnected normally.
    ///
    /// Emitted when the client intentionally disconnects from the system.
    ClientDisconnect { client_id: String, conn_id: usize },

    /// The connection to a client was unexpectedly lost.
    ///
    /// Unlike [`ClientDisconnect`], this event reflects an abrupt loss of
    /// connection, such as a network failure or crash.
    ClientConnectionLoss { client_id: String, conn_id: usize },

    /// The system has processed a bloop.
    BloopProcessed(ProcessedBloop),

    /// One or more achievements were awarded to players.
    AchievementsAwarded(AchievementAwardBatch),
}
