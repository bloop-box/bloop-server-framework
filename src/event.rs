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

    /// The connection to a client ended abnormally.
    ///
    /// Unlike [`ClientDisconnect`], this event reflects any termination other
    /// than a clean quit or idle timeout: a network failure, a protocol
    /// violation, or a server-side error while handling a request.
    ClientConnectionLoss { client_id: String, conn_id: usize },

    /// The system has processed a bloop.
    BloopProcessed(ProcessedBloop),

    /// One or more achievements were awarded to players.
    AchievementsAwarded(AchievementAwardBatch),
}
