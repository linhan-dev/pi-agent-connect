//! Command parsing (pure). Plain-text Discord commands, no registered slash commands.
//!
//! The `/` prefix cannot be used on Discord: messages starting with `/` are
//! intercepted by Discord's own slash-command UI (autocomplete, or the
//! command handler of any other registered bot), so they never arrive here
//! as plain text. The default command prefix is therefore `.`, overridable
//! via `PIAC_COMMAND_PREFIX`:
//!
//! Full names: `.new .abort .session .model .thinking`
//! Short aliases: `.n .a .s .m .t`

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    New,
    Abort,
    Session,
    Model(Option<String>),
    Thinking(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseResult {
    Command(Command),
    /// Starts with `.` but is not a known command.
    Unknown,
    /// Plain message text.
    NotCommand,
}

/// Parse a message. `prefix` is the command prefix (default `.`); any message
/// not starting with it is plain text.
pub fn parse(content: &str, prefix: char) -> ParseResult {
    let trimmed = content.trim();
    let mut chars = trimmed.chars();
    if chars.next() != Some(prefix) {
        return ParseResult::NotCommand;
    }
    let rest = chars.as_str();
    let (word, rest) = match rest.split_once(char::is_whitespace) {
        Some((w, r)) => (w, Some(r.trim())),
        None => (rest, None),
    };
    let arg = |rest: Option<&str>| -> Option<String> {
        rest.map(|s| s.to_string()).filter(|s| !s.is_empty())
    };
    let command = match word.to_ascii_lowercase().as_str() {
        "new" | "n" => Command::New,
        "abort" | "a" => Command::Abort,
        "session" | "s" => Command::Session,
        "model" | "m" => Command::Model(arg(rest)),
        "thinking" | "t" => Command::Thinking(arg(rest)),
        _ => return ParseResult::Unknown,
    };
    ParseResult::Command(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_names() {
        assert_eq!(parse(".new", '.'), ParseResult::Command(Command::New));
        assert_eq!(parse(".abort", '.'), ParseResult::Command(Command::Abort));
        assert_eq!(
            parse(".session", '.'),
            ParseResult::Command(Command::Session)
        );
        assert_eq!(
            parse(".model", '.'),
            ParseResult::Command(Command::Model(None))
        );
        assert_eq!(
            parse(".thinking", '.'),
            ParseResult::Command(Command::Thinking(None))
        );
    }

    #[test]
    fn short_aliases() {
        assert_eq!(parse(".n", '.'), ParseResult::Command(Command::New));
        assert_eq!(parse(".a", '.'), ParseResult::Command(Command::Abort));
        assert_eq!(parse(".s", '.'), ParseResult::Command(Command::Session));
        assert_eq!(parse(".m", '.'), ParseResult::Command(Command::Model(None)));
        assert_eq!(
            parse(".t", '.'),
            ParseResult::Command(Command::Thinking(None))
        );
    }

    #[test]
    fn args() {
        assert_eq!(
            parse(".model deepseek/deepseek-v4-flash", '.'),
            ParseResult::Command(Command::Model(Some("deepseek/deepseek-v4-flash".into())))
        );
        assert_eq!(
            parse(".model   ", '.'),
            ParseResult::Command(Command::Model(None))
        );
        assert_eq!(
            parse(".thinking high", '.'),
            ParseResult::Command(Command::Thinking(Some("high".into())))
        );
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(parse(".NEW", '.'), ParseResult::Command(Command::New));
        assert_eq!(
            parse(".Model gpt/x", '.'),
            ParseResult::Command(Command::Model(Some("gpt/x".into())))
        );
    }

    #[test]
    fn unknown_and_plain() {
        assert_eq!(parse(".foo bar", '.'), ParseResult::Unknown);
        assert_eq!(parse(".", '.'), ParseResult::Unknown);
        assert_eq!(parse("hello there", '.'), ParseResult::NotCommand);
        assert_eq!(parse("", '.'), ParseResult::NotCommand);
    }

    #[test]
    fn custom_prefix() {
        assert_eq!(parse("!new", '!'), ParseResult::Command(Command::New));
        assert_eq!(
            parse("!model gpt/x", '!'),
            ParseResult::Command(Command::Model(Some("gpt/x".into())))
        );
        // With `!` as prefix, `.new` is plain text and vice versa.
        assert_eq!(parse(".new", '!'), ParseResult::NotCommand);
        assert_eq!(parse("!wat", '!'), ParseResult::Unknown);
    }
}
