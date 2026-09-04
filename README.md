# HOWCUEME

> Conditional self-wakeup daemon for AI agents — built for the [BIT](https://github.com/yxpil/bit) ecosystem.

[![Release](https://img.shields.io/github/v/release/yxpil/HOWCUEME?style=flat-square)](https://github.com/yxpil/HOWCUEME/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/yxpil/HOWCUEME/total?style=flat-square)](https://github.com/yxpil/HOWCUEME/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-black?style=flat-square)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/yxpil/HOWCUEME/ci.yml?style=flat-square&label=CI)](https://github.com/yxpil/HOWCUEME/actions/workflows/ci.yml)
[![Platform](https://img.shields.io/badge/platform-linux%20%7C%20macOS%20%7C%20Windows-black?style=flat-square)](https://github.com/yxpil/HOWCUEME/releases)

---

## English

### About

HOWCUEME lets an AI agent (especially [BIT](https://github.com/yxpil/bit)) **wake itself up when a condition is met**. A small daemon polls rules on a fixed interval — time intervals, daily times, file changes, HTTP probes, process presence — and when a condition fires it executes an action: a **webhook** POST, a local **command**, or a **wake_bit** call that talks to BIT's remote chat API (`POST /api/chat`). The result is unattended, condition-driven automation: the agent defines *when* it wants to be woken, HOWCUEME does the waiting.

### Features

- **5 condition types** (`when`, exactly one per rule):
  | Type | Fields | Fires when |
  |---|---|---|
  | `interval` | `every_secs` | every N seconds since last trigger (first poll fires immediately) |
  | `daily` | `at` = `"HH:MM"` | at most once per local calendar day, at or after the configured time (catch-up if the daemon starts later) |
  | `file` | `path`, `op = "exists" \| "changed"` | `exists`: path exists (debounce with `cooldown_secs`); `changed`: mtime differs from the last observed snapshot (first observation records a baseline, does not fire) |
  | `http` | `url`, `expect_status` (default 200), `timeout_secs` (default 5) | HTTP GET status equals `expect_status` |
  | `process` | `name`, `op = "exists" \| "absent"` | process name is running / not running (via `sysinfo`) |
- **3 action types** (`action`): `webhook` (POST JSON), `command` (direct exec, **no shell**, cross-platform), `wake_bit` (POST `{bit_url}/api/chat` with `Authorization: Bearer <client_key>` and `{"message": prompt}` to wake a BIT agent).
- **Cooldown** (`cooldown_secs`) per rule prevents trigger storms.
- **Persistent state** (`state.json`): last trigger time per rule + file mtime snapshots, survives restarts.
- **Daemon or one-shot**: `run` polls every 5 s (adjustable); `run --once` evaluates a single round and exits — ideal for tests and BIT exec calls.
- **HTTP API** in `serve` mode on `127.0.0.1:8752`: `GET /health`, `GET /rules`, `POST /invoke` (BIT Remote protocol), MCP (Streamable HTTP JSON-RPC) on `POST /mcp` and `POST /`.
- **BIT exec-mode contract**: every subcommand accepts `--json`; when stdin is a pipe, a JSON object is read and merged over CLI args (stdin wins).

### Install

**Option A — release binaries** (from [Releases](https://github.com/yxpil/HOWCUEME/releases/latest), CI attaches them automatically):

| Platform | Archive |
|---|---|
| Linux x86_64 | `howcueme-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `howcueme-v0.1.0-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `howcueme-v0.1.0-x86_64-pc-windows-msvc.zip` |

```bash
tar -xzf howcueme-v0.1.0-aarch64-apple-darwin.tar.gz
sudo mv howcueme /usr/local/bin/ && howcueme --version
```

**Option B — from source:**

```bash
git clone https://github.com/yxpil/HOWCUEME.git && cd HOWCUEME
cargo build --release
# binary at target/release/howcueme
```

### Quick start

Data lives under `~/.howcueme/` (`rules.toml`, `state.json`). Set `HOWCUEME_DATA_DIR` to relocate; pass `-c <file>` to use a specific rules file.

`~/.howcueme/rules.toml` — one example per condition/action type:

```toml
# Wake BIT every 60s via webhook (cooldown 300s debounces)
[[rule]]
name = "wake-bit-interval"
cooldown_secs = 300
enabled = true
[rule.when]
type = "interval"
every_secs = 60
[rule.action]
type = "webhook"
url = "http://127.0.0.1:8753/hook"

# Daily report at 09:30 local time
[[rule]]
name = "daily-report"
[rule.when]
type = "daily"
at = "09:30"
[rule.action]
type = "command"
cmd = "echo"
args = ["time for the daily report"]

# Fire when a watched file changes (mtime snapshot stored in state.json)
[[rule]]
name = "queue-file-changed"
[rule.when]
type = "file"
path = "~/bitdata/queue.json"
op = "changed"
[rule.action]
type = "command"
cmd = "echo"
args = ["queue changed"]

# Fire when an HTTP probe returns the expected status
[[rule]]
name = "api-up"
[rule.when]
type = "http"
url = "http://127.0.0.1:8600/api/health"
expect_status = 200
timeout_secs = 5
[rule.action]
type = "command"
cmd = "echo"
args = ["BIT API is up"]

# Fire when the bit process disappears
[[rule]]
name = "bit-gone"
[rule.when]
type = "process"
name = "bit"
op = "absent"
[rule.action]
type = "webhook"
url = "http://127.0.0.1:9000/alert"

# Wake a BIT agent through its remote chat API
[[rule]]
name = "wake-bit-daily"
[rule.when]
type = "daily"
at = "09:30"
[rule.action]
type = "wake_bit"
bit_url = "http://127.0.0.1:8600"
client_key = "<BIT client key>"
prompt = "Good morning! Run the daily report workflow."
```

> TOML note: `cooldown_secs` / `enabled` may be written before `[rule.when]` / `[rule.action]` (recommended) or after them — HOWCUEME lifts them back to the rule level automatically.

Run it:

```bash
howcueme validate                 # check the rules file, exit 0/1
howcueme validate --json          # {"ok":true,"rules_count":6,"rules":[...],"errors":[]}
howcueme run                      # daemon: poll every 5s (Ctrl-C to stop)
howcueme run --interval 10        # daemon with custom poll interval
howcueme run --once               # evaluate one round and exit (no daemon)
howcueme list                     # rules + last trigger times (from state.json)
howcueme list --json
howcueme fire wake-bit-daily      # force-trigger a rule now (test helper)
howcueme serve                    # HTTP API on 127.0.0.1:8752 (+ internal poller)
howcueme serve --port 9000        # custom port; --host to bind elsewhere
```

Trigger output: a human log line goes to **stderr**, and the action result JSON line goes to **stdout**:

```json
{"rule":"manual-test","triggered_at":"2026-09-04T09:20:02Z","ok":true,"forced":false,"action":{"type":"command","cmd":"echo","args":["woken-up"]},"when":{"type":"file","path":"...","op":"exists"},"when_result":{"exists":true,"mtime_nanos":1788513602920677035},"result":{"exit_code":0,"stdout":"woken-up\n","stderr":""}}
```

### BIT Integration

HOWCUEME integrates with BIT in three ways (see [bit](https://github.com/yxpil/bit)).

**Way 1 — CLI tools (BIT `exec` runtime).** BIT spawns the binary, sends `params` JSON via **stdin** (merged over CLI args, stdin wins), and reads JSON from **stdout**; exit code 0 = success. Two useful registrations, as stored in BIT's `tools.json` (or returned by `GET /api/tools`):

Evaluate one round (condition-driven wakeup — BIT calls this whenever it wants a poll to happen):

```json
{
  "id": "tool.howcueme.once",
  "name": "howcueme_once",
  "description": "Evaluate howcueme rules for one round; any rule whose condition is met fires its action. Params are merged over CLI args via stdin.",
  "parameters": {
    "type": "object",
    "properties": {
      "config": { "type": "string", "description": "Optional path to a rules.toml" },
      "interval": { "type": "integer", "description": "Ignored in --once mode" }
    }
  },
  "kind": { "kind": "interpreter", "runtime": "exec", "code": "run --once" },
  "created_by": "user",
  "created_at": "2026-09-04 09:00:00",
  "enabled": true
}
```

Force-fire one rule (the agent decides *what* to trigger; params `{"rule": "..."}` arrive on stdin):

```json
{
  "id": "tool.howcueme.fire",
  "name": "howcueme_fire",
  "description": "Force-trigger a howcueme rule right now, bypassing its condition and cooldown. Params: {\"rule\": \"<rule name>\"}.",
  "parameters": {
    "type": "object",
    "properties": {
      "rule": { "type": "string", "description": "Rule name to trigger" }
    },
    "required": ["rule"]
  },
  "kind": { "kind": "interpreter", "runtime": "exec", "code": "fire" },
  "created_by": "user",
  "created_at": "2026-09-04 09:00:00",
  "enabled": true
}
```

**Way 2 — Remote tool (HTTP serve mode).** Start `howcueme serve` (binds `127.0.0.1:8752`), then register:

```json
{
  "id": "tool.howcueme.remote",
  "name": "howcueme",
  "description": "howcueme daemon control: params.action = status | list | fire | validate. fire needs params.rule.",
  "parameters": {
    "type": "object",
    "properties": {
      "action": { "type": "string", "enum": ["status", "list", "fire", "validate"] },
      "rule": { "type": "string", "description": "Rule name, required for action=fire" }
    },
    "required": ["action"]
  },
  "kind": { "kind": "remote", "url": "http://127.0.0.1:8752/invoke" },
  "created_by": "user",
  "created_at": "2026-09-04 09:00:00",
  "enabled": true
}
```

Test the same call with curl:

```bash
curl -s -X POST http://127.0.0.1:8752/invoke \
  -H 'Content-Type: application/json' \
  -d '{"tool_id":"t1","tool":"howcueme","invoked_by":"manual","params":{"action":"fire","rule":"wake-bit-daily"}}'
# {"ok":true,"event":{"rule":"wake-bit-daily","forced":true,"ok":true,"result":{...}}}
```

**Way 3 — HTTP API (`wake_bit` → BIT).** BIT exposes a remote chat endpoint: `POST http://127.0.0.1:8600/api/chat` with `Authorization: Bearer <client_key>` (BIT's client key) and body `{"message": "..."}`. The `wake_bit` action does exactly this. Verify BIT is reachable first:

```bash
curl -s -X POST http://127.0.0.1:8600/api/chat \
  -H "Authorization: Bearer <BIT_client_key>" \
  -H "Content-Type: application/json" \
  -d '{"message": "hello from howcueme"}'
# {"reply":"...","messages":[...]}
```

And the matching rule:

```toml
[[rule]]
name = "wake-bit-on-queue"
[rule.when]
type = "file"
path = "~/bitdata/queue.json"
op = "changed"
[rule.action]
type = "wake_bit"
bit_url = "http://127.0.0.1:8600"
client_key = "<BIT client key>"
prompt = "The task queue changed. Please process new entries."
```

**The self-wakeup loop.** Put together, HOWCUEME closes the loop for unattended agents:

1. The agent (BIT) uses its `write_file` tool to edit `~/.howcueme/rules.toml` — declaring *when* it wants to be woken (tomorrow 09:00, when a file changes, when a service dies, ...).
2. The `howcueme` daemon keeps polling those conditions.
3. When a condition fires, the `wake_bit` action POSTs to BIT's `/api/chat` — the agent wakes up with a prompt and continues working (or re-arms the rules for the next cycle).

No human in the loop: the agent schedules its own wake-ups.

### API

`serve` mode (default `127.0.0.1:8752`, `--host/--port` overridable; an internal poller runs the rules too):

| Endpoint | Method | Description |
|---|---|---|
| `/health` | GET | Liveness probe → `{"ok":true}` |
| `/rules` | GET | All rules + persisted state (last trigger, mtime snapshots) |
| `/invoke` | POST | BIT Remote tool entry: `{"tool_id":"...","tool":"...","invoked_by":"...","params":{...}}`, routed on `params.action` (or `params.tool`): `status` \| `list` \| `fire` \| `validate`; `fire` requires `params.rule` |
| `/mcp`, `/` | POST | MCP Streamable HTTP JSON-RPC: `initialize`, `tools/list`, `tools/call`, `ping` — the four actions surface as four tools (`status`, `list`, `fire`, `validate`; `fire` takes `{"rule": "..."}`) |

`/invoke` examples:

```bash
curl -s http://127.0.0.1:8752/health
curl -s http://127.0.0.1:8752/rules
curl -s -X POST http://127.0.0.1:8752/invoke -H 'Content-Type: application/json' \
  -d '{"params":{"action":"status"}}'
curl -s -X POST http://127.0.0.1:8752/invoke -H 'Content-Type: application/json' \
  -d '{"params":{"action":"fire","rule":"wake-bit-daily"}}'
curl -s -X POST http://127.0.0.1:8752/invoke -H 'Content-Type: application/json' \
  -d '{"params":{"action":"validate"}}'
```

Errors: unknown action or missing `params.rule` → HTTP 400 with `{"ok":false,"error":"..."}`; unknown rule → HTTP 404.

### License

Apache-2.0. See [LICENSE](LICENSE).

---

## 中文

### 简介

HOWCUEME 让 AI 智能体（尤其是 [BIT](https://github.com/yxpil/bit)）**在满足条件时"自己醒来"**。一个小型守护进程按固定间隔轮询规则——时间间隔、每日时刻、文件变化、HTTP 探测、进程存在——条件满足时执行动作：**webhook** POST、本地**命令**，或 **wake_bit**（调用 BIT 远程对话接口 `POST /api/chat` 唤醒智能体）。由此实现无人值守的、条件驱动的自动化：智能体自己定义"何时想被唤醒"，HOWCUEME 负责等待与触发。

### 功能

- **5 种触发条件**（`when`，每条规则五选一）：
  | 类型 | 字段 | 触发时机 |
  |---|---|---|
  | `interval` | `every_secs` | 距上次触发每 N 秒（首次轮询立即触发） |
  | `daily` | `at` = `"HH:MM"` | 每个本地日最多一次，到达或晚于配置时刻即触发（守护进程晚启动会补触发） |
  | `file` | `path`、`op = "exists" \| "changed"` | `exists`：路径存在（配合 `cooldown_secs` 防抖）；`changed`：mtime 与上次快照不同（首次观察只记录基线不触发） |
  | `http` | `url`、`expect_status`（默认 200）、`timeout_secs`（默认 5） | HTTP GET 状态码等于 `expect_status` |
  | `process` | `name`、`op = "exists" \| "absent"` | 进程名正在运行 / 不在运行（基于 `sysinfo`） |
- **3 种动作**（`action`）：`webhook`（POST JSON）、`command`（直接执行，**不经 shell**，跨平台安全）、`wake_bit`（POST `{bit_url}/api/chat`，携带 `Authorization: Bearer <client_key>` 与 `{"message": prompt}`，唤醒 BIT 智能体）。
- **冷却**（`cooldown_secs`）逐规则防抖，避免触发风暴。
- **状态持久化**（`state.json`）：每条规则的上次触发时间 + 文件 mtime 快照，重启不丢。
- **守护或单次**：`run` 默认每 5 秒轮询（可调）；`run --once` 只评估一轮立即退出——适合测试与 BIT exec 调用。
- **HTTP API**：`serve` 模式监听 `127.0.0.1:8752`：`GET /health`、`GET /rules`、`POST /invoke`（BIT Remote 协议）、MCP（Streamable HTTP JSON-RPC，`POST /mcp` 与 `POST /`）。
- **BIT exec 契约**：所有子命令支持 `--json`；stdin 为管道时读取 JSON 对象并合并覆盖 CLI 参数（stdin 优先）。

### 安装

**方式一 —— 发布二进制**（见 [Releases](https://github.com/yxpil/HOWCUEME/releases/latest)，由 CI 自动附加）：

| 平台 | 压缩包 |
|---|---|
| Linux x86_64 | `howcueme-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `howcueme-v0.1.0-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `howcueme-v0.1.0-x86_64-pc-windows-msvc.zip` |

**方式二 —— 源码构建：**

```bash
git clone https://github.com/yxpil/HOWCUEME.git && cd HOWCUEME
cargo build --release
# 二进制位于 target/release/howcueme
```

### 快速上手

数据目录为 `~/.howcueme/`（`rules.toml`、`state.json`）。用 `HOWCUEME_DATA_DIR` 可覆盖目录；`-c <文件>` 指定规则文件。

规则文件写法见上文英文版 `Quick start` 中的完整示例（覆盖全部 5 种条件与 3 种动作，可直接复制修改）。

```bash
howcueme validate                 # 校验规则文件，退出码 0/1
howcueme run                      # 守护：每 5 秒轮询（Ctrl-C 停止）
howcueme run --once               # 只评估一轮立即退出（不驻留）
howcueme list                     # 规则 + 上次触发时间（读 state.json）
howcueme fire wake-bit-daily      # 手动强制触发某规则（测试用）
howcueme serve                    # HTTP API：127.0.0.1:8752（内部同样跑轮询）
howcueme serve --port 9000        # 自定义端口；--host 可改绑定地址
```

触发时：stderr 打人类可读日志，stdout 输出一行动作结果 JSON（格式见上文英文版）。

### BIT 集成

HOWCUEME 以三种方式接入 BIT（详见 [bit](https://github.com/yxpil/bit)）。

**方式一 —— CLI 工具（BIT `exec` 运行时）。** BIT 启动二进制，把 `params` JSON 通过 **stdin** 写入（与 CLI 参数合并，stdin 优先），从 **stdout** 读取 JSON；退出码 0 = 成功。两个常用注册片段（与 BIT `tools.json` 中的存储格式一致）：

评估一轮（条件驱动唤醒——BIT 想轮询时调用它）：

```json
{
  "id": "tool.howcueme.once",
  "name": "howcueme_once",
  "description": "评估 howcueme 规则一轮；条件满足的规则立即执行动作。params 通过 stdin 合并到 CLI 参数。",
  "parameters": {
    "type": "object",
    "properties": {
      "config": { "type": "string", "description": "可选，rules.toml 路径" },
      "interval": { "type": "integer", "description": "--once 模式下忽略" }
    }
  },
  "kind": { "kind": "interpreter", "runtime": "exec", "code": "run --once" },
  "created_by": "user",
  "created_at": "2026-09-04 09:00:00",
  "enabled": true
}
```

强制触发某规则（智能体决定"触发什么"；params `{"rule": "..."}` 走 stdin）：

```json
{
  "id": "tool.howcueme.fire",
  "name": "howcueme_fire",
  "description": "立即强制触发一条 howcueme 规则，绕过条件与冷却。params：{\"rule\": \"<规则名>\"}。",
  "parameters": {
    "type": "object",
    "properties": {
      "rule": { "type": "string", "description": "要触发的规则名" }
    },
    "required": ["rule"]
  },
  "kind": { "kind": "interpreter", "runtime": "exec", "code": "fire" },
  "created_by": "user",
  "created_at": "2026-09-04 09:00:00",
  "enabled": true
}
```

**方式二 —— Remote 工具（HTTP serve 模式）。** 先运行 `howcueme serve`（监听 `127.0.0.1:8752`），再注册：

```json
{
  "id": "tool.howcueme.remote",
  "name": "howcueme",
  "description": "howcueme 守护进程控制：params.action = status | list | fire | validate。fire 需要 params.rule。",
  "parameters": {
    "type": "object",
    "properties": {
      "action": { "type": "string", "enum": ["status", "list", "fire", "validate"] },
      "rule": { "type": "string", "description": "规则名，action=fire 时必填" }
    },
    "required": ["action"]
  },
  "kind": { "kind": "remote", "url": "http://127.0.0.1:8752/invoke" },
  "created_by": "user",
  "created_at": "2026-09-04 09:00:00",
  "enabled": true
}
```

用 curl 测试同样的调用：

```bash
curl -s -X POST http://127.0.0.1:8752/invoke \
  -H 'Content-Type: application/json' \
  -d '{"tool_id":"t1","tool":"howcueme","invoked_by":"manual","params":{"action":"fire","rule":"wake-bit-daily"}}'
```

**方式三 —— HTTP API（`wake_bit` → BIT）。** BIT 提供远程对话端点：`POST http://127.0.0.1:8600/api/chat`，请求头 `Authorization: Bearer <client_key>`（BIT 的 Client Key），请求体 `{"message": "..."}`。`wake_bit` 动作做的正是这件事。先验证 BIT 可达：

```bash
curl -s -X POST http://127.0.0.1:8600/api/chat \
  -H "Authorization: Bearer <BIT的client_key>" \
  -H "Content-Type: application/json" \
  -d '{"message": "hello from howcueme"}'
```

对应规则：

```toml
[[rule]]
name = "wake-bit-on-queue"
[rule.when]
type = "file"
path = "~/bitdata/queue.json"
op = "changed"
[rule.action]
type = "wake_bit"
bit_url = "http://127.0.0.1:8600"
client_key = "<BIT的client_key>"
prompt = "任务队列有变化，请处理新条目。"
```

**自唤醒闭环。** 组合起来，HOWCUEME 为无人值守智能体闭合了回路：

1. 智能体（BIT）用 `write_file` 工具修改 `~/.howcueme/rules.toml` —— 声明自己"何时想被唤醒"（明天 09:00、某个文件变化时、某个服务挂掉时……）。
2. `howcueme` 守护进程持续轮询这些条件。
3. 条件满足时，`wake_bit` 动作 POST 到 BIT 的 `/api/chat` —— 智能体带着提示词醒来，继续工作（或为下一轮重新布置规则）。

全程无需人工介入：智能体为自己安排唤醒。

### API

`serve` 模式（默认 `127.0.0.1:8752`，`--host/--port` 可覆盖；内部同样运行轮询循环）：

| 端点 | 方法 | 说明 |
|---|---|---|
| `/health` | GET | 存活探测 → `{"ok":true}` |
| `/rules` | GET | 全部规则 + 持久化状态（上次触发、mtime 快照） |
| `/invoke` | POST | BIT Remote 工具入口：`{"tool_id":"...","tool":"...","invoked_by":"...","params":{...}}`，按 `params.action`（或 `params.tool`）路由：`status` \| `list` \| `fire` \| `validate`；`fire` 需要 `params.rule` |
| `/mcp`、`/` | POST | MCP Streamable HTTP JSON-RPC：`initialize`、`tools/list`、`tools/call`、`ping`——四个动作以四个工具暴露（`status`、`list`、`fire`、`validate`；`fire` 入参 `{"rule": "..."}`） |

错误：未知 action 或缺少 `params.rule` → HTTP 400，返回 `{"ok":false,"error":"..."}`；规则不存在 → HTTP 404。

### 安全与合规

- `client_key` 是 BIT 的访问凭据：请只在受信任的本机/内网使用，不要把包含 `client_key` 的 `rules.toml` 提交到版本库。
- `command` 动作不经过 shell：参数按原样传给可执行文件，无注入面；但请勿把不可信来源可控的路径/参数直接放进规则。
- `serve` 默认只绑定 `127.0.0.1`，不暴露到公网；如需远程访问，请自行前置网关与鉴权。
- 本项目为 Apache-2.0 许可（见 [LICENSE](LICENSE)），是 BIT 生态的独立卫星工具，与 BIT 主程序相互独立。

---

Part of the **BIT ecosystem** — the agent hub that woke you up: [github.com/yxpil/bit](https://github.com/yxpil/bit)
