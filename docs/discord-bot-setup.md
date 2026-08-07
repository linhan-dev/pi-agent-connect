# Setting Up a Discord Bot

**Create a bot, grab its token, and DM it. That's everything pi-agent-connect
needs, in about 5 minutes.**

[English](discord-bot-setup.md) · [简体中文](discord-bot-setup.zh-CN.md)

> ⚠️ **DMs only.** pi-agent-connect works over DMs only. No channels, no group
> chats. Channel messages are ignored and never get a reply.

## 1. Create an application

Open the [Discord Developer Portal](https://discord.com/developers/applications),
click **New Application** in the top-right, give it a name (e.g. `pi`), and hit
**Create**.

## 2. Create the bot and copy its token

In the left sidebar, go to **Bot**. The token shows up masked under **Token**.
Click **Copy** to grab it (hit **Reset Token** only if you ever want to rotate
it). Keep the token secret; you'll set `PIAC_DISCORD_TOKEN` to it in the
[README](../README.md#setup) config step.

## 3. Enable Message Content Intent

Still on the **Bot** page, scroll down to **Privileged Gateway Intents**, enable
**Message Content Intent**, and save. Without it the bot can't see message
contents, and pi-agent-connect has nothing to relay.

## 4. Invite the bot to a server

Discord only lets you DM a bot when you share a server with it, so invite the
bot to one of your servers first (a private server works fine).

1. In the left sidebar, go to **OAuth2 → URL Generator**.
2. Tick the `bot` scope. No channel permissions are needed for DMs. You can
   leave them all unchecked.
3. Copy the generated URL, open it in a browser, pick a server, and authorize.

## 5. DM the bot

Start pi-agent-connect first, then DM the bot.

1. Click the bot's name in the member list or in any channel.
2. In the profile popup, hit **Message** to open a DM.
3. Send `/session` to check the connection, or just say hello.

The bot appears in the member list even while offline; messages sent before
pi-agent-connect is running won't be answered.

## Notes

- **Channel messages are ignored.** pi-agent-connect only reads DMs.
- **The token is a secret.** Anyone holding it can control your bot.
- To rotate the token, hit **Reset Token** again. The old one stops working
  immediately.
