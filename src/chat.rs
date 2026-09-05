//! Port for Discord I/O.

use async_trait::async_trait;
use serenity::http::Http;
use serenity::model::id::ChannelId;
use std::sync::Arc;
use std::time::Duration;

/// Whether a failed send can be retried.
///
/// The startup gate and the worker both classify failures the same way:
/// transient conditions are retried with backoff, permanent ones terminate
/// the process (a bot that cannot talk to its owner must not keep serving).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatErrorKind {
    /// Network-level failure (no HTTP response), HTTP 5xx, or 429 rate limit:
    /// retrying may succeed.
    Transient,
    /// Definite refusal (4xx other than 429) or invariant violation:
    /// retrying is pointless.
    Permanent,
}

#[derive(Debug)]
pub struct ChatError {
    pub kind: ChatErrorKind,
    pub detail: String,
}

/// Exponential backoff schedule shared by the startup greeting gate
/// (`main.rs`) and the worker's fail-closed send path (`worker.rs`).
pub const RETRY_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
pub const RETRY_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Next backoff delay (exponential, capped at `RETRY_BACKOFF_MAX`).
pub fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(RETRY_BACKOFF_MAX)
}

/// Classify an HTTP status code into a retry policy.
///
/// 429 (rate limit) and 5xx (server-side) are transient; every other
/// non-success status is a definite refusal that retrying cannot fix.
pub fn classify_status(status: u16) -> ChatErrorKind {
    if status == 429 || (500..=599).contains(&status) {
        ChatErrorKind::Transient
    } else {
        ChatErrorKind::Permanent
    }
}

/// Classify a serenity error into a retry policy.
///
/// - An HTTP response we received (success or error) is judged by status code.
/// - A request that never got a response (connection refused, timeout, DNS) is
///   a network problem that may recover.
/// - Client-side construction errors (bad URL, invalid header, ...) are bugs:
///   retrying is pointless.
pub fn classify_serenity_error(e: &serenity::Error) -> ChatErrorKind {
    match e {
        serenity::Error::Http(http_err) => match http_err {
            serenity::http::HttpError::UnsuccessfulRequest(res) => {
                classify_status(res.status_code.as_u16())
            }
            serenity::http::HttpError::Request(_) => ChatErrorKind::Transient,
            _ => ChatErrorKind::Permanent,
        },
        _ => ChatErrorKind::Permanent,
    }
}

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
    /// Lockdown mode (no whitelist configured): sending is deliberately a
    /// silent no-op, so `None` channel here is by design and not an error.
    locked: bool,
}

impl DiscordChat {
    pub fn new(http: Arc<Http>, locked: bool) -> Self {
        DiscordChat {
            http,
            channel_id: None,
            locked,
        }
    }

    pub fn set_channel(&mut self, id: ChannelId) {
        self.channel_id = Some(id);
    }
}

/// Send `text` to `channel_id`, splitting into Discord-sized chunks.
///
/// Used by the startup greeting gate (before a `DiscordChat` is wired up) and
/// by `DiscordChat::send`. Classifies failures the same way everywhere.
pub async fn send_to_channel(
    http: Arc<Http>,
    channel_id: ChannelId,
    text: &str,
) -> Result<(), ChatError> {
    for chunk in crate::format::split_message(text, crate::format::discord_max()) {
        channel_id
            .say(&http, chunk)
            .await
            .map_err(|e| ChatError {
                kind: classify_serenity_error(&e),
                detail: e.to_string(),
            })?;
    }
    Ok(())
}

#[async_trait]
impl Chat for DiscordChat {
    async fn send(&self, text: String) -> Result<(), ChatError> {
        let Some(channel_id) = self.channel_id else {
            if self.locked {
                // Lockdown mode: no owner channel by design, nothing to send.
                return Ok(());
            }
            // Normal mode without a channel: the startup greeting gate must
            // have failed or the channel was lost. Retrying is pointless.
            return Err(ChatError {
                kind: ChatErrorKind::Permanent,
                detail: "no owner channel configured (startup greeting gate failed)".into(),
            });
        };
        send_to_channel(self.http.clone(), channel_id, &text).await
    }

    async fn send_typing(&self) {
        if let Some(channel_id) = self.channel_id {
            let _ = channel_id.broadcast_typing(&self.http).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_status_transient() {
        assert_eq!(classify_status(429), ChatErrorKind::Transient);
        for status in [500, 502, 503, 504] {
            assert_eq!(classify_status(status), ChatErrorKind::Transient, "{status}");
        }
    }

    #[test]
    fn classify_status_permanent() {
        for status in [400, 401, 403, 404, 422] {
            assert_eq!(classify_status(status), ChatErrorKind::Permanent, "{status}");
        }
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(next_backoff(Duration::from_secs(1)), Duration::from_secs(2));
        assert_eq!(next_backoff(Duration::from_secs(16)), Duration::from_secs(30));
        assert_eq!(next_backoff(Duration::from_secs(30)), Duration::from_secs(30));
    }

    #[tokio::test]
    async fn send_is_silent_noop_in_lockdown_without_channel() {
        let http = Arc::new(Http::new("dummy-token"));
        let chat = DiscordChat::new(http, true);
        assert!(chat.send("hello".into()).await.is_ok());
    }

    #[tokio::test]
    async fn send_is_permanent_error_without_channel_in_normal_mode() {
        let http = Arc::new(Http::new("dummy-token"));
        let chat = DiscordChat::new(http, false);
        let err = chat.send("hello".into()).await.unwrap_err();
        assert_eq!(err.kind, ChatErrorKind::Permanent);
    }
}
