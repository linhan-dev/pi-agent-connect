//! Message formatting (pure). All pico system messages carry the `[pico]` prefix,
//! are in English and contain no emoji. Pi's own replies are forwarded as-is.

use crate::agent::{AgentError, SessionState, SessionStats};

pub const PREFIX: &str = "[pico]";

pub fn pico(text: &str) -> String {
    format!("{PREFIX} {text}")
}

/// Split a message into chunks of at most `max` bytes, preferring line breaks.
pub fn split_message(text: &str, max: usize) -> Vec<String> {
    if text.len() <= max {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut rest = text;
    while rest.len() > max {
        // Largest char-boundary index strictly below `max`.
        let boundary = rest
            .char_indices()
            .take_while(|(i, _)| *i < max)
            .map(|(i, c)| i + c.len_utf8())
            .last()
            .unwrap_or(max);
        let split_at = rest[..boundary].rfind('\n').map(|i| i + 1).unwrap_or(boundary);
        chunks.push(rest[..split_at].to_string());
        rest = rest[split_at..].trim_start_matches('\n');
    }
    if !rest.is_empty() {
        chunks.push(rest.to_string());
    }
    chunks
}

pub const fn discord_max() -> usize {
    2000
}

// ── Command confirmations ─────────────────────────────────────────────

pub fn new_ack() -> &'static str {
    "ok, next message will start a new session"
}

pub fn abort_ack(was_running: bool, cleared: usize) -> String {
    let head = if was_running {
        "aborted running task".to_string()
    } else {
        "nothing was running".to_string()
    };
    if cleared > 0 {
        format!("{head}; {}", cleared_msg(cleared))
    } else {
        head
    }
}

pub fn cleared_msg(n: usize) -> String {
    if n == 1 {
        "cleared 1 queued message".to_string()
    } else {
        format!("cleared {n} queued messages")
    }
}

pub fn commands_list() -> &'static str {
    "commands: /new /abort /session /model /thinking"
}

pub fn queued(backlog: usize) -> String {
    format!("queued (backlog: {backlog})")
}

pub fn attachments_rejected() -> &'static str {
    "attachments are not supported"
}

pub fn startup(cwd: &str, user: &str) -> String {
    format!("pico started: cwd={cwd}, user={user}")
}

pub fn shutting_down() -> &'static str {
    "pico shutting down"
}

pub fn internal_error(msg: &str) -> String {
    format!("internal error: {msg}")
}

pub fn prompt_timed_out(seconds: u64) -> String {
    format!("pi timed out after {seconds}s, aborted")
}

pub fn model_set(model_ref: &str) -> String {
    format!("model set to {model_ref}")
}

pub fn models_list(models: &[String]) -> String {
    format!("available models: {}", models.join(", "))
}

pub fn thinking_set(level: &str) -> String {
    format!("thinking level set to {level}")
}

pub fn levels_list(levels: &[String]) -> String {
    format!("available levels: {}", levels.join(", "))
}

// ── Errors / session info ─────────────────────────────────────────────

pub fn agent_error(e: &AgentError) -> String {
    match e {
        AgentError::SpawnFailed(msg) => format!("failed to start pi: {msg}"),
        AgentError::NonZeroExit { code, stderr } => {
            let stderr = truncate(stderr, 300);
            match code {
                Some(c) => format!("pi exited with code {c}: {stderr}"),
                None => format!("pi exited with error: {stderr}"),
            }
        }
        AgentError::EmptyOutput => "pi finished without output".to_string(),
        AgentError::Rpc { error, .. } => format!("pi rejected: {error}"),
        AgentError::RpcTimeout { seconds, .. } => format!("pi rejected: timed out after {seconds}s"),
    }
}

pub fn session_info(state: &SessionState, stats: Option<&SessionStats>) -> String {
    let base = format!(
        "session: id={} file={} model={} thinking={}",
        state.session_id,
        state.session_file,
        state.model.as_deref().unwrap_or("unknown"),
        state.thinking.as_deref().unwrap_or("unknown"),
    );
    match stats {
        Some(s) => format!(
            "{base} tokens: in={} out={} cache={} cost={}",
            s.tokens_input,
            s.tokens_output,
            s.tokens_cache_read,
            s.cost.map(|c| format!("{c:.4}")).unwrap_or_else(|| "n/a".to_string()),
        ),
        None => base,
    }
}

pub fn new_session_info(state: &SessionState) -> String {
    format!(
        "new session: id={} file={} model={} thinking={}",
        state.session_id,
        state.session_file,
        state.model.as_deref().unwrap_or("unknown"),
        state.thinking.as_deref().unwrap_or("unknown"),
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix() {
        assert_eq!(pico("hi"), "[pico] hi");
    }

    #[test]
    fn split_at_newlines() {
        let text = "aaaa\nbbbb\ncccc\ndddd";
        let chunks = split_message(text, 10);
        assert_eq!(chunks, vec!["aaaa\nbbbb\n".to_string(), "cccc\ndddd".to_string()]);
    }

    #[test]
    fn split_hard_no_newline() {
        let text = "abcdefghijklmnop";
        let chunks = split_message(text, 8);
        assert_eq!(chunks, vec!["abcdefgh".to_string(), "ijklmnop".to_string()]);
    }

    #[test]
    fn split_multibyte_boundaries() {
        let text = "你好世界你好世界你好世界";
        let chunks = split_message(text, 12); // 12 bytes = 4 CJK chars
        for c in &chunks {
            assert!(c.len() <= 12);
        }
        assert_eq!(chunks.iter().map(|s| s.as_str()).collect::<Vec<_>>().concat(), text);
    }

    #[test]
    fn short_message_not_split() {
        assert_eq!(split_message("hello", 2000), vec!["hello".to_string()]);
    }

    #[test]
    fn agent_error_mapping() {
        assert_eq!(
            agent_error(&AgentError::EmptyOutput),
            "pi finished without output"
        );
        assert_eq!(
            agent_error(&AgentError::NonZeroExit { code: Some(1), stderr: "boom".into() }),
            "pi exited with code 1: boom"
        );
        assert_eq!(
            agent_error(&AgentError::SpawnFailed("nope".into())),
            "failed to start pi: nope"
        );
        assert_eq!(
            agent_error(&AgentError::Rpc { command: "set_model".into(), error: "unknown model".into() }),
            "pi rejected: unknown model"
        );
    }

    #[test]
    fn non_zero_exit_stderr_truncated() {
        let long = "x".repeat(400);
        let msg = agent_error(&AgentError::NonZeroExit { code: Some(2), stderr: long });
        assert!(msg.len() < 350);
        assert!(msg.starts_with("pi exited with code 2: "));
    }

    #[test]
    fn session_info_block() {
        let state = SessionState {
            session_id: "abc".into(),
            session_file: "/tmp/s.jsonl".into(),
            model: Some("deepseek/deepseek-v4-flash".into()),
            thinking: Some("high".into()),
        };
        let s = session_info(&state, None);
        assert!(s.contains("id=abc"));
        assert!(s.contains("file=/tmp/s.jsonl"));
        assert!(s.contains("model=deepseek/deepseek-v4-flash"));
        assert!(s.contains("thinking=high"));

        let stats = SessionStats {
            tokens_input: 10,
            tokens_output: 20,
            tokens_cache_read: 30,
            cost: Some(0.1234),
        };
        let s2 = session_info(&state, Some(&stats));
        assert!(s2.contains("tokens: in=10 out=20 cache=30 cost=0.1234"), "{s2}");
    }

    #[test]
    fn new_session_block() {
        let state = SessionState {
            session_id: "abc".into(),
            session_file: "/tmp/s.jsonl".into(),
            model: None,
            thinking: None,
        };
        let s = new_session_info(&state);
        assert!(s.starts_with("new session: id=abc"));
        assert!(s.contains("model=unknown"));
    }

    #[test]
    fn command_texts() {
        assert_eq!(new_ack(), "ok, next message will start a new session");
        assert_eq!(abort_ack(true, 0), "aborted running task");
        assert_eq!(abort_ack(false, 2), "nothing was running; cleared 2 queued messages");
        assert_eq!(abort_ack(false, 1), "nothing was running; cleared 1 queued message");
        assert_eq!(cleared_msg(1), "cleared 1 queued message");
        assert_eq!(queued(3), "queued (backlog: 3)");
        assert_eq!(commands_list(), "commands: /new /abort /session /model /thinking");
    }
}
