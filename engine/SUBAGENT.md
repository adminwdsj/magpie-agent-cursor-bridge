# Subagent 排障记录

Cursor 派 subagent 失败时按下面的顺序查。路径都相对 `engine/`。

## 先看哪里

1. 引擎日志：`~/.config/magpie-cursor-bridge/engine-stderr.log`（默认只有警告和错误，搜 `ignoring unsupported runtime ConversationAction`）。
2. Cursor 日志：`~/Library/Application Support/Cursor/logs/<最新>/window*/exthost/anysphere.cursor-agent-host/`，搜 `bidiAppend failed`，报错原文直接说明是哪一类。
3. Cursor 本地开关缓存：`~/Library/Application Support/Cursor/User/globalStorage/state.vscdb` 的 `workbench.experiments.statsigBootstrap`，键是开关名的 djb2 哈希。Cursor 服务端推新开关、把 Agent 流量改到新协议，是这类故障最常见的来源。
4. 桥数据库：`~/.config/magpie-cursor-bridge/data/bridge.db` 的 `tool_round_calls`（`name='Task'`）。请求在引擎解码阶段被拒时这里不会有记录。

## 已解决的几类

| 报错 | 原因 | 修法所在 |
|---|---|---|
| `Cursor user message action has no message_id` | 前台 subagent 的 action 不带 `message_id`；用 Task 的 `resume` 续用已有 subagent 时连 `subagent_type_name` 也没有 | `server/src/cursor/compile/run.rs`：缺 id 时按「类型（续用记为 resume）+ 已有轮数 + 文本」合成确定性 id |
| `invalid wire type value: 6/7` | 服务端开关 `agent_host_local_loop` 让 subagent 改走本地循环 + `RunInference` 协议 | `server/src/cursor/services/analytics.rs`：改写 BootstrapStatsig 时关掉该开关 |
| `Cursor subagent request must include both parent headers` | Cursor 3.30 起父级请求头各自独立发送 | `server/src/api/cursor/handlers.rs` 的 `parent_headers`：只按 `x-parent-agent-tool-call-id` 判断父子关系，父请求 ID 缺失时用根请求 ID 兜底 |
| 后台子代理跑完，父对话不继续；子代理一直显示「Stopped」 | 完成通知在父对话流上到达时被当作不支持的动作丢弃；检查点没写回完成状态 | `server/src/cursor/conversation/runtime.rs`（转发 `BackgroundTaskCompletionAction`）、`conversation/output.rs`（注入当前轮）、`checkpoint/builder.rs`（`record_subagent_completions`）；未登录时的开关在 `services/analytics.rs` |

## 相关的 499

Cursor 卡顿检测 30 秒告警、90 秒中断并重试，日志关键字 `stall detector`、`stallAborted":true`。引擎的 RunSSE 输出流空闲 15 秒会主动发心跳（`server/src/api/cursor/run_sse.rs`）。

## 构建与测试

```bash
CARGO_TARGET_DIR=/tmp/cargo-target-bridge ./scripts/build-native.sh
```

脚本会先跑 `cursor-server` 全部单测和 `lodestar_replay_repro` 回归，再编译、签名、更新 `integrity.json`。仅支持 macOS arm64。
