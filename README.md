# pi-agent-connect

Minimal Discord DM gateway for the [pi coding agent](https://pi.dev). Rust single
binary, foreground, one command. No database, no config file, no daemon, no own
session management.

## Principles

- **pi-agent-connect does not manage pi's state.** Sessions, model and thinking
  settings all belong to pi (`~/.pi/agent/`). It only spawns `pi` and speaks its
  documented CLI (print mode for messages, short-lived RPC for commands).
- **Zero dependency on pi's npm SDK.** Only the `pi` binary on `PATH` is used.
- **System messages are prefixed with `[pi-agent-connect]`** (English, no emoji);
  pi replies are forwarded as-is.
- **Reply/quote metadata on Discord messages is ignored**, matching the pi
  terminal experience.

## Configuration (environment variables only)

| Variable                           | Required | Meaning                                             |
|------------------------------------|----------|-----------------------------------------------------|
| `PIAC_DISCORD_TOKEN`           | yes      | Discord bot token; missing = refuse to start        |
| `PIAC_DISCORD_ALLOWED_USER_ID` | no       | Single Discord user id. Empty = **lockdown mode** (start, audit-log every message to stdout, process nothing) |
| `PIAC_CWD`                     | no       | pi working directory (default: launch cwd / `pwd`)  |

Model and thinking level are NOT configurable here: pi reads its own
`~/.pi/agent/settings.json`.

On startup the binary logs a readable, multi-line summary of the loaded
configuration (token masked to its last 4 characters), so you can confirm in
the logs whether the environment variables actually reached the process.

## Run

```bash
cd ~/projects/pi-agent-connect
export PIAC_DISCORD_TOKEN="..."                    # required
export PIAC_DISCORD_ALLOWED_USER_ID="1234567890"    # find your own id: run once in lockdown, DM the bot, read the audit log
export PIAC_CWD="/path/to/workdir"                 # optional

cargo run --release
# or build once: cargo build --release && ./target/release/pi-agent-connect
```

SIGINT / SIGTERM shut down cleanly (abort the running pi task, announce,
disconnect).

### Install from Homebrew (macOS)

```bash
brew tap linhan-dev/pi-agent-connect
brew install pi-agent-connect
pi-agent-connect --version
```

The tap (`linhan-dev/homebrew-pi-agent-connect`) tracks the GitHub releases:
tagging `vX.Y.Z` in this repo builds binaries and publishes a release, then
dispatches an event to the tap which updates its formula immediately (requires
the `TAP_TOKEN` secret; see below). A Homebrew tap must live in a public
repository.

To enable automatic tap updates, create a fine-grained personal access token
with **Contents: Read and write** access limited to the tap repo
`linhan-dev/homebrew-pi-agent-connect`, then store it as an Actions secret
named `TAP_TOKEN` in *this* repo (Settings → Secrets and variables → Actions,
or `gh secret set TAP_TOKEN -R linhan-dev/pi-agent-connect`). Without it the
tap is not updated automatically — run the tap's `update-formula` workflow
manually instead.

## Commands (plain text, no registered slash commands)

| Command            | Alias | Action                                                        |
|--------------------|-------|---------------------------------------------------------------|
| `/new`             | `/n`  | Abort task, clear queue; next message starts a fresh session  |
| `/abort`           | `/a`  | Abort the running task, clear queue                           |
| `/session`         | `/s`  | Session id / file / model / thinking / tokens via pi RPC      |
| `/model <ref>`     | `/m`  | Switch model (`provider/modelId`, persists in pi settings)    |
| `/model`           |       | List available models                                         |
| `/thinking <lvl>`  | `/t`  | Set thinking level (persists in pi settings)                  |
| `/thinking`        |       | List available levels                                         |

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
