# Changelog

## 0.3.18

新增数据库统计与清理命令。`bridge.db` 只增不减，本机已涨到 1.9 GB。

- `bash setup.command db`：按表显示行数和内容大小、文件大小和会话时间范围。
- `bash setup.command db clean traces|sessions|vacuum|all`：清理调用日志、超过 N 天（`--days`，默认 30）没更新的会话、只压缩，或清空全部运行数据（需 `--yes`）。默认清理后压缩，`--no-vacuum` 跳过。清理前停掉后台桥，结束后自动重新启动。
- 安全：删除在一个事务里完成，删除时关掉外键检查，删完逐表核对外键；引用断掉的行比删除前多，就由触发器整体回滚，数据不变。
- 参考了 kingchen95272025/magpie-agent-cursor-bridge-enhanced 0.2.10 的数据库管理，改动：
  - 不开外键检查删除：开着时每删一个检查点都要全表扫描没有索引的引用列，在 1.9 GB 的库上 5 分钟都删不完；改后同样的清理 1.2 秒。
  - 不靠出错退出回滚：sqlite3 命令行在脚本出错退出时会保留已执行的删除（3.54 实测），改用触发器 `RAISE(ROLLBACK)`。
  - 不用只读方式打开：停桥后引擎删掉 `-wal`/`-shm`，只读打开会报 `unable to open database file`。
  - 只做命令行，不做控制台页面。
- 验证：新增 4 项测试（按天清理只删旧会话并保留内容块与配置、日志与全部清空保留模型配置、会造成断引用的清理整体回滚、参数校验）；在本机数据库副本上按 14 天清理 38 个会话用时 1.2 秒，压缩 1971 MB → 1701 MB 用时 6.6 秒，外键与完整性检查通过。

## 0.3.17

后台子代理跑完后，父对话能接着往下走；子代理状态不再一直显示「Stopped」。

- 完成通知：父对话还在输出时，Cursor 会把后台子代理的完成通知（`BackgroundTaskCompletionAction`）追加到当前流上。引擎以前把它当不支持的动作直接丢弃，父对话就停在子代理后面不动。现在和用户追加消息走同一条路：父对话还在输出就注入当前轮，已经停下就用这条通知开新一轮。
- 子代理状态：收到完成通知后，把检查点里对应子代理的状态从 `Backgrounded` 改成成功、出错或已中止，并写入完成时间。
- 开关：Cursor 只有在 `long_running_jobs`、`cursor_agent_host`、`agent_store_sync_client` 都打开时才派发完成通知。登录状态下服务端本来就是开的（本机实测）；未登录、Cursor 退回桥本地开关表时，现在也显式打开。`agent_host_local_loop` 仍然关闭。
- 不带模型的首条追加：本地 agent host 可能先发心跳或工具结果再发 RunRequest，以前报 `first BidiAppend message must select a model` 拒收，现在按本地请求处理。Cursor 官方流的第一条一定带模型，不受影响。
- 诊断日志：引擎启动时初始化日志，默认只记警告和错误，写入本机 `engine-stderr.log`；需要时用 `RUST_LOG=cursor_server=info` 调高。
- 参考了 kingchen95272025/magpie-agent-cursor-bridge-enhanced 0.2.7 的做法；没有照搬「桥主动唤醒父对话、父对话保持 15 分钟不结束」那一层，Cursor 自己会派发完成通知，等有证据表明它漏发再加。
- 验证：新增完成状态写回、本地开关表两项单测。

## 0.3.16

引擎源码收进本仓库 `engine/`，不再保存补丁、构建时也不再拉取外部代码；`scripts/build-native.sh` 直接编译仓库源码。排障记录见 `engine/SUBAGENT.md`。

适配 Cursor 3.30.8：subagent 再次大量失败；新加的模型在菜单里显示成「名字 · 渠道」。

- subagent：Cursor 3.30 起各个父级请求头独立发送，子 agent 请求可能只带 `x-parent-request-id`，或只带 `x-parent-agent-tool-call-id` 加 `x-root-parent-request-id`。引擎原先要求两个头必须同时出现，否则整条请求报 `Cursor subagent request must include both parent headers`。现在只按工具调用 ID 判断父子关系，父请求 ID 缺失时用根请求 ID 兜底；只有父请求 ID 时按普通请求处理。
- 模型名：用户没在 Magpie 里改过名的模型，Magpie 交给插件的名字带 ` · 渠道` 后缀，桥原样透传。现在统一去掉这个后缀；改过名的模型仍以 Magpie 里的名字为准。
- 安装器：Homebrew 把 Node 升到依赖 `libnode.137.dylib` 的版本后，安装器只认写死的 `libnode.127.dylib`，没把新库复制进发布目录，后台反复因缺库启动失败。现在按 `otool` 结果复制任意版本的 `libnode`；测试改为真正启动复制出的运行时。
- 验证：新增父级请求头组合的引擎单测与模型名回归测试；引擎单测 170 项、Lodestar 回归 3 项、插件测试 74 项通过；本机安装后桥正常连接，菜单名已无渠道后缀。

## 0.3.15

修复 Gemini（gemini-4-carbon / argon）等长时间静默思考的模型被 Cursor 反复中断，上游记大量 499。

- 原因：Cursor 的卡顿检测在 30 秒无数据时告警、90 秒时中断请求并重试；引擎只在收到客户端心跳时回一次心跳，模型思考期间输出流完全静默。中断让上游记 499，重试又撞上 agy 的"相同提示词正在运行（503）"。
- 修法：引擎的 RunSSE 输出流空闲 15 秒就主动发一个心跳帧；心跳直接写入响应，不进重放日志。
- 验证：新增"静默 15 秒先收到心跳、之后正常转发数据"的回归测试，引擎单测 169 项通过。

## 0.3.14

修复 Cursor 3.29.15 起 subagent 全部失败（`protobuf decode error … invalid wire type value: 6/7`）。

- 原因：Cursor 服务端以 100% 灰度打开了 `agent_host_local_loop`，subagent 改为在 Cursor 本机跑循环、只通过 `RunInference` 协议请求模型。桥把这条 `RunInferenceClientMessage` 当作 `AgentClientMessage` 解码，会话 ID 首字符 `'6'`/`'7'` 被读成非法 wire type。
- 修法：引擎改写 `BootstrapStatsig` 下发时，把 `agent_host_local_loop` 关掉（与已关闭的两个 websocket 开关同一处），Cursor 退回桥支持的后端循环。这也是 Cursor 自身报错文案建议的做法。
- 升级后需彻底退出并重开一次 Cursor，让它重新拉取开关。
- 验证：引擎 analytics 单测 3 项（新增"服务端下发 true 被改为 false"回归测试）通过；在 Cursor 3.29.15 实测派 subagent 成功。

## 0.3.13

卸载时删除本机根证书。此前 `setup.command uninstall` 只还原 Cursor 代理，证书和私钥都留在本机。

- 卸载按 SHA-1 指纹精确删除钥匙串里本插件的 `Cursor BYOK Local CA`（同名的其他工具证书不受影响），并删除 `data/ca` 目录及私钥；钥匙串删除失败时会明确提示手动删除。
- README 写明会安装受信任根证书、只在插件页移除不会删除证书，以及 Cursor 使用条款带来的账号风险；说明本插件为社区插件，未上架插件市场。
- 验证：插件测试 73 项通过（新增卸载删证书回归测试）。引擎与 0.3.12 相同。

## 0.3.12

Cursor 模型菜单只显示 Magpie 里的模型名，不再在后面拼接 ` · provider/model` 完整 ID；完整 ID 仍在悬停提示里（`Magpie / <id>`）。

- 引擎显示名由「名字 · ID」改为「名字」，引擎重新构建，`integrity.json` 校验值已更新。引擎内部模型编号由显示名参与计算，升级后 Cursor 里已选的模型可能需要重新选一次。
- `scripts/build-native.sh` 尊重 `CARGO_TARGET_DIR`，可复用编译缓存。
- 验证：引擎单测 3 项（新增显示名断言）、Lodestar 回归 3 项、插件测试 72 项通过。

## 0.3.11

修复 `magpie cursor-bridge <模型>` 命令行连接报 `ReferenceError: process is not defined`：Magpie 命令行在没有 `process` 的沙箱里执行插件的 `connect()`，自动启动检查现在先判断 `process` 是否存在，连接配置照常写出，由已在运行的后台桥接自行接管。

- 验证：插件测试 72 项通过，新增无 `process` 沙箱的连接回归测试（修复前失败）。

## 0.3.10

改名 **Magpie-Cursor-Relay**（npm 包 `magpie-cursor-relay`），准备上架 Magpie 插件市场，与同名上游 `magpie-agent-cursor-bridge` 区分。Agent ID 仍是 `cursor-bridge`，已有连接与设置不受影响。

- `package.json` 的 `os`/`cpu` 改为实际随包提供的 `darwin`/`arm64`，不再声明 Windows 与 Intel。
- 功能代码与 0.3.9 相同。

## 0.3.9

修复 Connect 之后在 Magpie 新增的模型在 Cursor 里显示成 `provider/model` 完整 ID 的问题。

- 每次刷新模型列表都读取 Magpie `settings.json` 里的 `modelNames`，新模型和改名立即生效，无需重新 Connect。
- 名字优先级：Magpie 当前名字 → Connect 时的快照名 → 网关返回名 → 去掉 provider 前缀的模型 ID。
- 验证：插件全量测试 71 项通过，新增名字优先级回归测试（修复前失败）。

## 0.3.8

适配 Cursor checkpoint 消息中 `{"__type":"Uint8Array","hex":"..."}` 格式的图片载荷，修复含图会话续聊报 `Cursor message is missing image` 的问题。

## 0.3.7

适配 Cursor 3.29.9 协议变更，修复更新后对话报错或挂起的问题。

- `BidiAppend` 解码同时兼容 `data_binary`（tag 4 二进制字节）与旧版 `data`（tag 1 hex）。
- `BootstrapStatsig` 强制关闭 `nal_websocket_client` 与 `run_inference_websocket_client`，并将 `/agent/v1/run` 加入本地代理路由，防止 WebSocket 直连绕过本地桥。
- 验证：服务端 166 项单测、插件 70 项单测及 `data_binary` 端到端冒烟均通过。

## 0.3.6

修复安装器未把 Node 运行时依赖的 `libnode.127.dylib` 一起放入发布目录，导致 launchd 启动桥接时出现 `OS_REASON_DYLD` 并反复崩溃。

- 发布目录现在根据 `otool -L` 自动复制 `@rpath` 动态库，launchd 不再依赖开发机的 Homebrew 路径。
- 验证：插件全量测试 70 项通过，并覆盖运行时动态库随 release staging 复制。

## 0.3.5

修复上游 413 请求体超限被桥错误编码成 `unavailable` 的问题：这类确定性拒绝现在标为不可重试并立即显示，避免 Cursor 对同一 52.9 MB 请求重复重试；502/超时仍保留可重试。

- 发布包包含新的原生引擎，保留 0.3.4 的续聊历史修复。
- 验证：发布模式错误生命周期 7 项、Lodestar 回归 3 项、413 分类与 Cursor 错误详情回归、插件测试 70 项全部通过。

## 0.3.4

修复 Cursor 续聊反复从旧进度开始，以及随后报
`Cursor stable history shrank` 的问题。

- Cursor 复用用户消息编号、同时携带更新后的完整历史时，以本次历史为准；
  旧 `input_anchor` 仅用于未携带根历史的请求。保留原去重机制和历史完整性检查，
  不清空数据库，不删除消息，不修改模型或输出预算。
- 加入离线回归测试：同编号但内容变化、同内容带最新历史续聊、无历史重试。
  修复前前两项失败；修复后全部通过，模拟工具执行一次并成功入账。
- 构建脚本固定到本次实际测试的源码提交，保留独立的 engine_version，
  加入回归测试并在计算校验值前完成 macOS 临时签名。
- 修复插件测试的自动启动隔离：在安排异步任务前读取禁止启动开关，
  测试恢复原环境变量，防止测试结束后意外安装插件。
- 验证：完整服务端测试 268 项、插件测试 70 项全部通过；真实历史离线回放和
  实际 Cursor 续聊均已越过原故障点，历史继续增长、工具结果正常入账。
- 升级只在没有活动任务时切换后台进程，保留旧发布目录；无需清空会话数据库。
- 修复上游 413 请求体超限被桥错误编码成 `unavailable` 的问题：这类确定性拒绝现在标为不可重试并立即显示，避免 Cursor 对同一 52.9 MB 请求重复重试；502/超时仍保留可重试。
- 验证：发布模式错误生命周期 7 项、Lodestar 回归 3 项、413 分类与 Cursor 错误详情回归均通过。

## 0.3.3

Stop long opus replies from being truncated. Responses were cut mid-stream with
`finish_reason=length`, failing the run with "model stopped before completing the
response", because the request never carried an output cap and the gateway's low
default applied.

### Fixed

- `configure()` now propagates a model's configured output cap to the request
  (`model.max_output_tokens`); it previously set display name / context / reasoning
  but dropped the token cap, so `openai_chat` sent no `max_completion_tokens` and
  the gateway truncated. This was an upstream gap — no DB or config value could
  reach the request without it.
- The bridge catalogues each model's real `max_output_tokens` from the gateway
  `/models` response (captured in `gatewayModels`), clamped to 64000 to stay well
  inside the context window. So opus gets a generous cap and smaller models keep
  their own limit, instead of every model inheriting the gateway's low default.

## 0.3.2

Harden message hydration against a non-UTF8 `payload_json`. An older engine once
wrote raw bytes where a user `message_id` belonged, leaving one row whose JSON is
not valid UTF-8; `messages.rs` read it with `row.get::<&str>` which **panics** on
bad bytes, taking down the worker thread (the stray `messages.rs:101` panic seen
in long conversations).

### Fixed

- Hydrate reads `payload_json` with `try_get`, so a non-UTF8 row fails that one
  request with a decode error instead of panicking the worker. The affected
  conversation's live lineage keeps working; only a turn that actually touches the
  bad row errors, recoverably. No data is deleted — the clean engine never writes
  such rows, this just stops the old scars from crashing anything.

## 0.3.1

Front subagents work. When the agent spawns a subagent with `run_in_background`
false or absent, Cursor sends the turn without a `message_id`, and upstream
`compile::run::action` rejected it with `Cursor user message action has no
message_id` — so only background subagents (`run_in_background: true`) ran.

### Fixed

- A subagent turn with an empty `message_id` now gets a deterministic synthetic
  id (`synthetic-subagent:<digest of kind + text>`) and compiles through the
  **normal** path — unlike the old binary patch, which skipped the compile path
  and corrupted checkpoints. The id is derived from the subagent kind and text,
  so retries stay idempotent; the subagent runs in its own `conversation_id` and
  never pollutes the parent. A non-subagent empty `message_id` is still rejected.
  Covered by unit tests in `server/src/cursor/compile/run.rs`.

## 0.3.0

The engine is now built from source instead of shipped as a prebuilt binary. The
bin is `cursor-magpie-bridge` compiled from leookun/cursor-byok (MIT) plus
`source.patch`, which adds the headless bridge entry (`server/src/bin/
cursor-magpie-bridge.rs`): it reads `effective.json`, catalogues the Magpie models
against a loopback-only gateway, runs the proxy over the already-trusted CA, and
emits the same `ready` event the supervisor consumes. The JS layer, config
contract, CA path, and data dir are unchanged, so this is a drop-in swap that
reuses the existing trusted CA with no re-trust.

### Fixed

- Dropped the front-subagent binary patch that earlier builds carried. That patch
  rerouted every user message past the engine's normal compile path, which
  produced the 1–2 message checkpoints and the `FOREIGN KEY constraint failed`
  (787) / "stable history shrank" errors seen in long conversations. The source
  build ships upstream `compile::run::action` unchanged: conversations compile
  correctly. A **front** subagent (`run_in_background` false/absent) still returns
  the upstream `Cursor user message action has no message_id`; use a background
  subagent (`run_in_background: true`). Re-adding front-subagent support at source
  level is a tracked follow-up, see `PATCH-subagent.md`.

## 0.2.7

A toggle restarted Cursor to make it re-read `http.proxy`, which cost every open
editor and put the gateway's 499s in the log while the model list still took its
time. The upgrade path also left two things behind: the real Startup entry still
named the previous release, and the previous release's watchdog kept running.

### Changed

- A connect or disconnect no longer restarts Cursor. The relay already keeps the
  published address constant, so a toggle does not change the value Cursor holds;
  `CURSOR_BRIDGE_RESTART_CURSOR=1` restores the old kill for anyone who wants it.
  `CURSOR_BRIDGE_NO_RESTART` is gone, since not restarting is now the default.

### Fixed

- An automatic install no longer redirects the Startup entry. `connect()` passed
  `CURSOR_BRIDGE_ROOT`, and `pathed()` treats that as an isolated install, so the
  entry was written into a private folder while the real one kept pointing at the
  release it was meant to replace: the next login started the old build. The
  variable is now forwarded only when the caller set it themselves.
- Upgrading stops the previous watchdog. It holds its own lock and outlives the
  supervisor it guards, so stopping only `supervisor.lock` left the old release's
  watchdog watching a directory that had already been replaced.
- `activate` reads the Startup entry back and fails when it does not name the
  release being installed, instead of reporting success.

## 0.2.6

Magpie replaces this plugin's files from GitHub but never rewrites the record of
the installed release, so after an update the supervisor on disk was still the
previous plugin's. `start()` asked whether a supervisor was alive and, when the
old one still held its lock, returned before anything compared versions: an
upgrade from the Magpie plugin page silently kept running the old build, and the
toggle still read connected.

### Fixed

- A live supervisor is reused only when it runs the plugin's own version. The
  version it publishes in `status.json` is compared with `package.json`, and a
  mismatch falls through to `setup()`, which stops the old supervisor and stages
  a release matching the files that are actually installed. Upgrading in Magpie
  now takes effect on the next toggle instead of requiring a manual install.
- A `status.json` without a version is not treated as a mismatch. An older
  supervisor cannot be attributed that way, and rebuilding on it would fight a
  healthy bridge on every connect.

## 0.2.5

Cursor reads `http.proxy` once and keeps using that address, so writing the
setting was never enough: Cursor only noticed when its own retry backoff
expired. Leaving the address stable helped, but the switch still felt slow.

### Fixed

- Cursor is restarted when, and only when, the published proxy address actually
  changes. This is what upstream cursor-byok does: it ends Cursor before writing
  the settings, and Cursor reads the new proxy as it reopens. The switch is now
  immediate in both directions instead of waiting for Cursor to come around.
  A rewrite with the same address still does nothing, so a healthy bridge never
  kills Cursor in a loop.
- `applyProxy` now reports whether the settings text changed, which is what
  separates a real switch from the per-pass rewrite. Disconnecting relies on the
  same signal, so Cursor drops the Magpie models as soon as the keys are removed
  instead of continuing to use a proxy that is gone.

### Added

- `CURSOR_BRIDGE_NO_RESTART=1` keeps Cursor running, trading the immediate
  switch for Cursor's own refresh delay. Useful while something unsaved is open.

## 0.2.4

Cursor caches the proxy address it read from `settings.json`, so anything that moved that address cost a visible stall. Both changes below keep the published address and the map behind it still.

### Fixed

- Cursor is given a stable proxy address. The engine takes an ephemeral port on every start, so a watchdog restart moved the address while Cursor kept dialing the previous one; its `agent-store-sync` then failed with "Failed to establish a socket connection to proxies" and retried on a backoff, which is what made the model list appear late after a reconnect. The supervisor now listens on a fixed port (`CURSOR_BRIDGE_RELAY_PORT`, default 52407) and forwards to whatever port the engine holds, so a restart is invisible to Cursor. Verified: the engine moved from pid 21340 to 31640 with the published address unchanged, and both the proxy CONNECT and the catalogue still answered.
- The fixed port is bound, not probed. Asking "is this port free?" first was a race with the previous supervisor's closing socket, which answered "busy" and published a different address -- exactly the churn the fixed port prevents. Binding is retried briefly, and a fallback port is announced in `status.json` instead of being adopted silently.
- A toggle is noticed in under a second. The serving loop waited two seconds and the catalogue was refreshed every fifteen, although the gateway answers in tens of milliseconds. Disconnect and reconnect now measure about 0.8s each, down from 1.5s and 1.9s.

## 0.2.3

Everything below was found on a machine where Magpie showed the agent as connected while Cursor listed only its own models. The bridge had been dead for seventeen hours and nothing said so.

### Fixed

- A release staged by an earlier plugin is no longer launched. Magpie replaces this plugin's files from GitHub but never rewrites `installation.json`, so the recorded release could belong to the previous plugin; the older `bridge.js` has no watchdog, and one exit left the bridge down for good. A version mismatch now falls back to `setup()` and stages a release that matches the installed files.
- The supervisor starts with its own watchdog enabled. `launch()` passed `CURSOR_BRIDGE_NO_WATCHDOG=1`, which disabled the only watchdog that exists: `connect()` runs once per toggle, so nothing else could ever restart a supervisor that died. A killed supervisor was revived within a second in testing, and Cursor's proxy followed the new port.
- A failed launch is recorded in `autostart.log`. `spawn` reports an unrunnable runtime through its `error` event instead of throwing, and the handler was empty, so a launch that never produced a supervisor left no evidence anywhere.
- An engine that exits before `ready` no longer reports "check certificate trust". Exit code 107 was printed with that hint on a machine whose CA was trusted all along, sending every reader down the wrong path; the message now says the engine exited before ready and leaves the cause to the engine's own stderr.
- A scratch `home` no longer writes the Startup entry of the real profile. `paths()` read `APPDATA` unconditionally, so running the tests overwrote the real Startup script with a path into `%TEMP%`; that tree was deleted when the test finished and every later login silently started nothing.
- `install.js` honours `CURSOR_BRIDGE_ROOT`. It ignored the variable `agent.js` passes when falling back to `setup()`, so a run that asked for an isolated root rewrote the real `installation.json`, staged into the real release directory and started a second supervisor.
- `npm-shrinkwrap.json` no longer pins the package to macOS/arm64. Installing on Windows was refused by npm's platform check while `package.json` already allowed both platforms.

### Changed

- Starting the bridge is reported as `starting` until the engine is up, so a reader never sees the previous run's `connected` during the gap.
- The agent notice no longer tells users to restart Cursor. The catalogue is served per request, so opening a panel or the model picker is enough; the old wording sent users to a restart that merely masked the startup delay.

### Added

- A release bump no longer breaks the tests: they read the version from `package.json` instead of repeating the string.

## 0.2.2

- Turning the agent on starts the bridge: a staged release is launched as the supervisor, and a first run falls back to the packaged setup.
- Node built-ins load lazily inside `connect()`, so the agent module still imports when the sandbox offers none.
- Windows certificate trust follows the engine's own CA verdict. The bridge reads the local machine root store, so a per-user import no longer reports success while the bridge refuses to start.
- The packaged native engine stays at 0.2.1: its version is compiled into the binary, so `integrity.json` now pins `engine_version` separately and a JavaScript release no longer has to match it.

### Fixed

- A stale `supervisor.lock` no longer disables model sync. The lock records the owner's pid *and* start time, so a dead supervisor whose pid was reused by an unrelated process is reclaimed instead of answering "already running". This was the reported cause of Cursor showing no Magpie models while Magpie still showed the agent as connected.
- `status.json` now carries a heartbeat, so a supervisor that dies without a clean shutdown no longer leaves a frozen `connected` state that reads as healthy.
- A failed autostart is recorded in `autostart.log` instead of being swallowed, so a bridge that never started is diagnosable.
- The install tests run even where the OS temp directory refuses the native engine's certificate store.

## 0.2.1

- Hide Cursor Auto in the bridge model picker using Cursor's display configuration, including clearing cached Auto controls. Magpie models and route groups remain selectable.

## 0.2.0

- Standard Magpie Agent package with an explicit file allowlist and bundled JavaScript dependency.
- Independent, per-machine certificate initialization; no original Cursor BYOK installation needed.
- Windows trust checks the generated CA thumbprint, so an older Cursor BYOK root with the same name is not treated as trusted.
- The agent module imports only relative files, so Magpie's plugin sandbox can load it.
- Explicit macOS certificate trust step with SSL trust verification; existing certificates preserved.
- Setup reuses Magpie's Bun and keeps a local runtime copy, removing the dependency on user-installed Node.
- Plugin/native version pairing and native SHA-256 verification.
- Complete version directories and rollback of launch configuration after failed service registration.
- Status reports the actual running version. Updating the plugin requires rerunning setup to activate the matching service.
- Apple Silicon macOS only; no installation lifecycle scripts and no credentials in the package.

## 0.1.1

- Return only Magpie's model catalogue, without appending Cursor subscription models.

## 0.1.0

- Cursor protocol bridge through Magpie, background lifecycle, and recoverable proxy settings.
