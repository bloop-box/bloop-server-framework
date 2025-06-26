use crate::achievement::AchievementAwardBatch;
use crate::bloop::ProcessedBloop;
use std::net::IpAddr;

#[derive(Clone)]
pub enum Event {
    ClientConnect {
        client_id: String,
        con_id: usize,
        local_ip: IpAddr,
    },
    ClientDisconnect {
        client_id: String,
        con_id: usize,
    },
    ClientConnectionLoss {
        client_id: String,
        con_id: usize,
    },
    BloopProcessed(ProcessedBloop),
    AchievementsAwarded(AchievementAwardBatch),
}
