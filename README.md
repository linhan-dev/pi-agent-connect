# pi-agent-connect

**Connect pi agent to your IM. Work in DMs with minimal config.**

[English](README.md) · [简体中文](README.zh-CN.md)

## Features

- **A gateway between your IM and pi.** DM the bot; pi-agent-connect hands your
  message to the `pi` agent on your machine and relays the reply back.
- **Use it from anywhere.** pi keeps running on your machine. You DM it from
  your phone.
- **Private chats only.** No channels, no group chats. A DM is a session, and
  that's the whole model.
- **No new concepts.** Sessions, commands and model settings come straight from
  pi itself. You use pi the same way, just over a DM.
- **One binary, minimal config.** No database, no config file, no daemon.
- **Currently supports Discord.**

## Setup

1. **Create a Discord bot** and copy its token. See
   [this guide](docs/discord-bot-setup.md).

2. **Install the binary.**

   ```bash
   # macOS
   brew tap linhan-dev/pi-agent-connect
   brew install pi-agent-connect

   # or from source
   cargo build --release
   ```

3. **Configure and run.**

   Only `PIAC_DISCORD_TOKEN` is required. Don't know your user id yet? No
   problem. Leave it unset, run once, and read it from the log (below).

   **First run (lockdown mode)**

   ```bash
   export PIAC_DISCORD_TOKEN="..."                   # the token from step 1
   pi-agent-connect
   ```

   The startup log will say `LOCKDOWN MODE`. DM the bot. It **won't reply**
   (messages are blocked, that's expected), but the terminal prints a
   `blocked message` line containing your `user_id`. Note it down.

   **Second run (normal mode)**

   ```bash
   export PIAC_DISCORD_TOKEN="..."                   # the token from step 1
   export PIAC_DISCORD_ALLOWED_USER_ID="1234567890"  # the user_id from the log
   pi-agent-connect
   ```

   This time the bot DMs you a startup message on boot. You're whitelisted,
   start chatting.

   `PIAC_CWD` (optional) sets pi's working directory; it defaults to the directory
   you launched from.

   Keep the exports in `~/.zshrc` so you don't retype them every time.

## Usage

Plain-text commands, no registered slash commands.

| Command            | Alias | Action                                      |
|--------------------|-------|---------------------------------------------|
| `/new`             | `/n`  | Abort the running task, clear queue; next message starts a fresh session |
| `/abort`           | `/a`  | Abort the running task, clear queue         |
| `/session`         | `/s`  | Session id / file / model / thinking / tokens |
| `/model [ref]`     | `/m`  | Switch model; list models with no arg       |
| `/thinking [lvl]`  | `/t`  | Set thinking; list levels with no arg       |

Model and thinking levels are pi's own (`~/.pi/agent/settings.json`). Change
them anytime with `/model` and `/thinking` in the DM.

Messages prefixed `[pi-agent-connect]` come from the gateway itself; messages
without the prefix are pi's replies, forwarded as-is.

## Architecture

pi-agent-connect is a thin gateway by design. It only spawns `pi` and speaks its
documented CLI, so sessions, models and settings stay pi's own
(`~/.pi/agent/`).

```
Discord events ──► Router (pure) ──► Worker (single consumer, in-memory queue)
                       ├─ print mode:  pi -c -p "…"
                       └─ short RPC:   pi --mode rpc --continue
```

**Adding another IM.** IM backends are pluggable behind a thin interface.
Adding one means implementing that interface. Telegram, WhatsApp, any IM with
DMs can slot in. PRs welcome.

## License

MIT
