# 前台 Subagent 修复（macOS）

## 问题

Cursor 3.26.x 下，让 agent 派**前台** subagent（`run_in_background` 缺失或 `false`）时立刻失败：

```
[invalid_argument] Cursor user message action has no message_id
```

后台 subagent（`run_in_background: true`）不受影响。

## 根因

引擎 `cursor_server::compile::run::action`（`0x10076f0a4`）在编译 Cursor 发来的
action 时，要求 `UserMessage.message_id` 非空：

```
0x10076f344   ldr  x8, [x19, 0x880]    ; message_id 指针
0x10076f348   cbz  x8, 0x10076f738     ; 空 -> 报 "has no message_id"
```

BYOK 桥下发起的**前台** subagent action 不带该字段，于是被拒。后台 subagent 走另一条
不经过此校验的路径，所以正常。

证据（`bridge.db` 的 `tool_round_calls`，11 次 Task 调用 100% 相关）：

| `run_in_background` | 结果 |
|---|---|
| 缺失 / `false` | 失败 7 次，0 成功 |
| `true` | 成功 5 次，0 失败 |

## 修法

把非空校验改成跳进引擎**自带的"空 message 降级路径"**（`0x10076f780`）。这条路径
原本就存在，用于 message 列表为空的情况，最终仍走正常构造并返回。

```
0x10076f348   cbz x8, 0x10076f738   (0xb4001f88)
          ->  b     0x10076f780     (0x1400010e)
```

改动只有一条指令、3 个字节（`0x76f348~0x76f34b`），不碰数据结构、不碰
`conversation_id`、不碰 `message_id` 语义。

## 应用

```bash
python3 patch-engine.py <原始 cursor-magpie-bridge> <输出>
codesign --force --sign - <输出>
```

脚本会校验原指令是否为 `0xb4001f88`，不匹配就拒绝改动，避免误补到别的版本上。

## 验证

- 前台 subagent 成功返回 `Subagent ID` + 实际调研结果
- 子 agent 会话独立（`conversation_id` 与父不同，不外泄、不污染父上下文）
- 补丁后引擎正常启动，`status.json` 显示 `connected`、13 个模型

## 范围

- 只修 **macOS arm64**。Windows 的 `cursor-magpie-bridge.exe` 未改动。
- 基于 `engine_version 0.2.1`（`0.2.7` 的 JS 层 + 0.2.1 的引擎）。上游换新引擎后需重新定位。
