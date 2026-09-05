//! Single-consumer message processor.
//!
//! One queue, one worker task, no locks. The event loop is the single
//! producer; the worker is the single consumer. Control jobs (`.new`,
//! `.abort`, shutdown) interrupt the currently running task; everything
//! else (prompts, session/model/thinking commands) is processed serially.
//!
//! Timeouts and abort live here (business layer), not in the agent adapter,
//! so they are unit-testable with fake agent/chat implementations.

use crate::agent::{Agent, AgentError, SessionState, SessionStats};
use crate::chat::{next_backoff, Chat, ChatErrorKind, RETRY_BACKOFF_INITIAL};
use crate::format;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    Prompt(String),
    New,
    Abort,
    Session,
    Model(Option<String>),
    Thinking(Option<String>),
    Shutdown,
}

pub struct Worker<A: Agent, C: Chat> {
    agent: Arc<A>,
    chat: Arc<C>,
    prompt_timeout: Duration,
    rx: mpsc::UnboundedReceiver<Job>,
}

struct Running {
    done: oneshot::Receiver<TaskOutcome>,
    task: JoinHandle<()>,
}

enum TaskOutcome {
    Prompt {
        fresh: bool,
        result: Result<String, AgentError>,
    },
    PromptTimedOut,
    Session(Result<(SessionState, Option<SessionStats>), AgentError>),
    Model {
        model_ref: String,
        result: Result<(), AgentError>,
    },
    Models(Result<Vec<String>, AgentError>),
    Thinking {
        level: String,
        result: Result<(), AgentError>,
    },
    ThinkingLevels(Result<Vec<String>, AgentError>),
}

const ABORT_DRAIN_TIMEOUT: Duration = Duration::from_secs(15);
const TYPING_INTERVAL: Duration = Duration::from_secs(8);

impl<A: Agent + 'static, C: Chat + 'static> Worker<A, C> {
    pub fn new(
        agent: Arc<A>,
        chat: Arc<C>,
        prompt_timeout: Duration,
        rx: mpsc::UnboundedReceiver<Job>,
    ) -> Self {
        Worker {
            agent,
            chat,
            prompt_timeout,
            rx,
        }
    }

    pub async fn run(mut self) {
        let mut current: Option<Running> = None;
        let mut queue: VecDeque<Job> = VecDeque::new();
        let mut fresh = false;
        let mut known_session_id: Option<String> = None;
        let mut rx_closed = false;

        loop {
            // Start the next queued job when idle.
            if current.is_none()
                && let Some(job) = queue.pop_front()
            {
                match job {
                    Job::Shutdown => return,
                    Job::Prompt(prompt) => {
                        current = Some(self.start_prompt(prompt, fresh));
                        fresh = false;
                    }
                    Job::Session => current = Some(self.start_session()),
                    Job::Model(arg) => current = Some(self.start_model(arg)),
                    Job::Thinking(arg) => current = Some(self.start_thinking(arg)),
                    Job::New | Job::Abort => {}
                }
            }
            if current.is_none() && queue.is_empty() && rx_closed {
                return;
            }

            let outcome: Option<Result<TaskOutcome, oneshot::error::RecvError>> = tokio::select! {
                maybe = self.rx.recv(), if !rx_closed => {
                    match maybe {
                        None => {
                            // All senders dropped: finish the remaining queue, then exit.
                            rx_closed = true;
                            None
                        }
                        Some(job) => {
                            match job {
                        Job::New => {
                            let had_task = current.is_some();
                            let cleared = queue.len();
                            queue.clear();
                            self.drain_current(&mut current).await;
                            fresh = true;
                            let mut ack = format::new_ack().to_string();
                            let mut extra = Vec::new();
                            if had_task {
                                extra.push("aborted running task".to_string());
                            }
                            if cleared > 0 {
                                extra.push(format::cleared_msg(cleared));
                            }
                            if !extra.is_empty() {
                                ack = format!("{ack}; {}", extra.join(", "));
                            }
                            self.send_reliable(format::system(&ack), "new ack").await;
                            continue;
                        }
                        Job::Abort => {
                            let was_running = current.is_some();
                            let cleared = queue.len();
                            queue.clear();
                            self.drain_current(&mut current).await;
                            let ack = format::abort_ack(was_running, cleared);
                            self.send_reliable(format::system(&ack), "abort ack").await;
                            continue;
                        }
                        Job::Shutdown => {
                            self.drain_current(&mut current).await;
                            return;
                        }
                        other => {
                            if current.is_some() {
                                let backlog = queue.len() + 1;
                                let is_prompt = matches!(other, Job::Prompt(_));
                                queue.push_back(other);
                                if is_prompt {
                                    self.send_reliable(
                                        format::system(&format::queued(backlog)),
                                        "queued notice",
                                    )
                                    .await;
                                }
                            } else {
                                queue.push_back(other);
                            }
                        }
                            }
                            None
                        }
                    }
                }
                res = async {
                    match current.as_mut() {
                        Some(r) => Some((&mut r.done).await),
                        None => None,
                    }
                }, if current.is_some() => {
                    res
                }
            };

            match outcome {
                Some(Ok(task_outcome)) => {
                    if let Some(r) = current.take() {
                        let _ = r.task.await;
                    }
                    self.handle_outcome(task_outcome, &mut known_session_id)
                        .await;
                }
                Some(Err(_)) => {
                    let _ = current.take();
                    self.send_reliable(
                        format::system(&format::internal_error("agent task failed")),
                        "internal error notice",
                    )
                    .await;
                }
                None => {}
            }
        }
    }

    /// Send a message, retrying transient failures with backoff until success.
    ///
    /// Fail-closed semantics: a bot that cannot deliver a reply must not keep
    /// executing blindly (the owner cannot see results and cannot abort). On a
    /// transient failure (network, 429, 5xx) this retries with exponential
    /// backoff while queued jobs accumulate; on a permanent failure (4xx other
    /// than 429, missing channel) the process is terminated with a clear log.
    ///
    /// Retry granularity is the whole message: if a multi-chunk message (>
    /// 2000 chars, split by `Chat::send`) fails mid-send, the already-delivered
    /// chunks are re-sent on the next attempt. Accepted: duplicates only occur
    /// during a transient outage on an unusually long message, and delivery
    /// matters more than dedup.
    async fn send_reliable(&self, text: String, what: &str) {
        let mut delay = RETRY_BACKOFF_INITIAL;
        let mut attempt: u32 = 0;
        loop {
            match self.chat.send(text.clone()).await {
                Ok(()) => return,
                Err(e) if e.kind == ChatErrorKind::Transient => {
                    attempt += 1;
                    tracing::warn!(
                        what,
                        attempt,
                        error = %e.detail,
                        ?delay,
                        "send failed (transient), retrying"
                    );
                    tokio::time::sleep(delay).await;
                    delay = next_backoff(delay);
                }
                Err(e) => {
                    tracing::error!(
                        what,
                        error = %e.detail,
                        "send failed (permanent), terminating"
                    );
                    // `process::exit` skips destructors, so SIGTERM a live pi
                    // child before exiting rather than leaving it running.
                    // Note: `abort`'s SIGKILL escalation runs in a spawned
                    // task that does not survive `process::exit`; this is
                    // SIGTERM-only, which pi honours in practice.
                    self.agent.abort().await;
                    std::process::exit(1);
                }
            }
        }
    }

    /// Abort the running task and wait (bounded) for it to die.
    async fn drain_current(&self, current: &mut Option<Running>) {
        if let Some(r) = current.take() {
            self.agent.abort().await;
            let _ = tokio::time::timeout(ABORT_DRAIN_TIMEOUT, r.done).await;
            let _ = tokio::time::timeout(Duration::from_secs(2), r.task).await;
        }
    }

    fn start_prompt(&self, prompt: String, fresh: bool) -> Running {
        let agent = self.agent.clone();
        let chat = self.chat.clone();
        let timeout = self.prompt_timeout;
        let (tx, done) = oneshot::channel();
        let task = tokio::spawn(async move {
            let typing = {
                let chat = chat.clone();
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(TYPING_INTERVAL);
                    interval.tick().await; // fire immediately
                    loop {
                        chat.send_typing().await;
                        interval.tick().await;
                    }
                })
            };
            let outcome = match tokio::time::timeout(timeout, agent.run_prompt(prompt, fresh)).await
            {
                Ok(Ok(text)) => TaskOutcome::Prompt {
                    fresh,
                    result: Ok(text),
                },
                Ok(Err(e)) => TaskOutcome::Prompt {
                    fresh,
                    result: Err(e),
                },
                Err(_) => {
                    agent.abort().await;
                    TaskOutcome::PromptTimedOut
                }
            };
            typing.abort();
            let _ = tx.send(outcome);
        });
        Running { done, task }
    }

    fn start_session(&self) -> Running {
        let agent = self.agent.clone();
        let (tx, done) = oneshot::channel();
        let task = tokio::spawn(async move {
            let outcome = match agent.get_state().await {
                Ok(state) => {
                    let stats = agent.get_session_stats().await.ok();
                    TaskOutcome::Session(Ok((state, stats)))
                }
                Err(e) => TaskOutcome::Session(Err(e)),
            };
            let _ = tx.send(outcome);
        });
        Running { done, task }
    }

    fn start_model(&self, arg: Option<String>) -> Running {
        let agent = self.agent.clone();
        let (tx, done) = oneshot::channel();
        let task = tokio::spawn(async move {
            let outcome = match arg {
                Some(model_ref) => {
                    let result = agent.set_model(&model_ref).await;
                    TaskOutcome::Model { model_ref, result }
                }
                None => TaskOutcome::Models(agent.list_models().await),
            };
            let _ = tx.send(outcome);
        });
        Running { done, task }
    }

    fn start_thinking(&self, arg: Option<String>) -> Running {
        let agent = self.agent.clone();
        let (tx, done) = oneshot::channel();
        let task = tokio::spawn(async move {
            let outcome = match arg {
                Some(level) => {
                    let result = agent.set_thinking(&level).await;
                    TaskOutcome::Thinking { level, result }
                }
                None => TaskOutcome::ThinkingLevels(agent.list_thinking_levels().await),
            };
            let _ = tx.send(outcome);
        });
        Running { done, task }
    }

    async fn handle_outcome(&self, outcome: TaskOutcome, known_session_id: &mut Option<String>) {
        match outcome {
            TaskOutcome::Prompt {
                fresh,
                result: Ok(text),
            } => {
                self.send_reliable(text, "prompt reply").await;
                if fresh || known_session_id.is_none() {
                    match self.agent.get_state().await {
                        Ok(state) => {
                            *known_session_id = Some(state.session_id.clone());
                            self.send_reliable(
                                format::system(&format::new_session_info(&state)),
                                "session info",
                            )
                            .await;
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to query session state for new-session info");
                        }
                    }
                }
            }
            TaskOutcome::Prompt { result: Err(e), .. } => {
                self.send_reliable(format::system(&format::agent_error(&e)), "error reply")
                    .await;
            }
            TaskOutcome::PromptTimedOut => {
                self.send_reliable(
                    format::system(&format::prompt_timed_out(self.prompt_timeout.as_secs())),
                    "timeout notice",
                )
                .await;
            }
            TaskOutcome::Session(Ok((state, stats))) => {
                self.send_reliable(
                    format::system(&format::session_info(&state, stats.as_ref())),
                    "session info",
                )
                .await;
            }
            TaskOutcome::Session(Err(e)) => {
                self.send_reliable(format::system(&format::agent_error(&e)), "error reply")
                    .await;
            }
            TaskOutcome::Model {
                model_ref,
                result: Ok(()),
            } => {
                self.send_reliable(format::system(&format::model_set(&model_ref)), "model ack")
                    .await;
            }
            TaskOutcome::Model { result: Err(e), .. } => {
                self.send_reliable(format::system(&format::agent_error(&e)), "error reply")
                    .await;
            }
            TaskOutcome::Models(Ok(models)) => {
                self.send_reliable(format::system(&format::models_list(&models)), "models list")
                    .await;
            }
            TaskOutcome::Models(Err(e)) => {
                self.send_reliable(format::system(&format::agent_error(&e)), "error reply")
                    .await;
            }
            TaskOutcome::Thinking {
                level,
                result: Ok(()),
            } => {
                self.send_reliable(format::system(&format::thinking_set(&level)), "thinking ack")
                    .await;
            }
            TaskOutcome::Thinking { result: Err(e), .. } => {
                self.send_reliable(format::system(&format::agent_error(&e)), "error reply")
                    .await;
            }
            TaskOutcome::ThinkingLevels(Ok(levels)) => {
                self.send_reliable(format::system(&format::levels_list(&levels)), "levels list")
                    .await;
            }
            TaskOutcome::ThinkingLevels(Err(e)) => {
                self.send_reliable(format::system(&format::agent_error(&e)), "error reply")
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ChatError;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::watch;

    // ── Fakes ──────────────────────────────────────────────────────────

    #[derive(Debug, Clone, PartialEq)]
    enum Behavior {
        Ok(String),
        Err(AgentError),
        Hang,
    }

    #[derive(Clone)]
    struct FakeAgent {
        calls: Arc<StdMutex<Vec<String>>>,
        /// Shared event log (with FakeChat) for cross-component ordering assertions.
        events: Arc<StdMutex<Vec<String>>>,
        behaviors: Arc<StdMutex<VecDeque<Behavior>>>,
        abort_tx: Arc<watch::Sender<bool>>,
        state: Arc<StdMutex<SessionState>>,
        stats: Arc<StdMutex<SessionStats>>,
        models: Arc<StdMutex<Vec<String>>>,
        levels: Arc<StdMutex<Vec<String>>>,
    }

    impl FakeAgent {
        fn new() -> Self {
            Self::with_events(Arc::new(StdMutex::new(Vec::new())))
        }

        fn with_events(events: Arc<StdMutex<Vec<String>>>) -> Self {
            let (abort_tx, _) = watch::channel(false);
            FakeAgent {
                calls: Arc::new(StdMutex::new(Vec::new())),
                events,
                behaviors: Arc::new(StdMutex::new(VecDeque::new())),
                abort_tx: Arc::new(abort_tx),
                state: Arc::new(StdMutex::new(SessionState {
                    session_id: "sess-1".into(),
                    session_file: "/tmp/s.jsonl".into(),
                    model: Some("deepseek/deepseek-v4-flash".into()),
                    thinking: Some("high".into()),
                })),
                stats: Arc::new(StdMutex::new(SessionStats {
                    tokens_input: 10,
                    tokens_output: 20,
                    tokens_cache_read: 30,
                    cost: Some(0.5),
                })),
                models: Arc::new(StdMutex::new(vec!["deepseek/a".into(), "qiuming/b".into()])),
                levels: Arc::new(StdMutex::new(vec!["off".into(), "high".into()])),
            }
        }

        fn note(&self, s: &str) {
            self.calls.lock().unwrap().push(s.to_string());
            self.events.lock().unwrap().push(s.to_string());
        }

        fn reply(&self, text: &str) {
            self.behaviors
                .lock()
                .unwrap()
                .push_back(Behavior::Ok(text.to_string()));
        }

        fn fail(&self, e: AgentError) {
            self.behaviors.lock().unwrap().push_back(Behavior::Err(e));
        }

        fn hang(&self) {
            self.behaviors.lock().unwrap().push_back(Behavior::Hang);
        }

        fn call_log(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Agent for FakeAgent {
        async fn run_prompt(&self, prompt: String, fresh: bool) -> Result<String, AgentError> {
            self.note(&format!("prompt(fresh={fresh}):{prompt}"));
            let behavior = self.behaviors.lock().unwrap().pop_front();
            match behavior {
                Some(Behavior::Ok(t)) => Ok(t),
                Some(Behavior::Err(e)) => Err(e),
                Some(Behavior::Hang) => {
                    let mut rx = self.abort_tx.subscribe();
                    loop {
                        if *rx.borrow() {
                            break;
                        }
                        if rx.changed().await.is_err() {
                            break;
                        }
                    }
                    Err(AgentError::NonZeroExit {
                        code: None,
                        stderr: "killed".into(),
                    })
                }
                None => Ok("default reply".into()),
            }
        }

        async fn abort(&self) {
            self.note("abort");
            let _ = self.abort_tx.send(true);
        }

        async fn get_state(&self) -> Result<SessionState, AgentError> {
            self.note("get_state");
            Ok(self.state.lock().unwrap().clone())
        }

        async fn get_session_stats(&self) -> Result<SessionStats, AgentError> {
            self.note("get_session_stats");
            Ok(self.stats.lock().unwrap().clone())
        }

        async fn set_model(&self, model_ref: &str) -> Result<(), AgentError> {
            self.note(&format!("set_model:{model_ref}"));
            Ok(())
        }

        async fn list_models(&self) -> Result<Vec<String>, AgentError> {
            self.note("list_models");
            Ok(self.models.lock().unwrap().clone())
        }

        async fn set_thinking(&self, level: &str) -> Result<(), AgentError> {
            self.note(&format!("set_thinking:{level}"));
            Ok(())
        }

        async fn list_thinking_levels(&self) -> Result<Vec<String>, AgentError> {
            self.note("list_thinking_levels");
            Ok(self.levels.lock().unwrap().clone())
        }
    }

    #[derive(Clone)]
    struct FakeChat {
        sent: Arc<StdMutex<Vec<String>>>,
        /// Shared event log (with FakeAgent) for cross-component ordering assertions.
        events: Arc<StdMutex<Vec<String>>>,
        typing: Arc<AtomicUsize>,
        /// Pending transient failures keyed by exact send text (consumed in order).
        fail_on: Arc<StdMutex<VecDeque<(String, usize)>>>,
    }

    impl FakeChat {
        fn new() -> Self {
            Self::with_events(Arc::new(StdMutex::new(Vec::new())))
        }

        fn with_events(events: Arc<StdMutex<Vec<String>>>) -> Self {
            FakeChat {
                sent: Arc::new(StdMutex::new(Vec::new())),
                events,
                typing: Arc::new(AtomicUsize::new(0)),
                fail_on: Arc::new(StdMutex::new(VecDeque::new())),
            }
        }

        /// Make the next `count` sends of exactly `text` fail transiently.
        fn fail_sends_transiently_for(&self, text: &str, count: usize) {
            self.fail_on
                .lock()
                .unwrap()
                .push_back((text.to_string(), count));
        }

        fn messages(&self) -> Vec<String> {
            self.sent.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Chat for FakeChat {
        async fn send(&self, text: String) -> Result<(), ChatError> {
            let fail = {
                let mut q = self.fail_on.lock().unwrap();
                match q.iter_mut().find(|(t, _)| *t == text) {
                    Some((_, n)) if *n > 0 => {
                        *n -= 1;
                        true
                    }
                    _ => false,
                }
            };
            if fail {
                self.events.lock().unwrap().push(format!("send-fail:{text}"));
                Err(ChatError {
                    kind: ChatErrorKind::Transient,
                    detail: "network blip".into(),
                })
            } else {
                self.sent.lock().unwrap().push(text.clone());
                self.events.lock().unwrap().push(format!("send-ok:{text}"));
                Ok(())
            }
        }

        async fn send_typing(&self) {
            self.typing.fetch_add(1, Ordering::SeqCst);
        }
    }

    // ── Harness ────────────────────────────────────────────────────────

    async fn drive(agent: FakeAgent, chat: FakeChat, jobs: Vec<Job>) -> (Vec<String>, Vec<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        for job in jobs {
            tx.send(job).unwrap();
        }
        drop(tx);
        let worker = Worker::new(
            Arc::new(agent.clone()),
            Arc::new(chat.clone()),
            Duration::from_secs(30),
            rx,
        );
        worker.run().await;
        (agent.call_log(), chat.messages())
    }

    // ── Tests ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn prompt_flow_posts_reply_and_first_session_info() {
        let agent = FakeAgent::new();
        agent.reply("pi answer");
        let chat = FakeChat::new();
        let (calls, messages) = drive(agent, chat, vec![Job::Prompt("hello".into())]).await;

        // Pi reply is sent raw (no [pi-agent-connect] prefix).
        assert!(messages.iter().any(|m| m == "pi answer"), "{messages:?}");
        // First message since startup: background session info is posted.
        assert!(
            messages
                .iter()
                .any(|m| m.starts_with("[pi-agent-connect] new session: id=sess-1")),
            "{messages:?}"
        );
        assert_eq!(calls[0], "prompt(fresh=false):hello");
        assert!(calls.contains(&"get_state".to_string()), "{calls:?}");
    }

    #[tokio::test]
    async fn messages_processed_serially() {
        let agent = FakeAgent::new();
        agent.reply("r1");
        agent.reply("r2");
        agent.reply("r3");
        let chat = FakeChat::new();
        let (calls, _) = drive(
            agent,
            chat,
            vec![
                Job::Prompt("a".into()),
                Job::Prompt("b".into()),
                Job::Prompt("c".into()),
            ],
        )
        .await;
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.starts_with("prompt"))
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                "prompt(fresh=false):a".to_string(),
                "prompt(fresh=false):b".to_string(),
                "prompt(fresh=false):c".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn new_marks_next_prompt_fresh() {
        let agent = FakeAgent::new();
        agent.reply("ra");
        agent.reply("rb");
        let chat = FakeChat::new();
        let (calls, messages) = drive(
            agent,
            chat,
            vec![Job::New, Job::Prompt("a".into()), Job::Prompt("b".into())],
        )
        .await;
        let prompts: Vec<_> = calls
            .iter()
            .filter(|c| c.starts_with("prompt"))
            .cloned()
            .collect();
        assert_eq!(prompts[0], "prompt(fresh=true):a");
        assert_eq!(prompts[1], "prompt(fresh=false):b");
        assert!(
            messages.contains(
                &"[pi-agent-connect] ok, next message will start a new session".to_string()
            )
        );
    }

    #[tokio::test]
    async fn new_aborts_running_task_clears_queue() {
        let agent = FakeAgent::new();
        agent.hang(); // "a" hangs until aborted
        let chat = FakeChat::new();
        let (calls, messages) = drive(
            agent,
            chat,
            vec![
                Job::Prompt("a".into()),
                Job::Prompt("b".into()),
                Job::New,
                Job::Prompt("c".into()),
            ],
        )
        .await;

        let prompts: Vec<_> = calls
            .iter()
            .filter(|c| c.starts_with("prompt"))
            .cloned()
            .collect();
        assert_eq!(
            prompts,
            vec![
                "prompt(fresh=false):a".to_string(),
                "prompt(fresh=true):c".to_string()
            ]
        );
        assert!(calls.contains(&"abort".to_string()), "{calls:?}");
        assert!(
            messages.contains(&"[pi-agent-connect] queued (backlog: 1)".to_string()),
            "{messages:?}"
        );
        assert!(
            messages.contains(
                &"[pi-agent-connect] ok, next message will start a new session; aborted running task, cleared 1 queued message"
                    .to_string()
            ),
            "{messages:?}"
        );
    }

    #[tokio::test]
    async fn abort_kills_running_task() {
        let agent = FakeAgent::new();
        agent.hang();
        let chat = FakeChat::new();
        let (calls, messages) = drive(agent, chat, vec![Job::Prompt("a".into()), Job::Abort]).await;
        assert!(calls.contains(&"abort".to_string()), "{calls:?}");
        assert!(
            messages.contains(&"[pi-agent-connect] aborted running task".to_string()),
            "{messages:?}"
        );
        // The aborted task's error must be suppressed.
        assert!(
            !messages.iter().any(|m| m.contains("pi exited with code")),
            "{messages:?}"
        );
    }

    #[tokio::test]
    async fn abort_idle_reports_nothing_running() {
        let agent = FakeAgent::new();
        let chat = FakeChat::new();
        let (_, messages) = drive(agent, chat, vec![Job::Abort]).await;
        assert!(messages.contains(&"[pi-agent-connect] nothing was running".to_string()));
    }

    #[tokio::test]
    async fn pi_error_is_mapped() {
        let agent = FakeAgent::new();
        agent.fail(AgentError::NonZeroExit {
            code: Some(1),
            stderr: "boom".into(),
        });
        let chat = FakeChat::new();
        let (_, messages) = drive(agent, chat, vec![Job::Prompt("x".into())]).await;
        assert!(
            messages.contains(&"[pi-agent-connect] pi exited with code 1: boom".to_string()),
            "{messages:?}"
        );
    }

    #[tokio::test]
    async fn hung_prompt_times_out() {
        let agent = FakeAgent::new();
        agent.hang();
        let chat = FakeChat::new();
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(Job::Prompt("x".into())).unwrap();
        drop(tx);
        let worker = Worker::new(
            Arc::new(agent.clone()),
            Arc::new(chat.clone()),
            Duration::from_millis(1100),
            rx,
        );
        worker.run().await;
        assert!(
            chat.messages()
                .contains(&"[pi-agent-connect] pi timed out after 1s, aborted".to_string()),
            "{:?}",
            chat.messages()
        );
        assert!(agent.call_log().contains(&"abort".to_string()));
    }

    #[tokio::test]
    async fn session_command_reports_state_and_stats() {
        let agent = FakeAgent::new();
        let chat = FakeChat::new();
        let (calls, messages) = drive(agent, chat, vec![Job::Session]).await;
        assert!(calls.contains(&"get_state".to_string()));
        assert!(calls.contains(&"get_session_stats".to_string()));
        assert!(
            messages
                .iter()
                .any(|m| m.starts_with("[pi-agent-connect] session: id=sess-1")
                    && m.contains("tokens: in=10 out=20")),
            "{messages:?}"
        );
    }

    #[tokio::test]
    async fn model_and_thinking_commands() {
        let agent = FakeAgent::new();
        let chat = FakeChat::new();
        let (calls, messages) = drive(
            agent,
            chat,
            vec![
                Job::Model(Some("deepseek/x".into())),
                Job::Model(None),
                Job::Thinking(Some("high".into())),
                Job::Thinking(None),
            ],
        )
        .await;
        assert!(calls.contains(&"set_model:deepseek/x".to_string()));
        assert!(calls.contains(&"list_models".to_string()));
        assert!(calls.contains(&"set_thinking:high".to_string()));
        assert!(calls.contains(&"list_thinking_levels".to_string()));
        assert!(messages.contains(&"[pi-agent-connect] model set to deepseek/x".to_string()));
        assert!(
            messages.contains(
                &"[pi-agent-connect] available models: deepseek/a, qiuming/b".to_string()
            )
        );
        assert!(messages.contains(&"[pi-agent-connect] thinking level set to high".to_string()));
        assert!(messages.contains(&"[pi-agent-connect] available levels: off, high".to_string()));
    }
    #[tokio::test(start_paused = true)]
    async fn transient_send_failure_retries_and_blocks_until_delivered() {
        // Prompt a's reply fails transiently twice, then succeeds. The worker
        // must retry until it is delivered, and must NOT start prompt b before
        // that — fail-closed: no blind execution while replies cannot be sent.
        let events = Arc::new(StdMutex::new(Vec::new()));
        let agent = FakeAgent::with_events(events.clone());
        agent.reply("r1");
        agent.reply("r2");
        let chat = FakeChat::with_events(events.clone());
        chat.fail_sends_transiently_for("r1", 2);
        let (_, _) = drive(
            agent,
            chat,
            vec![Job::Prompt("a".into()), Job::Prompt("b".into())],
        )
        .await;

        let events = events.lock().unwrap().clone();
        let fails = events
            .iter()
            .filter(|e| e.as_str() == "send-fail:r1")
            .count();
        assert_eq!(fails, 2, "reply r1 must be retried until success: {events:?}");
        let delivered = events
            .iter()
            .position(|e| e.as_str() == "send-ok:r1")
            .expect("r1 eventually delivered");
        let prompt_b = events
            .iter()
            .position(|e| e.as_str() == "prompt(fresh=false):b")
            .expect("prompt b eventually runs");
        assert!(
            delivered < prompt_b,
            "reply for a must be delivered before b executes: {events:?}"
        );
        assert!(
            events.iter().any(|e| e.as_str() == "send-ok:r2"),
            "reply r2 delivered after recovery: {events:?}"
        );
    }

    #[tokio::test]
    async fn shutdown_aborts_current_and_exits() {
        let agent = FakeAgent::new();
        agent.hang();
        let chat = FakeChat::new();
        let (calls, _) = drive(agent, chat, vec![Job::Prompt("a".into()), Job::Shutdown]).await;
        assert!(calls.contains(&"abort".to_string()));
    }
}
