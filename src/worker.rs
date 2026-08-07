//! Single-consumer message processor.
//!
//! One queue, one worker task, no locks. The event loop is the single
//! producer; the worker is the single consumer. Control jobs (`/new`,
//! `/abort`, shutdown) interrupt the currently running task; everything
//! else (prompts, session/model/thinking commands) is processed serially.
//!
//! Timeouts and abort live here (business layer), not in the agent adapter,
//! so they are unit-testable with fake agent/chat implementations.

use crate::agent::{Agent, AgentError, SessionState, SessionStats};
use crate::chat::Chat;
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
            if current.is_none() {
                if let Some(job) = queue.pop_front() {
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
                            let _ = self.chat.send(format::system(&ack)).await;
                            continue;
                        }
                        Job::Abort => {
                            let was_running = current.is_some();
                            let cleared = queue.len();
                            queue.clear();
                            self.drain_current(&mut current).await;
                            let ack = format::abort_ack(was_running, cleared);
                            let _ = self.chat.send(format::system(&ack)).await;
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
                                    let _ = self
                                        .chat
                                        .send(format::system(&format::queued(backlog)))
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
                    let _ = self
                        .chat
                        .send(format::system(&format::internal_error("agent task failed")))
                        .await;
                }
                None => {}
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
                if let Err(e) = self.chat.send(text).await {
                    tracing::error!(error = %e.0, "failed to send response");
                }
                if fresh || known_session_id.is_none() {
                    match self.agent.get_state().await {
                        Ok(state) => {
                            *known_session_id = Some(state.session_id.clone());
                            let _ = self
                                .chat
                                .send(format::system(&format::new_session_info(&state)))
                                .await;
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "failed to query session state for new-session info");
                        }
                    }
                }
            }
            TaskOutcome::Prompt { result: Err(e), .. } => {
                let _ = self
                    .chat
                    .send(format::system(&format::agent_error(&e)))
                    .await;
            }
            TaskOutcome::PromptTimedOut => {
                let _ = self
                    .chat
                    .send(format::system(&format::prompt_timed_out(
                        self.prompt_timeout.as_secs(),
                    )))
                    .await;
            }
            TaskOutcome::Session(Ok((state, stats))) => {
                let _ = self
                    .chat
                    .send(format::system(&format::session_info(
                        &state,
                        stats.as_ref(),
                    )))
                    .await;
            }
            TaskOutcome::Session(Err(e)) => {
                let _ = self
                    .chat
                    .send(format::system(&format::agent_error(&e)))
                    .await;
            }
            TaskOutcome::Model {
                model_ref,
                result: Ok(()),
            } => {
                let _ = self
                    .chat
                    .send(format::system(&format::model_set(&model_ref)))
                    .await;
            }
            TaskOutcome::Model { result: Err(e), .. } => {
                let _ = self
                    .chat
                    .send(format::system(&format::agent_error(&e)))
                    .await;
            }
            TaskOutcome::Models(Ok(models)) => {
                let _ = self
                    .chat
                    .send(format::system(&format::models_list(&models)))
                    .await;
            }
            TaskOutcome::Models(Err(e)) => {
                let _ = self
                    .chat
                    .send(format::system(&format::agent_error(&e)))
                    .await;
            }
            TaskOutcome::Thinking {
                level,
                result: Ok(()),
            } => {
                let _ = self
                    .chat
                    .send(format::system(&format::thinking_set(&level)))
                    .await;
            }
            TaskOutcome::Thinking { result: Err(e), .. } => {
                let _ = self
                    .chat
                    .send(format::system(&format::agent_error(&e)))
                    .await;
            }
            TaskOutcome::ThinkingLevels(Ok(levels)) => {
                let _ = self
                    .chat
                    .send(format::system(&format::levels_list(&levels)))
                    .await;
            }
            TaskOutcome::ThinkingLevels(Err(e)) => {
                let _ = self
                    .chat
                    .send(format::system(&format::agent_error(&e)))
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
        behaviors: Arc<StdMutex<VecDeque<Behavior>>>,
        abort_tx: Arc<watch::Sender<bool>>,
        state: Arc<StdMutex<SessionState>>,
        stats: Arc<StdMutex<SessionStats>>,
        models: Arc<StdMutex<Vec<String>>>,
        levels: Arc<StdMutex<Vec<String>>>,
    }

    impl FakeAgent {
        fn new() -> Self {
            let (abort_tx, _) = watch::channel(false);
            FakeAgent {
                calls: Arc::new(StdMutex::new(Vec::new())),
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
            self.calls
                .lock()
                .unwrap()
                .push(format!("prompt(fresh={fresh}):{prompt}"));
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
            self.calls.lock().unwrap().push("abort".into());
            let _ = self.abort_tx.send(true);
        }

        async fn get_state(&self) -> Result<SessionState, AgentError> {
            self.calls.lock().unwrap().push("get_state".into());
            Ok(self.state.lock().unwrap().clone())
        }

        async fn get_session_stats(&self) -> Result<SessionStats, AgentError> {
            self.calls.lock().unwrap().push("get_session_stats".into());
            Ok(self.stats.lock().unwrap().clone())
        }

        async fn set_model(&self, model_ref: &str) -> Result<(), AgentError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("set_model:{model_ref}"));
            Ok(())
        }

        async fn list_models(&self) -> Result<Vec<String>, AgentError> {
            self.calls.lock().unwrap().push("list_models".into());
            Ok(self.models.lock().unwrap().clone())
        }

        async fn set_thinking(&self, level: &str) -> Result<(), AgentError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("set_thinking:{level}"));
            Ok(())
        }

        async fn list_thinking_levels(&self) -> Result<Vec<String>, AgentError> {
            self.calls
                .lock()
                .unwrap()
                .push("list_thinking_levels".into());
            Ok(self.levels.lock().unwrap().clone())
        }
    }

    #[derive(Clone)]
    struct FakeChat {
        sent: Arc<StdMutex<Vec<String>>>,
        typing: Arc<AtomicUsize>,
    }

    impl FakeChat {
        fn new() -> Self {
            FakeChat {
                sent: Arc::new(StdMutex::new(Vec::new())),
                typing: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn messages(&self) -> Vec<String> {
            self.sent.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Chat for FakeChat {
        async fn send(&self, text: String) -> Result<(), ChatError> {
            self.sent.lock().unwrap().push(text);
            Ok(())
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

    #[tokio::test]
    async fn shutdown_aborts_current_and_exits() {
        let agent = FakeAgent::new();
        agent.hang();
        let chat = FakeChat::new();
        let (calls, _) = drive(agent, chat, vec![Job::Prompt("a".into()), Job::Shutdown]).await;
        assert!(calls.contains(&"abort".to_string()));
    }
}
