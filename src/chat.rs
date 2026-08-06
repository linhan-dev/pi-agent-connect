//! Port for Discord I/O.

use async_trait::async_trait;
use serenity::http::Http;
use serenity::model::id::ChannelId;
use std::sync::Arc;

#[derive(Debug)]
pub struct ChatError(pub String);

#[async_trait]
pub trait Chat: Send + Sync {
    /// Send a full message (system messages already carry the `[pi-agent-connect]` prefix;
    /// pi replies are forwarded raw).
    async fn send(&self, text: String) -> Result<(), ChatError>;
    async fn send_typing(&self);
}

/// Real implementation backed by serenity. Talks to a single DM channel.
pub struct DiscordChat {
    http: Arc<Http>,
    channel_id: Option<ChannelId>,
}

impl DiscordChat {
    pub fn new(http: Arc<Http>) -> Self {
        DiscordChat { http, channel_id: None }
    }

    pub fn set_channel(&mut self, id: ChannelId) {
        self.channel_id = Some(id);
    }
}

#[async_trait]
impl Chat for DiscordChat {
    async fn send(&self, text: String) -> Result<(), ChatError> {
        let Some(channel_id) = self.channel_id else {
            // No owner channel (e.g. lockdown mode): nothing to send.
            return Ok(());
        };
        for chunk in crate::format::split_message(&text, crate::format::discord_max()) {
            channel_id
                .say(&self.http, chunk)
                .await
                .map_err(|e| ChatError(e.to_string()))?;
        }
        Ok(())
    }

    async fn send_typing(&self) {
        if let Some(channel_id) = self.channel_id {
            let _ = channel_id.broadcast_typing(&self.http).await;
        }
    }
}
