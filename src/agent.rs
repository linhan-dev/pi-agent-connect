//! Port for the pi CLI (the only external AI dependency).
//!
//! pico treats pi as a black box and talks to it only through its documented
//! CLI: print mode (`pi -c -p "…"`) for messages, and short-lived RPC mode
//! (`pi --mode rpc --continue`) for session/model/thinking commands.
//! There is intentionally NO dependency on pi's npm SDK.
//!
//! Protocol parsing lives in pure functions (`find_response_line`, `parse_*`)
//! so it can be unit-tested against canned pi output.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    SpawnFailed(String),
    NonZeroExit { code: Option<i32>, stderr: String },
    EmptyOutput,
    Rpc { command: String, error: String },
    RpcTimeout { command: String, seconds: u64 },
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionState {
    pub session_id: String,
    pub session_file: String,
    pub model: Option<String>,
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionStats {
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_cache_read: u64,
    pub cost: Option<f64>,
}

#[async_trait]
pub trait Agent: Send + Sync {
    /// Run one message in print mode. `fresh` = start a new session (no `--continue`).
    async fn run_prompt(&self, prompt: String, fresh: bool) -> Result<String, AgentError>;
    /// Abort the currently running operation (kills the active child process).
    async fn abort(&self);
    async fn get_state(&self) -> Result<SessionState, AgentError>;
    async fn get_session_stats(&self) -> Result<SessionStats, AgentError>;
    async fn set_model(&self, model_ref: &str) -> Result<(), AgentError>;
    async fn list_models(&self) -> Result<Vec<String>, AgentError>;
    async fn set_thinking(&self, level: &str) -> Result<(), AgentError>;
    async fn list_thinking_levels(&self) -> Result<Vec<String>, AgentError>;
}

// ── Real implementation: spawns the `pi` binary ─────────────────────────

pub struct RealAgent {
    pi_bin: String,
    cwd: PathBuf,
    /// PID of the active child (print or RPC), for abort().
    active_pid: Arc<Mutex<Option<u32>>>,
    rpc_timeout: Duration,
}

impl RealAgent {
    pub fn new(pi_bin: String, cwd: PathBuf) -> Self {
        RealAgent {
            pi_bin,
            cwd,
            active_pid: Arc::new(Mutex::new(None)),
            rpc_timeout: Duration::from_secs(15),
        }
    }

    async fn register_pid(&self, pid: Option<u32>) {
        if let Some(pid) = pid {
            *self.active_pid.lock().await = Some(pid);
        }
    }

    async fn clear_pid(&self, pid: u32) {
        let mut guard = self.active_pid.lock().await;
        if *guard == Some(pid) {
            *guard = None;
        }
    }

    /// One short-lived RPC call: spawn `pi --mode rpc --continue`, send one
    /// command, read the matching response line, exit.
    ///
    /// The stdin pipe is kept open until the response arrives: closing stdin
    /// early makes pi shut down before slow commands (e.g. `get_available_models`)
    /// finish.
    async fn rpc(&self, command: Value, expected: &str) -> Result<Value, AgentError> {
        let line = serde_json::to_string(&command)
            .map_err(|e| AgentError::Rpc { command: expected.into(), error: e.to_string() })?;
        let mut child = Command::new(&self.pi_bin)
            .args(["--mode", "rpc", "--continue"])
            .current_dir(&self.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| AgentError::SpawnFailed(e.to_string()))?;
        let pid = child.id();
        self.register_pid(pid).await;

        let mut stdin = child.stdin.take().expect("pi rpc stdin");
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|e| AgentError::Rpc { command: expected.into(), error: e.to_string() })?;

        // Stream stdout until the matching response line arrives (or timeout).
        let stdout = child.stdout.take().expect("pi rpc stdout");
        let mut reader = tokio::io::BufReader::new(stdout);
        let mut found: Option<String> = None;
        let read_result = tokio::time::timeout(self.rpc_timeout, async {
            let mut buf = String::new();
            loop {
                buf.clear();
                let n = reader
                    .read_line(&mut buf)
                    .await
                    .map_err(|e| AgentError::Rpc { command: expected.into(), error: e.to_string() })?;
                if n == 0 {
                    break; // EOF without a response
                }
                if let Some(line) = find_response_line(&buf, expected) {
                    found = Some(line.trim().to_string());
                    break;
                }
            }
            Ok::<(), AgentError>(())
        })
        .await;

        match read_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                self.abort().await;
                let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                if let Some(pid) = pid {
                    self.clear_pid(pid).await;
                }
                return Err(e);
            }
            Err(_) => {
                self.abort().await;
                let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                if let Some(pid) = pid {
                    self.clear_pid(pid).await;
                }
                return Err(AgentError::RpcTimeout { command: expected.into(), seconds: self.rpc_timeout.as_secs() });
            }
        }

        // Response found: close stdin so pi exits, then reap.
        drop(stdin);
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        if let Some(pid) = pid {
            self.clear_pid(pid).await;
        }

        let Some(resp) = found else {
            return Err(AgentError::Rpc {
                command: expected.into(),
                error: "no response received".into(),
            });
        };
        let value: Value = serde_json::from_str(&resp).map_err(|e| AgentError::Rpc {
            command: expected.into(),
            error: format!("bad response: {e}"),
        })?;
        if value.get("success").and_then(|s| s.as_bool()) != Some(true) {
            let err = value
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown error")
                .to_string();
            return Err(AgentError::Rpc { command: expected.into(), error: err });
        }
        Ok(value.get("data").cloned().unwrap_or(Value::Null))
    }
}

#[async_trait]
impl Agent for RealAgent {
    async fn run_prompt(&self, prompt: String, fresh: bool) -> Result<String, AgentError> {
        let mut args: Vec<String> = Vec::new();
        if !fresh {
            args.push("--continue".into());
        }
        args.push("-p".into());
        args.push(prompt.clone());

        let child = Command::new(&self.pi_bin)
            .args(&args)
            .current_dir(&self.cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| AgentError::SpawnFailed(e.to_string()))?;
        let pid = child.id();
        self.register_pid(pid).await;

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| AgentError::SpawnFailed(e.to_string()))?;
        if let Some(pid) = pid {
            self.clear_pid(pid).await;
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(AgentError::NonZeroExit { code: output.status.code(), stderr });
        }
        if stdout.is_empty() {
            return Err(AgentError::EmptyOutput);
        }
        Ok(stdout)
    }

    async fn abort(&self) {
        let pid = *self.active_pid.lock().await;
        if let Some(pid) = pid {
            kill_pid(pid, false);
            let active = self.active_pid.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                if *active.lock().await == Some(pid) {
                    kill_pid(pid, true);
                }
            });
        }
    }

    async fn get_state(&self) -> Result<SessionState, AgentError> {
        let data = self.rpc(json!({"type": "get_state"}), "get_state").await?;
        Ok(parse_session_state(&data))
    }

    async fn get_session_stats(&self) -> Result<SessionStats, AgentError> {
        let data = self.rpc(json!({"type": "get_session_stats"}), "get_session_stats").await?;
        Ok(parse_session_stats(&data))
    }

    async fn set_model(&self, model_ref: &str) -> Result<(), AgentError> {
        let (provider, model_id) = split_model_ref(model_ref)?;
        self.rpc(
            json!({"type": "set_model", "provider": provider, "modelId": model_id}),
            "set_model",
        )
        .await?;
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<String>, AgentError> {
        let data = self.rpc(json!({"type": "get_available_models"}), "get_available_models").await?;
        Ok(parse_model_list(&data))
    }

    async fn set_thinking(&self, level: &str) -> Result<(), AgentError> {
        self.rpc(json!({"type": "set_thinking_level", "level": level}), "set_thinking_level")
            .await?;
        Ok(())
    }

    async fn list_thinking_levels(&self) -> Result<Vec<String>, AgentError> {
        let data =
            self.rpc(json!({"type": "get_available_thinking_levels"}), "get_available_thinking_levels")
                .await?;
        Ok(parse_level_list(&data))
    }
}

// ── Pure protocol parsing (unit-tested with canned pi output) ───────────

/// Scan RPC output for the first `{"type":"response","command":<cmd>}` line,
/// skipping banner / non-JSON lines that pi may print before the response.
pub fn find_response_line<'a>(output: &'a str, command: &str) -> Option<&'a str> {
    output.lines().find(|line| {
        let line = line.trim();
        if !line.starts_with('{') {
            return false;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return false;
        };
        v.get("type").and_then(|t| t.as_str()) == Some("response")
            && v.get("command").and_then(|c| c.as_str()) == Some(command)
    })
}

pub fn parse_session_state(v: &Value) -> SessionState {
    SessionState {
        session_id: v.get("sessionId").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        session_file: v.get("sessionFile").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        model: v
            .get("model")
            .and_then(|m| m.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                v.get("model").and_then(|m| m.get("provider")).and_then(|p| p.as_str()).map(|p| {
                    let id = v.get("model").and_then(|m| m.get("id")).and_then(|i| i.as_str()).unwrap_or("");
                    format!("{p}/{id}")
                })
            }),
        thinking: v.get("thinkingLevel").and_then(|x| x.as_str()).map(|s| s.to_string()),
    }
}

pub fn parse_session_stats(v: &Value) -> SessionStats {
    let num = |path: &str| -> u64 {
        v.pointer(path).and_then(|x| x.as_u64()).unwrap_or(0)
    };
    SessionStats {
        tokens_input: num("/tokens/input"),
        tokens_output: num("/tokens/output"),
        tokens_cache_read: num("/tokens/cacheRead"),
        cost: v.get("cost").and_then(|c| c.as_f64()),
    }
}

/// `data.models` → list of `provider/id` refs.
pub fn parse_model_list(v: &Value) -> Vec<String> {
    v.get("models")
        .and_then(|m| m.as_array())
        .map(|models| {
            models
                .iter()
                .filter_map(|m| {
                    let id = m.get("id").and_then(|x| x.as_str())?;
                    let provider = m.get("provider").and_then(|x| x.as_str()).unwrap_or("?");
                    Some(format!("{provider}/{id}"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `data.levels` → list of levels.
pub fn parse_level_list(v: &Value) -> Vec<String> {
    v.get("levels")
        .and_then(|l| l.as_array())
        .map(|levels| {
            levels
                .iter()
                .filter_map(|l| l.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Split `provider/modelId` on the last `/`.
pub fn split_model_ref(model_ref: &str) -> Result<(String, String), AgentError> {
    match model_ref.rsplit_once('/') {
        Some((p, m)) if !p.is_empty() && !m.is_empty() => Ok((p.to_string(), m.to_string())),
        _ => Err(AgentError::Rpc {
            command: "set_model".into(),
            error: format!("invalid model ref '{model_ref}', expected provider/modelId"),
        }),
    }
}

fn kill_pid(pid: u32, force: bool) {
    #[cfg(unix)]
    {
        let sig = if force { "KILL" } else { "TERM" };
        let _ = std::process::Command::new("kill").args([format!("-{sig}"), pid.to_string()]).spawn();
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_response_skips_banners() {
        let output = "\
\u{1b}[32mModel catalog loaded\u{1b}[0m
some random line
{\"type\":\"response\",\"command\":\"get_state\",\"success\":true,\"data\":{\"sessionId\":\"abc\"}}
trailing junk
";
        let line = find_response_line(output, "get_state").unwrap();
        let v: Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["data"]["sessionId"], "abc");
    }

    #[test]
    fn find_response_matches_command() {
        let output = "{\"type\":\"response\",\"command\":\"set_model\",\"success\":true,\"data\":{}}";
        assert!(find_response_line(output, "set_model").is_some());
        assert!(find_response_line(output, "get_state").is_none());
    }

    #[test]
    fn parse_state_fields() {
        let v = json!({
            "sessionFile": "/tmp/s.jsonl",
            "sessionId": "abc123",
            "model": {"provider": "deepseek", "id": "deepseek-v4-flash"},
            "thinkingLevel": "high",
            "messageCount": 5
        });
        let s = parse_session_state(&v);
        assert_eq!(s.session_id, "abc123");
        assert_eq!(s.session_file, "/tmp/s.jsonl");
        assert_eq!(s.model.as_deref(), Some("deepseek/deepseek-v4-flash"));
        assert_eq!(s.thinking.as_deref(), Some("high"));
    }

    #[test]
    fn parse_state_null_model() {
        let v = json!({"sessionId": "x", "model": null, "thinkingLevel": "off"});
        let s = parse_session_state(&v);
        assert_eq!(s.model, None);
        assert_eq!(s.thinking.as_deref(), Some("off"));
    }

    #[test]
    fn parse_stats() {
        let v = json!({
            "tokens": {"input": 1, "output": 2, "cacheRead": 3, "cacheWrite": 4, "total": 10},
            "cost": 0.5,
            "contextUsage": {"tokens": 60, "contextWindow": 200, "percent": 30}
        });
        let s = parse_session_stats(&v);
        assert_eq!((s.tokens_input, s.tokens_output, s.tokens_cache_read), (1, 2, 3));
        assert_eq!(s.cost, Some(0.5));
    }

    #[test]
    fn parse_model_and_level_lists() {
        let models = json!({"models": [
            {"provider": "deepseek", "id": "deepseek-v4-flash"},
            {"provider": "qiuming", "id": "gpt-5.6-sol"},
            {"provider": "x", "id": "y", "name": "Y"}
        ]});
        assert_eq!(parse_model_list(&models), vec!["deepseek/deepseek-v4-flash", "qiuming/gpt-5.6-sol", "x/y"]);

        let levels = json!({"levels": ["off", "low", "high"]});
        assert_eq!(parse_level_list(&levels), vec!["off", "low", "high"]);
    }

    #[test]
    fn split_ref() {
        assert_eq!(
            split_model_ref("deepseek/deepseek-v4-flash").unwrap(),
            ("deepseek".to_string(), "deepseek-v4-flash".to_string())
        );
        assert!(split_model_ref("no-provider").is_err());
        assert!(split_model_ref("a/").is_err());
        assert!(split_model_ref("/b").is_err());
    }

    #[tokio::test]
    #[ignore = "manual smoke: requires the real pi binary (no model calls)"]
    async fn smoke_real_agent_rpc() {
        let dir = std::env::temp_dir().join("pico-smoke");
        std::fs::create_dir_all(&dir).expect("create smoke dir");
        let agent = RealAgent::new("pi".into(), dir);
        let state = agent.get_state().await.expect("get_state");
        assert!(!state.session_id.is_empty(), "session id empty");
        assert!(!state.session_file.is_empty(), "session file empty");
        println!("get_state -> id={} model={:?} thinking={:?}", state.session_id, state.model, state.thinking);

        let stats = agent.get_session_stats().await.expect("get_session_stats");
        println!("stats -> in={} out={} cost={:?}", stats.tokens_input, stats.tokens_output, stats.cost);

        let models = agent.list_models().await.expect("get_available_models");
        assert!(!models.is_empty(), "model list empty");
        println!("models -> {models:?}");

        let levels = agent.list_thinking_levels().await.expect("get_available_thinking_levels");
        assert!(!levels.is_empty(), "levels empty");
        println!("levels -> {levels:?}");

        // set_model / set_thinking are intentionally not exercised here: they
        // would mutate the user's real ~/.pi/agent/settings.json.
    }
}
