//! pi-agent-connect — minimal Discord DM gateway for the pi coding agent.
//!
//! Foreground single binary. No database, no config file, no daemon.
//! Everything is configured through environment variables.

mod agent;
mod chat;
mod commands;
mod config;
mod format;
mod router;
mod worker;

use agent::{Agent, RealAgent};
use chat::{
    classify_serenity_error, next_backoff, send_to_channel, Chat, ChatError, ChatErrorKind,
    DiscordChat, RETRY_BACKOFF_INITIAL,
};
use router::Decision;
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::model::channel::Message;
use serenity::model::gateway::{GatewayIntents, Ready};
use serenity::model::id::{ChannelId, UserId};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use worker::{Job, Worker};

/// Startup greeting gate: call `attempt` until it succeeds.
///
/// Transient failures (no HTTP response, 429, 5xx) are retried with
/// exponential backoff and logged; a permanent failure (definite 4xx refusal)
/// is returned to the caller, which terminates the process. The bot must not
/// connect the gateway / start serving before the owner has been greeted.
async fn greet_with_retry<F, Fut>(mut attempt: F) -> Result<ChannelId, ChatError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<ChannelId, ChatError>>,
{
    let mut delay = RETRY_BACKOFF_INITIAL;
    let mut attempt_no: u32 = 0;
    loop {
        match attempt().await {
            Ok(channel_id) => return Ok(channel_id),
            Err(e) if e.kind == ChatErrorKind::Transient => {
                attempt_no += 1;
                tracing::warn!(
                    attempt = attempt_no,
                    error = %e.detail,
                    ?delay,
                    "greeting failed (transient), retrying"
                );
                tokio::time::sleep(delay).await;
                delay = next_backoff(delay);
            }
            Err(e) => return Err(e),
        }
    }
}
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

struct Handler {
    tx: mpsc::UnboundedSender<Job>,
    config: Arc<config::Config>,
    bot_id: std::sync::RwLock<Option<String>>,
}

fn to_job(decision: Decision) -> Job {
    match decision {
        Decision::Command(commands::Command::New) => Job::New,
        Decision::Command(commands::Command::Abort) => Job::Abort,
        Decision::Command(commands::Command::Session) => Job::Session,
        Decision::Command(commands::Command::Model(arg)) => Job::Model(arg),
        Decision::Command(commands::Command::Thinking(arg)) => Job::Thinking(arg),
        Decision::Prompt(text) => Job::Prompt(text),
        _ => unreachable!("to_job called with a non-routable decision"),
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        *self.bot_id.write().unwrap() = Some(ready.user.id.to_string());
        tracing::info!(tag = %ready.user.tag(), "bot connected");
    }

    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }
        let bot_id = self.bot_id.read().unwrap().clone().unwrap_or_default();
        let incoming = router::IncomingMessage {
            author_id: msg.author.id.to_string(),
            author_name: msg.author.name.clone(),
            is_bot: msg.author.bot,
            has_attachments: !msg.attachments.is_empty(),
            content: msg.content.clone(),
        };
        let classification = router::classify(
            self.config.allowed_user.as_deref(),
            &bot_id,
            self.config.command_prefix,
            &incoming,
        );
        match classification {
            router::Classification {
                decision: Decision::Drop,
                audit: Some(reason),
            } => {
                let preview: String = incoming.content.chars().take(100).collect();
                tracing::warn!(
                    target: "audit",
                    user_id = %incoming.author_id,
                    user_name = %incoming.author_name,
                    reason = ?reason,
                    preview = %preview,
                    "blocked message"
                );
            }
            router::Classification {
                decision: Decision::Drop,
                audit: None,
            } => {}
            router::Classification {
                decision: Decision::RejectAttachments,
                ..
            } => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format::system(format::attachments_rejected()))
                    .await;
            }
            router::Classification {
                decision: Decision::UnknownCommand,
                ..
            } => {
                let _ = msg
                    .channel_id
                    .say(&ctx.http, format::system(&format::commands_list(self.config.command_prefix)))
                    .await;
            }
            router::Classification { decision, .. } => {
                let _ = self.tx.send(to_job(decision));
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }

    init_tracing();
    let config = match config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    tracing::info!("{}", config.summary());

    let intents = GatewayIntents::DIRECT_MESSAGES | GatewayIntents::MESSAGE_CONTENT;
    let (tx, rx) = mpsc::unbounded_channel::<Job>();
    let handler = Handler {
        tx: tx.clone(),
        config: Arc::new(config.clone()),
        bot_id: std::sync::RwLock::new(None),
    };
    let mut client = Client::builder(config.discord_token.clone(), intents)
        .event_handler(handler)
        .await
        .expect("failed to build Discord client");

    // Startup gate: open the owner's DM channel and announce. This must succeed
    // before the bot connects the gateway and starts serving — a bot that could
    // not greet its owner must not present itself as online. Transient failures
    // (network, 429, 5xx) are retried with backoff; permanent ones (401/403/...)
    // exit the process. Skipped entirely in lockdown mode (no whitelist).
    let mut dchat = DiscordChat::new(client.http.clone(), config.allowed_user.is_none());
    match &config.allowed_user {
        Some(user) => {
            let uid: u64 = match user.parse() {
                Ok(v) => v,
                Err(_) => {
                    eprintln!(
                        "PIAC_DISCORD_ALLOWED_USER_ID must be a numeric Discord user id, got: {user}"
                    );
                    std::process::exit(1);
                }
            };
            // The greeting attempt owns everything it needs; the channel is
            // wired into `dchat` only after the greeting is confirmed.
            let http = client.http.clone();
            let user_owned = user.clone();
            let user_log = user_owned.clone();
            let cwd = config.cwd.clone();
            let greeted = greet_with_retry(move || {
                let http = http.clone();
                let user = user_owned.clone();
                let cwd = cwd.clone();
                async move {
                    let dm = UserId::new(uid)
                        .create_dm_channel(&http)
                        .await
                        .map_err(|e| ChatError {
                            kind: classify_serenity_error(&e),
                            detail: e.to_string(),
                        })?;
                    let msg = format::system(&format::startup(
                        &cwd.display().to_string(),
                        &user,
                    ));
                    send_to_channel(http, dm.id, &msg).await?;
                    Ok(dm.id)
                }
            })
            .await;
            match greeted {
                Ok(channel_id) => {
                    dchat.set_channel(channel_id);
                    tracing::info!(user = %user_log, "startup message sent");
                }
                Err(e) => {
                    tracing::error!(error = %e.detail, "greeting failed (permanent), exiting");
                    std::process::exit(1);
                }
            }
        }
        None => {
            tracing::warn!(
                "LOCKDOWN MODE: no whitelist configured; messages are logged but not processed"
            );
        }
    }

    let agent = Arc::new(RealAgent::new("pi".to_string(), config.cwd.clone()));
    let dchat = Arc::new(dchat);
    let worker = Worker::new(agent.clone(), dchat.clone(), config.prompt_timeout, rx);
    let worker_task = tokio::spawn(worker.run());

    // Graceful shutdown: announce, abort the running task, drain, disconnect.
    let shard_manager = client.shard_manager.clone();
    let signal_chat = dchat.clone();
    let signal_agent = agent.clone();
    let signal_tx = tx.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        tracing::info!("shutdown signal received");
        let _ = signal_chat
            .send(format::system(format::shutting_down()))
            .await;
        signal_agent.abort().await;
        let _ = signal_tx.send(Job::Shutdown);
        let _ = tokio::time::timeout(Duration::from_secs(20), worker_task).await;
        shard_manager.shutdown_all().await;
        tracing::info!("pi-agent-connect stopped");
    });

    if let Err(e) = client.start().await {
        tracing::error!(error = %e, "discord client failed");
        std::process::exit(1);
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    let terminate = {
        let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        async move {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn greet_with_retry_retries_transient_failures_then_succeeds() {
        let mut attempts = 0u32;
        let result = greet_with_retry(|| {
            attempts += 1;
            async move {
                if attempts < 3 {
                    Err(ChatError {
                        kind: ChatErrorKind::Transient,
                        detail: "network blip".into(),
                    })
                } else {
                    Ok(ChannelId::new(1))
                }
            }
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(attempts, 3, "transient failures must be retried");
    }

    #[tokio::test]
    async fn greet_with_retry_returns_permanent_failure_immediately() {
        let mut attempts = 0u32;
        let result = greet_with_retry(|| {
            attempts += 1;
            async move {
                Err(ChatError {
                    kind: ChatErrorKind::Permanent,
                    detail: "403: cannot message this user".into(),
                })
            }
        })
        .await;
        let err = result.unwrap_err();
        assert_eq!(err.kind, ChatErrorKind::Permanent);
        assert_eq!(attempts, 1, "permanent failure must not be retried");
    }
}
