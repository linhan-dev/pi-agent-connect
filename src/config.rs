//! Configuration: everything comes from environment variables, no config file.
//!
//! - `PI_AGENT_CONNECT_DISCORD_TOKEN` (required, fail fast)
//! - `PI_AGENT_CONNECT_ALLOWED_USER` (single Discord user id; empty/missing = lockdown mode)
//! - `PI_AGENT_CONNECT_CWD` (pi working directory, default: `$HOME`)

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub discord_token: String,
    /// Single allowed user. `None` = lockdown mode (start, audit-log, process nothing).
    pub allowed_user: Option<String>,
    pub cwd: PathBuf,
}

impl Config {
    /// Load from the process environment. Fails fast if the token is missing.
    pub fn load() -> Result<Config, String> {
        let token = std::env::var("PI_AGENT_CONNECT_DISCORD_TOKEN")
            .map_err(|_| "PI_AGENT_CONNECT_DISCORD_TOKEN is required (set it in the environment)".to_string())?;
        let allowed_user = parse_allowed_user(std::env::var("PI_AGENT_CONNECT_ALLOWED_USER").ok());
        let cwd = std::env::var("PI_AGENT_CONNECT_CWD")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(home_dir);
        Ok(Config {
            discord_token: token,
            allowed_user,
            cwd,
        })
    }
}

/// Trim and normalize the optional whitelist value. Empty = no whitelist (lockdown).
pub fn parse_allowed_user(raw: Option<String>) -> Option<String> {
    raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allowed_user_means_lockdown() {
        assert_eq!(parse_allowed_user(None), None);
        assert_eq!(parse_allowed_user(Some("  ".to_string())), None);
        assert_eq!(parse_allowed_user(Some(" 123 ".to_string())), Some("123".to_string()));
    }

    #[test]
    fn load_missing_token_fails() {
        // No PI_AGENT_CONNECT_DISCORD_TOKEN in a clean-ish env: use a scoped removal.
        unsafe { std::env::remove_var("PI_AGENT_CONNECT_DISCORD_TOKEN") };
        assert!(Config::load().is_err());
    }
}
