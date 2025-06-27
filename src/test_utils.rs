use chrono::DateTime;
use std::sync::OnceLock;
use tokio::time::Instant;

pub struct Utc;

static BASE_INSTANT: OnceLock<Instant> = OnceLock::new();
static BASE_UTC: OnceLock<DateTime<chrono::Utc>> = OnceLock::new();

impl Utc {
    pub fn now() -> DateTime<chrono::Utc> {
        let base_instant = *BASE_INSTANT.get_or_init(|| Instant::now());
        let base_utc = *BASE_UTC.get_or_init(|| DateTime::from_timestamp(0, 0).unwrap());

        let elapsed = Instant::now()
            .checked_duration_since(base_instant)
            .unwrap_or_else(|| std::time::Duration::ZERO);

        base_utc + chrono::Duration::from_std(elapsed).unwrap()
    }
}
