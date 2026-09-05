# pi-agent-connect

**把 pi agent 接入你的 IM。在私聊里干活，配置极简。**

[English](README.md) · [简体中文](README.zh-CN.md)

## 特性

- **你的 IM 和 pi 之间的网关。** 给 bot 发私信，pi-agent-connect 把消息交给本机运行的 `pi` agent，再把回复发回私聊。
- **在哪都能用。** pi 跑在你的电脑上，你从手机或任何地方给它发私信。
- **只做私聊。** 没有频道、没有群聊。一个私聊就是一个会话，仅此而已。
- **没有新概念。** 会话、命令、模型设置全部来自 pi 自己。用法和 pi 终端一样，只是换成了私聊。
- **一个二进制，极简配置。** 没有数据库、没有配置文件、没有守护进程。
- **目前支持 Discord。**

## 安装

1. **创建 Discord bot** 并复制它的 token。方法见[这篇教程](docs/discord-bot-setup.zh-CN.md)。

2. **安装二进制。**

   ```bash
   # macOS
   brew tap linhan-dev/pi-agent-connect
   brew install pi-agent-connect

   # 或者从源码编译
   cargo build --release
   ```

3. **配置并运行。**

   必填的只有 `PIAC_DISCORD_TOKEN`。还不知道自己的用户 id？没关系。先不设它，跑一次从日志里查（见下）。

   **第 1 次运行（锁定模式）**

   ```bash
   export PIAC_DISCORD_TOKEN="..."                   # 第 1 步复制的 token
   pi-agent-connect
   ```

   启动日志会提示 `LOCKDOWN MODE`。给 bot 发条私信。它**不会回复**（消息被拦下了，这是正常的），但终端会打印一行 `blocked message`，里面有你的 `user_id`。记下来。

   **第 2 次运行（正常模式）**

   ```bash
   export PIAC_DISCORD_TOKEN="..."                   # 第 1 步复制的 token
   export PIAC_DISCORD_ALLOWED_USER_ID="1234567890"  # 刚才日志里的 user_id
   pi-agent-connect
   ```

   这次启动后，bot 会主动给你发一条私信。你已经进了白名单，直接开聊。

   `PIAC_CWD`（可选）设置 pi 的工作目录；不设置就用启动时的当前目录。

   `PIAC_COMMAND_PREFIX`（可选）不用设置，命令默认以 `.` 开头。

   `PIAC_PROMPT_TIMEOUT`（可选）每次消息里 pi 任务允许运行的最长秒数，默认 3600（1 小时）。
   超时后任务会被中止，并在私聊里收到提示。

   可以把这些 export 写进 `~/.zshrc`，免去每次重敲。

## 用法

纯文本命令，无需注册斜杠命令。命令以 `.` 开头：

| 命令               | 别名 | 作用                                              |
|--------------------|------|---------------------------------------------------|
| `.new`             | `.n` | 终止当前任务，清空队列；下一条消息开启新 session                 |
| `.abort`           | `.a` | 中止正在运行的任务，清空队列                       |
| `.session`         | `.s` | 查看 session id / 文件 / 模型 / thinking / tokens  |
| `.model [ref]`     | `.m` | 切换模型；不带参数列出可用模型                     |
| `.thinking [lvl]`  | `.t` | 设置 thinking 等级；不带参数列出可选等级           |

模型和 thinking 等级由 pi 自己管理（`~/.pi/agent/settings.json`）。随时可以在私聊里用 `.model` 和 `.thinking` 修改。

带 `[pi-agent-connect]` 前缀的消息来自 pi-agent-connect 本身；没有前缀的都是 pi agent 的回复，原样转发。

## 架构

pi-agent-connect 在设计上就是个薄网关。它只负责拉起 `pi` 并调用它文档化的 CLI，所以会话、模型和设置都归 pi 自己管（`~/.pi/agent/`）。

```
Discord events ──► Router (pure) ──► Worker (single consumer, in-memory queue)
                       ├─ print mode:  pi -c -p "…"
                       └─ short RPC:   pi --mode rpc --continue
```

**接入其他 IM。** 各 IM 后端通过一个薄接口接入，加一个新的只是实现这个接口。Telegram、WhatsApp，任何有私聊的 IM 都能接入。欢迎提交 PR。

## 许可

MIT

## 致谢

本项目受 [Crokily/pi-discord-gateway](https://github.com/Crokily/pi-discord-gateway) 启发。

## 讨论

- https://linux.do/t/topic/2725195
