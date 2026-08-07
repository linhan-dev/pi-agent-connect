# 创建 Discord bot 并私聊它

**创建 bot、拿到 token、私聊它。pi-agent-connect 需要的一切，大约 5 分钟。**

[English](discord-bot-setup.md) · [简体中文](discord-bot-setup.zh-CN.md)

> ⚠️ **只支持私聊。** pi-agent-connect 只走私聊。没有频道、没有群聊。频道里的消息会被忽略，永远不会得到回复。

## 1. 创建应用

打开 [Discord Developer Portal](https://discord.com/developers/applications)，点右上角 **New Application**，起个名字（比如 `pi`），**Create**。

## 2. 创建 bot 并复制 token

左侧栏进 **Bot** 页。**Token** 栏显示的是打码的 token。点 **Copy** 复制（只有想轮换时才需要点 **Reset Token**）。token 要保密，稍后在 [README](../README.zh-CN.md#安装) 的配置步骤里填进 `PIAC_DISCORD_TOKEN`。

## 3. 开启 Message Content Intent

还在 **Bot** 页，往下滚到 **Privileged Gateway Intents**，打开 **Message Content Intent**，保存。不开的话 bot 收不到消息内容，pi-agent-connect 也就没有东西可以转发。

## 4. 把 bot 拉进服务器

Discord 规定，只有和 bot **在同一个服务器**的用户才能私聊它。所以得先把 bot 拉进一个服务器（你自己的私人服务器就行）。

1. 左侧栏进 **OAuth2 → URL Generator**。
2. 勾选 `bot` scope。私聊不需要任何频道权限，可以全不勾。
3. 复制生成的 URL，浏览器打开，选一个服务器，授权。

## 5. 私聊 bot

先启动 pi-agent-connect，然后私聊 bot。

1. 在成员列表或任意频道里点 bot 的名字。
2. 在弹出的资料卡里点 **Message**，私聊就打开了。
3. 发一句 `/session` 检查连接，或者随便打个招呼。

bot 离线时也会显示在成员列表里；但 pi-agent-connect 运行之前发的消息不会得到回复。

## 注意事项

- **频道消息会被忽略。** pi-agent-connect 只读私聊。
- **token 是机密。** 任何人拿到它都能控制你的 bot。
- 想轮换 token 就再点一次 **Reset Token**，旧 token 立即失效。
