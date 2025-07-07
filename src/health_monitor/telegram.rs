use crate::health_monitor::{HealthReport, HealthReportSender};
use async_trait::async_trait;
use teloxide::types::{ChatId, MessageId};
use teloxide::{Bot, RequestError, prelude::*};
use tracing::error;

/// A [`HealthReportSender`] implementation that sends health reports via Telegram.
///
/// This sender uses the [`teloxide`] library to send a message containing the
/// health report to a specific chat. It also attempts to delete the previously sent
/// message to avoid cluttering the chat.
///
/// Note: This implementation assumes only one message is being managed at a time.
/// Deleting the previous message is done by subtracting 1 from the current message
/// ID, which is a simple but not fully robust approach.
///
/// # Examples
///
/// ```
/// use teloxide::types::ChatId;
/// use bloop_server_framework::health_monitor::{
///     telegram::TelegramReportHealthSender
/// };
///
/// let sender = TelegramReportHealthSender::new(
///     "your-bot-token",
///     ChatId(123456)
/// );
/// ```
#[derive(Debug)]
pub struct TelegramReportHealthSender {
    bot: Bot,
    chat_id: ChatId,
}

impl TelegramReportHealthSender {
    /// Creates a new [`TelegramReportHealthSender`].
    pub fn new(token: impl Into<String>, chat_id: ChatId) -> Self {
        Self {
            bot: Bot::new(token),
            chat_id,
        }
    }
}

#[async_trait]
impl HealthReportSender for TelegramReportHealthSender {
    type Error = RequestError;

    async fn send(&mut self, report: &HealthReport, silent: bool) -> Result<(), Self::Error> {
        let mut send_message = self.bot.send_message(self.chat_id, report.as_text_report());

        if silent {
            send_message = send_message.disable_notification(true);
        }

        let message = send_message.send().await?;

        if let Err(err) = self
            .bot
            .delete_message(self.chat_id, MessageId(message.id.0 - 1))
            .await
        {
            error!("Error deleting previous telegram message: {:?}", err);
        };

        Ok(())
    }
}
