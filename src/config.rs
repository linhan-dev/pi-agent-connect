//! Configuration: everything comes from environment variables, no config file.
//!
//! - `PIAC_DISCORD_TOKEN` (required, fail fast)
//! - `PIAC_DISCORD_ALLOWED_USER_ID` (single Discord user id; empty/missing = lockdown mode)
//! - `PIAC_CWD` (pi working directory, default: launch cwd / `pwd`)

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
        let token = std::env::var("PIAC_DISCORD_TOKEN").map_err(|_| {
            "PIAC_DISCORD_TOKEN is required (set it in the environment)".to_string()
        })?;
        let allowed_user = parse_allowed_user(std::env::var("PIAC_DISCORD_ALLOWED_USER_ID").ok());
        let cwd = std::env::var("PIAC_CWD")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        Ok(Config {
            discord_token: token,
            allowed_user,
            cwd,
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
        out
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
    fn summary_masks_token_and_shows_user() {
        let c = Config {
            discord_token: "super-secret-token".to_string(),
            allowed_user: Some("123456789".to_string()),
            cwd: PathBuf::from("/tmp/pi"),
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
    }

    #[test]
    fn load_missing_token_fails() {
        // No PIAC_DISCORD_TOKEN in a clean-ish env: use a scoped removal.
        unsafe { std::env::remove_var("PIAC_DISCORD_TOKEN") };
        assert!(Config::load().is_err());
    }
}
