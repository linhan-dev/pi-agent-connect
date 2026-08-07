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
use chat::{Chat, DiscordChat};
use router::Decision;
use serenity::async_trait;
use serenity::client::{Client, Context, EventHandler};
use serenity::model::channel::Message;
use serenity::model::gateway::{GatewayIntents, Ready};
use serenity::model::id::UserId;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use worker::{Job, Worker};

const PROMPT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

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
        let classification =
            router::classify(self.config.allowed_user.as_deref(), &bot_id, &incoming);
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
                    .say(&ctx.http, format::system(format::commands_list()))
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

    // Startup: open the owner's DM channel and announce (skipped in lockdown mode).
    let mut dchat = DiscordChat::new(client.http.clone());
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
            match UserId::new(uid).create_dm_channel(&client.http).await {
                Ok(dm) => {
                    dchat.set_channel(dm.id);
                    let msg = format::startup(&config.cwd.display().to_string(), user);
                    if let Err(e) = dchat.send(format::system(&msg)).await {
                        tracing::error!(error = %e.0, "failed to send startup message");
                    }
                    tracing::info!(user = %user, "startup message sent");
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to create DM channel with owner");
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
    let worker = Worker::new(agent.clone(), dchat.clone(), PROMPT_TIMEOUT, rx);
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
