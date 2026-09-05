//! Configuration: everything comes from environment variables, no config file.
//!
//! - `PIAC_DISCORD_TOKEN` (required, fail fast)
//! - `PIAC_DISCORD_ALLOWED_USER_ID` (single Discord user id; empty/missing = lockdown mode)
//! - `PIAC_CWD` (pi working directory, default: launch cwd / `pwd`)
//! - `PIAC_COMMAND_PREFIX` (optional, single symbol, default `.`)
//! - `PIAC_PROMPT_TIMEOUT` (optional, seconds per prompt run, default: 1 hour / 3600s)

use std::path::PathBuf;
use std::time::Duration;

/// Default command prefix. `.` is safe on every mainstream IM: `/`-prefixed
/// messages are intercepted as slash commands by Discord/Slack/Telegram etc.
/// and `@`-prefixed ones become mentions.
pub const DEFAULT_COMMAND_PREFIX: char = '.';

/// Default per-prompt timeout: one hour. A single pi run may take this long
/// before it is aborted. The worker is single-consumer, so a hung run stalls
/// everything queued behind it — the cap is the gateway's availability backstop.
pub const DEFAULT_PROMPT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub discord_token: String,
    /// Single allowed user. `None` = lockdown mode (start, audit-log, process nothing).
    pub allowed_user: Option<String>,
    pub cwd: PathBuf,
    /// Command prefix character, defaults to `.`. Should almost never be set.
    pub command_prefix: char,
    /// Max wall-clock time a single prompt's pi run may take before it is aborted.
    pub prompt_timeout: Duration,
}

impl Config {
    /// Load from the process environment. Fails fast if the token is missing.
    pub fn load() -> Result<Config, String> {
        let token = std::env::var("PIAC_DISCORD_TOKEN").map_err(|_| {
            "PIAC_DISCORD_TOKEN is required (set it in the environment)".to_string()
        })?;
        let allowed_user = parse_allowed_user(std::env::var("PIAC_DISCORD_ALLOWED_USER_ID").ok());
        let cwd = std::env::var("PIAC_CWD")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let command_prefix = parse_command_prefix(std::env::var("PIAC_COMMAND_PREFIX").ok())?;
        let prompt_timeout = parse_prompt_timeout(std::env::var("PIAC_PROMPT_TIMEOUT").ok())?;
        Ok(Config {
            discord_token: token,
            allowed_user,
            cwd,
            command_prefix,
            prompt_timeout,
        })
    }

    /// Human-readable startup summary of the loaded configuration.
    ///
    /// Secrets are masked (only the last 4 characters are shown) so the log can
    /// confirm the environment made it in without leaking the token.
    pub fn summary(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{} v{} configuration (environment)",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        );
        let _ = writeln!(
            out,
            "  {:<32} {}  (required)",
            "PIAC_DISCORD_TOKEN",
            mask_secret(&self.discord_token)
        );
        match &self.allowed_user {
            Some(user) => {
                let _ = writeln!(
                    out,
                    "  {:<32} {user}  (normal mode)",
                    "PIAC_DISCORD_ALLOWED_USER_ID"
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "  {:<32} (unset)  (lockdown mode)",
                    "PIAC_DISCORD_ALLOWED_USER_ID"
                );
            }
        }
        let cwd_source = if env_has("PIAC_CWD") {
            "from PIAC_CWD"
        } else {
            "defaulted to launch cwd"
        };
        let _ = writeln!(
            out,
            "  {:<32} {}  ({cwd_source})",
            "PIAC_CWD",
            self.cwd.display()
        );
        let _ = writeln!(
            out,
            "  {:<32} {}  (optional, default: '.')",
            "PIAC_COMMAND_PREFIX", self.command_prefix
        );
        let timeout_source = if env_has("PIAC_PROMPT_TIMEOUT") {
            "from PIAC_PROMPT_TIMEOUT"
        } else {
            "defaulted to 1 hour (3600s)"
        };
        let _ = writeln!(
            out,
            "  {:<32} {}s  ({timeout_source})",
            "PIAC_PROMPT_TIMEOUT",
            self.prompt_timeout.as_secs()
        );
        out
    }
}

/// Parse the optional command prefix. Unset/empty = `.`. Must be a single
/// symbol: `/` (slash-command interception) and `@` (mention) are rejected,
/// as are alphanumerics and whitespace.
pub fn parse_command_prefix(raw: Option<String>) -> Result<char, String> {
    let Some(s) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(DEFAULT_COMMAND_PREFIX);
    };
    let mut chars = s.chars();
    let Some(c) = chars.next() else {
        return Ok(DEFAULT_COMMAND_PREFIX);
    };
    if chars.next().is_some() {
        return Err(format!(
            "PIAC_COMMAND_PREFIX must be a single character, got: {s}"
        ));
    }
    if c == '/' {
        return Err(
            "PIAC_COMMAND_PREFIX cannot be '/': Discord/Slack intercept it as slash commands"
                .to_string(),
        );
    }
    if c == '@' {
        return Err(
            "PIAC_COMMAND_PREFIX cannot be '@': it is a mention on every platform".to_string(),
        );
    }
    if c.is_alphanumeric() || c.is_whitespace() {
        return Err(format!("PIAC_COMMAND_PREFIX must be a symbol, got: {c}"));
    }
    Ok(c)
}

/// Parse the optional per-prompt timeout (seconds). Unset/empty/whitespace =
/// default (1 hour). Must be a positive whole number of seconds; 0 (unlimited)
/// is rejected on purpose — a run with no cap can hang the single worker queue
/// indefinitely.
pub fn parse_prompt_timeout(raw: Option<String>) -> Result<Duration, String> {
    let Some(s) = raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
        return Ok(DEFAULT_PROMPT_TIMEOUT);
    };
    match s.parse::<u64>() {
        Ok(0) | Err(_) => Err(format!(
            "PIAC_PROMPT_TIMEOUT must be a positive integer number of seconds, got: {s}"
        )),
        Ok(secs) => Ok(Duration::from_secs(secs)),
    }
}

/// Show only the last 4 characters of a secret; nothing leaks otherwise.
fn mask_secret(secret: &str) -> String {
    if secret.is_empty() {
        return "****".to_string();
    }
    let tail: String = secret
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("****{tail}")
}

fn env_has(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .is_some()
}

/// Trim and normalize the optional whitelist value. Empty = no whitelist (lockdown).
pub fn parse_allowed_user(raw: Option<String>) -> Option<String> {
    raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env vars are process-global and tests run in parallel threads; serialize
    /// every test that reads or mutates the process environment through it.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn empty_allowed_user_means_lockdown() {
        assert_eq!(parse_allowed_user(None), None);
        assert_eq!(parse_allowed_user(Some("  ".to_string())), None);
        assert_eq!(
            parse_allowed_user(Some(" 123 ".to_string())),
            Some("123".to_string())
        );
    }

    #[test]
    fn mask_secret_keeps_only_tail() {
        assert_eq!(mask_secret("abcdefgh"), "****efgh");
        assert_eq!(mask_secret("ab"), "****ab");
        assert_eq!(mask_secret(""), "****");
    }

    #[test]
    fn command_prefix_default_and_validation() {
        // Unset / empty / whitespace → default `.`.
        assert_eq!(parse_command_prefix(None), Ok('.'));
        assert_eq!(parse_command_prefix(Some("  ".to_string())), Ok('.'));
        // Valid symbol accepted (trimmed).
        assert_eq!(parse_command_prefix(Some(" ! ".to_string())), Ok('!'));
        assert_eq!(parse_command_prefix(Some("-".to_string())), Ok('-'));
        // Rejected: slash, mention, alphanumerics, multi-char.
        // (Pure whitespace is trimmed away first and falls back to the default.)
        assert!(parse_command_prefix(Some("/".to_string())).is_err());
        assert!(parse_command_prefix(Some("@".to_string())).is_err());
        assert!(parse_command_prefix(Some("a".to_string())).is_err());
        assert!(parse_command_prefix(Some("1".to_string())).is_err());
        assert!(parse_command_prefix(Some("--".to_string())).is_err());
    }

    #[test]
    fn summary_masks_token_and_shows_user() {
        let _guard = ENV_LOCK.lock().unwrap();
        let c = Config {
            discord_token: "super-secret-token".to_string(),
            allowed_user: Some("123456789".to_string()),
            cwd: PathBuf::from("/tmp/pi"),
            command_prefix: '.',
            prompt_timeout: Duration::from_secs(3600),
        };
        let s = c.summary();
        assert!(
            !s.contains("super-secret-token"),
            "token must not appear in full"
        );
        assert!(s.contains("****oken"), "only the tail may be shown");
        assert!(s.contains("123456789"));
        assert!(s.contains("PIAC_DISCORD_TOKEN"));
        assert!(s.contains("PIAC_DISCORD_ALLOWED_USER_ID"));
        assert!(s.contains("PIAC_CWD"));
        assert!(s.contains("PIAC_COMMAND_PREFIX"));
        assert!(s.contains("PIAC_PROMPT_TIMEOUT"));
        assert!(s.contains("3600s  (defaulted to 1 hour (3600s))"));
        assert!(s.contains(".  (optional, default: '.')"));
    }

    #[test]
    fn prompt_timeout_default_and_validation() {
        // Unset / empty / whitespace → default 1 hour.
        assert_eq!(parse_prompt_timeout(None), Ok(DEFAULT_PROMPT_TIMEOUT));
        assert_eq!(
            parse_prompt_timeout(Some("  ".to_string())),
            Ok(DEFAULT_PROMPT_TIMEOUT)
        );
        assert_eq!(
            DEFAULT_PROMPT_TIMEOUT,
            Duration::from_secs(60 * 60),
            "default must stay 1 hour"
        );
        // Valid positive whole seconds (trimmed).
        assert_eq!(
            parse_prompt_timeout(Some("3600".to_string())),
            Ok(Duration::from_secs(3600))
        );
        assert_eq!(
            parse_prompt_timeout(Some("5".to_string())),
            Ok(Duration::from_secs(5))
        );
        assert_eq!(
            parse_prompt_timeout(Some(" 7200 ".to_string())),
            Ok(Duration::from_secs(7200))
        );
        // Rejected: zero (unlimited), non-numeric, negative, fractional, overflow.
        for bad in ["0", "abc", "-5", "1.5", "99999999999999999999"] {
            let err = parse_prompt_timeout(Some(bad.to_string())).unwrap_err();
            assert!(
                err.contains("positive integer number of seconds"),
                "unexpected error for {bad:?}: {err}"
            );
            assert!(err.contains(bad), "error should echo the raw value: {err}");
        }
    }

    #[test]
    fn load_missing_token_fails() {
        let _guard = ENV_LOCK.lock().unwrap();
        // No PIAC_DISCORD_TOKEN in a clean-ish env: use a scoped removal.
        unsafe { std::env::remove_var("PIAC_DISCORD_TOKEN") };
        assert!(Config::load().is_err());
    }

    #[test]
    fn load_bad_prefix_fails_fast() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("PIAC_DISCORD_TOKEN", "tok");
            std::env::set_var("PIAC_COMMAND_PREFIX", "//");
        }
        assert!(Config::load().is_err());
        unsafe {
            std::env::remove_var("PIAC_COMMAND_PREFIX");
        }
    }

    #[test]
    fn load_bad_timeout_fails_fast() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("PIAC_DISCORD_TOKEN", "tok");
            std::env::set_var("PIAC_PROMPT_TIMEOUT", "0");
        }
        assert!(Config::load().is_err());
        unsafe {
            std::env::remove_var("PIAC_PROMPT_TIMEOUT");
        }
    }
}
