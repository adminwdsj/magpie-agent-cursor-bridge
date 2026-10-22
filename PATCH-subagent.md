# 前台 Subagent 支持（跟踪中的后续项，开源构建尚未包含）

## 现状

0.3.0 的开源引擎**不包含**任何 subagent 补丁，`compile::run::action` 保持上游原样。

- **后台 subagent**（`run_in_background: true`）：正常工作。
- **前台 subagent**（`run_in_background` 缺失 / `false`）：返回上游错误
  `Cursor user message action has no message_id`。这是上游刻意的设计，不是 BUG。

需要派 subagent 时请用后台 subagent。

## 为什么移除了旧的二进制补丁

0.2.x 的闭源引擎带过一个二进制补丁，把

```
0x10076f348  cbz x8, 0x10076f738   (0xb4001f88)
         ->  b     0x10076f780     (0x1400010e)
```

本意是让空 `message_id` 落到“降级路径”。但对照开源源码
（`server/src/cursor/compile/run.rs` 的 `action()`）发现：`0x10076f348` 对应的
`cbz` 是**正常用户消息处理块的入口判断**，不是单纯的空值校验。跳过它会让**每一条**
用户消息都绕过正常编译路径，直接进只处理单字段的 stub。

后果就是长会话里出现 1–2 条消息的畸形 checkpoint、`FOREIGN KEY constraint failed`
(787) 和 “stable history shrank from N to M roots” 一连串错误。所以这个补丁是
checkpoint 损坏的**根因**，开源构建里彻底不带它。

## 要在源码层正确支持前台 subagent（后续）

正确做法不是跳过校验，而是在 `action()` 的 `UserMessageAction` 分支里，当
`user.message_id.is_empty()` 时**合成**一个确定性 id，再走正常路径：

```rust
// server/src/cursor/compile/run.rs，UserMessageAction 分支
let message_id = if user.message_id.is_empty() {
    // 前台 subagent 不带 message_id：用内容 + nonce 合成稳定 id，
    // input_id 仍是 cursor:user:<id>，正常锚定、正常构造 runtime event。
    format!("synthetic:{}", blob_id_of(&user.text, &action_context))
} else {
    user.message_id.clone()
};
```

- subagent 会话 `conversation_id` 独立于父，不污染父上下文。
- 合成 id 必须确定（同一内容同一 id），避免重复提交生成多个锚点。
- 改完要在本机真实跑一次前台 subagent + 一次长会话回归，确认不再出现 787 / shrank。

落在 `source.patch` 里，和桥接 bin 一起从源码编译，不再碰二进制。

## 范围

- 仅 macOS arm64。Windows 的 `cursor-magpie-bridge.exe` 本仓不维护。
- 基于 leookun/cursor-byok（engine 自报 `0.2.1`）。上游换引擎后按源码重新定位即可，
  不再依赖二进制偏移。
