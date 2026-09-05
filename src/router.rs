//! Message classification (pure logic). Decides what happens to each
//! incoming Discord message before it touches the worker queue.

use crate::commands::{Command, ParseResult, parse};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessage {
    pub author_id: String,
    pub author_name: String,
    pub is_bot: bool,
    pub has_attachments: bool,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditReason {
    /// Whitelist not configured: everything is logged and ignored.
    Lockdown,
    /// Author is not the allowed user.
    NotWhitelisted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Silently drop (bot message, non-whitelisted, lockdown, empty content).
    Drop,
    RejectAttachments,
    UnknownCommand,
    Command(Command),
    Prompt(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub decision: Decision,
    pub audit: Option<AuditReason>,
}

pub fn classify(
    allowed_user: Option<&str>,
    bot_id: &str,
    prefix: char,
    msg: &IncomingMessage,
) -> Classification {
    if msg.is_bot {
        return Classification {
            decision: Decision::Drop,
            audit: None,
        };
    }
    match allowed_user {
        None => {
            return Classification {
                decision: Decision::Drop,
                audit: Some(AuditReason::Lockdown),
            };
        }
        Some(user) if user != msg.author_id => {
            return Classification {
                decision: Decision::Drop,
                audit: Some(AuditReason::NotWhitelisted),
            };
        }
        Some(_) => {}
    }
    if msg.has_attachments {
        return Classification {
            decision: Decision::RejectAttachments,
            audit: None,
        };
    }
    let content = strip_self_mentions(&msg.content, bot_id);
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Classification {
            decision: Decision::Drop,
            audit: None,
        };
    }
    match parse(trimmed, prefix) {
        ParseResult::Command(c) => Classification {
            decision: Decision::Command(c),
            audit: None,
        },
        ParseResult::Unknown => Classification {
            decision: Decision::UnknownCommand,
            audit: None,
        },
        ParseResult::NotCommand => Classification {
            decision: Decision::Prompt(trimmed.to_string()),
            audit: None,
        },
    }
}

/// Remove the bot's own mentions (`<@id>` / `<@!id>`) from message content.
pub fn strip_self_mentions(content: &str, bot_id: &str) -> String {
    let mut s = content.to_string();
    s = s.replace(&format!("<@!{bot_id}>"), "");
    s = s.replace(&format!("<@{bot_id}>"), "");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(author: &str, content: &str) -> IncomingMessage {
        IncomingMessage {
            author_id: author.to_string(),
            author_name: "n".to_string(),
            is_bot: false,
            has_attachments: false,
            content: content.to_string(),
        }
    }

    fn classify_dot(allowed: Option<&str>, bot: &str, m: &IncomingMessage) -> Classification {
        classify(allowed, bot, '.', m)
    }

    #[test]
    fn bot_messages_dropped_silently() {
        let mut m = msg("123", "hello");
        m.is_bot = true;
        let c = classify_dot(Some("123"), "9", &m);
        assert_eq!(c.decision, Decision::Drop);
        assert_eq!(c.audit, None);
    }

    #[test]
    fn lockdown_drops_everything_with_audit() {
        let c = classify_dot(None, "9", &msg("999", "hello"));
        assert_eq!(c.decision, Decision::Drop);
        assert_eq!(c.audit, Some(AuditReason::Lockdown));
    }

    #[test]
    fn non_whitelisted_drops_with_audit() {
        let c = classify_dot(Some("123"), "9", &msg("456", "hello"));
        assert_eq!(c.decision, Decision::Drop);
        assert_eq!(c.audit, Some(AuditReason::NotWhitelisted));
    }

    #[test]
    fn whitelisted_message_routes_to_prompt() {
        let c = classify_dot(Some("123"), "9", &msg("123", "  hello there  "));
        assert_eq!(c.decision, Decision::Prompt("hello there".to_string()));
        assert_eq!(c.audit, None);
    }

    #[test]
    fn attachments_rejected_even_with_text() {
        let mut m = msg("123", "fix this file");
        m.has_attachments = true;
        let c = classify_dot(Some("123"), "9", &m);
        assert_eq!(c.decision, Decision::RejectAttachments);
    }

    #[test]
    fn commands_and_unknown() {
        assert_eq!(
            classify_dot(Some("123"), "9", &msg("123", ".new")).decision,
            Decision::Command(Command::New)
        );
        assert_eq!(
            classify_dot(Some("123"), "9", &msg("123", ".n")).decision,
            Decision::Command(Command::New)
        );
        assert_eq!(
            classify_dot(Some("123"), "9", &msg("123", ".wat")).decision,
            Decision::UnknownCommand
        );
    }

    #[test]
    fn mention_only_message_drops() {
        // A message consisting only of a bot mention becomes empty → drop.
        let c = classify_dot(Some("123"), "9", &msg("123", "<@9>"));
        assert_eq!(c.decision, Decision::Drop);
    }

    #[test]
    fn self_mentions_stripped_from_prompt() {
        let c = classify_dot(Some("123"), "9", &msg("123", "<@9> <@!9> summarize"));
        assert_eq!(c.decision, Decision::Prompt("summarize".to_string()));
    }

    #[test]
    fn strip_mentions() {
        assert_eq!(strip_self_mentions("<@9> hi <@!9>", "9"), " hi ");
        assert_eq!(strip_self_mentions("hi <@8>", "9"), "hi <@8>"); // other user untouched
    }
}
