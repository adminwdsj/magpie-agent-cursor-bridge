# 前台 Subagent 支持（0.3.1 已在源码层修复）

## 现状

0.3.1 起,前台和后台 subagent 都能用。修复在源码里,随引擎一起编译,不碰二进制。

- **前台 subagent**（`run_in_background` 缺失 / `false`）：✅ 正常。
- **后台 subagent**（`run_in_background: true`）：✅ 正常（本来就行）。

## 问题

Cursor 派前台 subagent 时,发来的 action 不带 `message_id`,上游
`compile::run::action` 直接拒绝:

```
[invalid_argument] Cursor user message action has no message_id
```

证据（`bridge.db` 的 `tool_round_calls`,11 次 Task 调用）:

| `run_in_background` | 结果 |
|---|---|
| 缺失 / `false` | 失败 7 次,0 成功 |
| `true` | 成功 5 次,0 失败 |

## 修法（源码,`server/src/cursor/compile/run.rs`）

在 `action()` 的 `UserMessageAction` 分支里,当 `message_id` 为空**且**这是一个
subagent 请求（`request.subagent_type_name` 有值）时,用「subagent 类型 + 文本」合成
一个确定性 id,再**照常走正常编译路径**:

```rust
let mut user = user.clone();
if user.message_id.is_empty() {
    let Some(kind) = request.subagent_type_name.as_deref() else {
        return Err(Error::Protocol(
            "Cursor user message action has no message_id".into(),
        ));
    };
    user.message_id = format!(
        "synthetic-subagent:{}",
        BlobId::digest(format!("{kind}\0{}", user.text).as_bytes()).to_base64()
    );
}
```

要点:

- **和当年那个二进制补丁相反**。旧补丁把 `cbz` 改成 `b`,跳过了整个正常处理块,导致
  每条用户消息都走只处理单字段的 stub —— 这才是 checkpoint 畸形（787 外键、stable
  history shrank）的根因。本修法不跳任何东西,只补一个合法 id,后续 checkpoint/roots
  逻辑和普通消息一模一样地跑。
- **确定性**:同一 subagent 类型 + 同一文本 → 同一 id,重试幂等,不会生成多个锚点。
- **隔离**:subagent 用自己的 `conversation_id`,不污染父上下文。
- **非 subagent 的空 id 仍然报错**（真·协议错不被掩盖）。

## 验证

- 单测:`server/src/cursor/compile/run.rs` 的 `tests`（合成/拒绝/保留三例），
  随 `cargo test -p cursor-server --lib` 跑,163 passed。
- 端到端:在 Cursor 里让 agent 派一个前台 subagent,应正常返回子 agent 结果。

## 范围

- 仅 macOS arm64。Windows 的 `cursor-magpie-bridge.exe` 本仓不维护。
- 基于 leookun/cursor-byok（engine 自报 `0.2.1`）。改在源码,随上游换引擎自动跟随,
  不再依赖二进制偏移。
