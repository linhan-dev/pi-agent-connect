# pico

Minimal Discord DM gateway for the [pi coding agent](https://pi.dev). Rust single
binary, foreground, one command. No database, no config file, no daemon, no own
session management.

## Principles

- **pico does not manage pi's state.** Sessions, model and thinking settings all
  belong to pi (`~/.pi/agent/`). pico only spawns `pi` and speaks its documented
  CLI (print mode for messages, short-lived RPC for commands).
- **Zero dependency on pi's npm SDK.** Only the `pi` binary on `PATH` is used.
- **System messages are prefixed with `[pico]`** (English, no emoji); pi replies
  are forwarded as-is.
- **Reply/quote metadata on Discord messages is ignored**, matching the pi
  terminal experience.

## Configuration (environment variables only)

| Variable             | Required | Meaning                                             |
|----------------------|----------|-----------------------------------------------------|
| `PICO_DISCORD_TOKEN` | yes      | Discord bot token; missing = refuse to start        |
| `PICO_ALLOWED_USER`  | no       | Single Discord user id. Empty = **lockdown mode** (start, audit-log every message to stdout, process nothing) |
| `PICO_CWD`           | no       | pi working directory (default: `$HOME`)             |

Model and thinking level are NOT configurable here: pi reads its own
`~/.pi/agent/settings.json`.

## Run

```bash
cd ~/projects/pico
export PICO_DISCORD_TOKEN="..."        # required
export PICO_ALLOWED_USER="1234567890"  # find your own id: run once in lockdown, DM the bot, read the audit log
export PICO_CWD="/path/to/workdir"     # optional

cargo run --release
# or build once: cargo build --release && ./target/release/pico
```

SIGINT / SIGTERM shut down cleanly (abort the running pi task, announce,
disconnect).

## Commands (plain text, no registered slash commands)

| Command          | Alias | Action                                                          |
|------------------|-------|-----------------------------------------------------------------|
| `/new`           | `/n`  | Abort task, clear queue; next message starts a fresh session    |
| `/abort`         | `/a`  | Abort the running task, clear queue                             |
| `/session`       | `/s`  | Session id / file / model / thinking / tokens via pi RPC        |
| `/model <ref>`   | `/m`  | Switch model (`provider/modelId`, persists in pi settings)      |
| `/model`         |       | List available models                                           |
| `/thinking <lvl>`| `/t`  | Set thinking level (persists in pi settings)                    |
| `/thinking`      |       | List available levels                                           |

Unknown `/...` replies with the command list. Attachments are rejected with an
error (v1: no file transfer).

## Architecture

```
Discord events ──► Router (pure) ──► Worker (single consumer, in-memory queue)
                                       ├─ print mode:  pi -c -p "…"
                                       └─ short RPC:   pi --mode rpc --continue
```

Business logic depends only on the `Agent` and `Chat` traits (ports). Tests
inject in-memory fakes — no network, no real pi.

## Testing

```bash
cargo test                          # 44 unit tests, no external deps
cargo test -- --ignored             # manual smoke against real pi (control RPC only, no model calls, no config mutation)
```

Manual smoke checklist (requires a real token + pi on PATH): start, DM the bot,
verify startup message, plain message round-trip, `/new` background info,
`/session`, `/model`, `/thinking`, `/abort`, Ctrl+C clean exit, restart resumes
the same session.
