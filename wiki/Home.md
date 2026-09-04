# HOWCUEME Wiki / HOWCUEME 维基

**HOWCUEME** — Conditional self-wakeup daemon for AI agents around the [BIT](https://github.com/yxpil/bit) ecosystem: a small daemon polls rules (time intervals, daily times, file changes, HTTP probes, process presence) and fires actions — webhook POST, local command, or `wake_bit` that calls BIT's remote chat API. The agent defines *when* it wants to be woken; HOWCUEME does the waiting.

**HOWCUEME** —— 面向 [BIT](https://github.com/yxpil/bit) 生态 AI 智能体的条件自唤醒守护进程：一个小型守护进程按固定间隔轮询规则（时间间隔、每日时刻、文件变化、HTTP 探测、进程存在），条件满足时执行动作 —— webhook POST、本地命令，或调用 BIT 远程对话接口的 `wake_bit`。智能体自己定义"何时想被唤醒"，HOWCUEME 负责等待与触发。

- Repo / 仓库: <https://github.com/yxpil/HOWCUEME>
- Releases / 发行版: <https://github.com/yxpil/HOWCUEME/releases>
- Binary name / 二进制名: `howcueme`
- Default port / 默认端口: `8752`
- Data dir / 数据目录: `~/.howcueme/`（`rules.toml` + `state.json`；env / 环境变量 `HOWCUEME_DATA_DIR` 可覆盖，`-c <file>` 可指定规则文件）

---

## Install / 安装

Grab a per-platform binary from the [latest release](https://github.com/yxpil/HOWCUEME/releases/latest) (`tar.gz` for macOS/Linux, `zip` for Windows), or:

从 [最新 Release](https://github.com/yxpil/HOWCUEME/releases/latest) 下载对应平台二进制（macOS/Linux 为 `tar.gz`，Windows 为 `zip`），或：

```bash
cargo install --git https://github.com/yxpil/HOWCUEME
```

## Usage / 使用

```bash
howcueme validate              # check the rules file, exit 0/1 / 校验规则文件
howcueme validate --json       # {"ok":true,"rules_count":6,"rules":[...],"errors":[]}
howcueme run                   # daemon: poll every 5s / 守护进程轮询
howcueme run --interval 10     # custom poll interval / 自定义轮询间隔
howcueme run --once            # evaluate one round and exit / 评估一轮即退出
howcueme list                  # rules + last trigger times / 规则与最近触发时间
howcueme fire <name>           # force-trigger a rule now (test helper) / 强制触发
howcueme serve                 # HTTP API on 127.0.0.1:8752 (+ internal poller / 含内部轮询)
```

Every subcommand accepts `--json`; piped stdin JSON merges over CLI args (stdin wins — the BIT exec contract). Trigger logs go to **stderr**, action result JSON lines go to **stdout**:

所有子命令支持 `--json`；管道 stdin 的 JSON 会合并覆盖 CLI 参数（stdin 优先，即 BIT exec 契约）。触发日志走 **stderr**，动作结果 JSON 行走 **stdout**：

```json
{"rule":"manual-test","triggered_at":"2026-09-04T09:20:02Z","ok":true,"forced":false,"action":{...},"when":{...},"when_result":{...},"result":{...}}
```

## Rules / 规则

`~/.howcueme/rules.toml` — each rule has `name`, optional `cooldown_secs` (anti-storm / 防触发风暴), `enabled`, exactly one `when` and one `action`:

每条规则含 `name`、可选 `cooldown_secs`、`enabled`，以及唯一的 `when` 与 `action`：

| `when.type` / 条件 | Fields / 字段 | Fires when / 触发时机 |
|---|---|---|
| `interval` | `every_secs` | every N seconds since last trigger (first poll fires immediately) / 距上次触发满 N 秒（首轮立即触发） |
| `daily` | `at` = `"HH:MM"` | at most once per local calendar day, catch-up if the daemon starts later / 每个本地日最多一次，迟启动会补触发 |
| `file` | `path`, `op = "exists" \| "changed"` | path exists (debounced) or mtime differs from the snapshot (first observation is baseline) / 路径存在（去抖）或 mtime 变化（首次观测仅记基线） |
| `http` | `url`, `expect_status` (200), `timeout_secs` (5) | HTTP GET status equals `expect_status` / GET 状态码等于期望值 |
| `process` | `name`, `op = "exists" \| "absent"` | process running / not running / 进程在跑 / 不在跑 |

| `action.type` / 动作 | Fields / 字段 | Behavior / 行为 |
|---|---|---|
| `webhook` | `url` | POST JSON payload / POST JSON 载荷 |
| `command` | `cmd`, `args?` | direct exec, **no shell**, cross-platform / 直接执行，**不经 shell**，跨平台 |
| `wake_bit` | `bit_url`, `client_key`, `prompt` | `POST {bit_url}/api/chat` with `Authorization: Bearer <client_key>` and `{"message": prompt}` — wakes a BIT agent / 唤醒 BIT 智能体 |

State (`state.json`) persists last trigger time per rule + file mtime snapshots and survives restarts. TOML note: `cooldown_secs`/`enabled` may sit before or after `[rule.when]`/`[rule.action]` — both are lifted back to the rule level.

`state.json` 持久化每条规则的最近触发时间与文件 mtime 快照，重启不丢。TOML 说明：`cooldown_secs`/`enabled` 写在 `[rule.when]`/`[rule.action]` 前后均可，会自动归位。

Example rule / 规则示例:

```toml
[[rule]]
name = "wake-bit-on-queue"
cooldown_secs = 300
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

## BIT integration / BIT 集成

Three ways / 三种方式:

1. **CLI (exec runtime)** — register two Interpreter tools: `code: "run --once"` (condition-driven poll; BIT calls it whenever it wants a round) and `code: "fire"` with `params: {"rule": "..."}` (the agent decides *what* to trigger). BIT pipes `params` JSON to stdin, reads JSON from stdout. / 注册 `run --once` 与 `fire` 两个 Interpreter 工具。
2. **Remote tool** — run `howcueme serve`, register URL `http://127.0.0.1:8752/invoke`; `params.action ∈ status | list | fire | validate` (`fire` needs `params.rule`). / 启动 serve 后注册 Remote 工具。
3. **Plain REST** — `GET /health`, `GET /rules`, `POST /invoke` (see [Protocol](Protocol)); and outbound: `wake_bit` → `POST {bit_url}/api/chat`.

### The self-wakeup loop / 自唤醒闭环

1. The BIT agent uses its `write_file` tool to edit `~/.howcueme/rules.toml` — declaring *when* it wants to be woken.
2. The `howcueme` daemon keeps polling those conditions.
3. When a condition fires, `wake_bit` POSTs to BIT's `/api/chat` — the agent wakes with a prompt and continues working (or re-arms the rules).

No human in the loop: the agent schedules its own wake-ups.

1. BIT 智能体用 `write_file` 工具编辑 `~/.howcueme/rules.toml`，声明"何时想被唤醒"。
2. `howcueme` 守护进程持续轮询这些条件。
3. 条件满足时 `wake_bit` POST 到 BIT 的 `/api/chat` —— 智能体带着 prompt 醒来继续工作（或重新布置下一轮规则）。

无人参与：智能体给自己排程唤醒。

## FAQ

**Q: Where are rules and state stored? / 规则和状态存在哪里？**
`~/.howcueme/rules.toml` and `~/.howcueme/state.json` (relocate the directory with `HOWCUEME_DATA_DIR`, or point `-c` at a specific rules file).

`~/.howcueme/rules.toml` 与 `~/.howcueme/state.json`（用 `HOWCUEME_DATA_DIR` 换目录，或用 `-c` 指定规则文件）。

**Q: Difference between `run` and `serve`? / `run` 和 `serve` 有什么区别？**
`run` is the pure polling daemon; `serve` runs an internal poller **and** the HTTP API on `127.0.0.1:8752` — one process, both roles.

`run` 是纯轮询守护；`serve` 在 `127.0.0.1:8752` 起 HTTP API 的**同时**也运行内部轮询 —— 一个进程两种角色。

**Q: Can a rule fire twice in a row? / 同一规则会连续触发吗？**
Not within `cooldown_secs`. Per-rule cooldowns debounce trigger storms; `daily` fires at most once per calendar day; `file changed` needs a new mtime.

`cooldown_secs` 内不会。每规则冷却防风暴；`daily` 每天最多一次；`file changed` 需要 mtime 再次变化。

**Q: Does the `command` action run through a shell? / `command` 动作走 shell 吗？**
No — direct exec with argv, cross-platform, no quoting pitfalls.

不走 —— 直接按 argv 执行，跨平台，没有引号转义坑。

**Q: `fire` bypasses the condition — and the cooldown? / `fire` 会绕过条件和冷却吗？**
It bypasses both. It is a test helper / manual override, marked `"forced": true` in the output.

都绕过。它是测试/手动触发辅助，输出中标记 `"forced": true`。

---

Part of the [BIT](https://github.com/yxpil/bit) ecosystem.
