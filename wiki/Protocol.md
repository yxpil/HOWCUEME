# HOWCUEME HTTP Protocol / HOWCUEME HTTP 协议

HOWCUEME `serve` mode exposes a small HTTP API on `127.0.0.1:8752` by default (`--host/--port` overridable). All responses are JSON. An internal poller runs the rules too — one process, both roles.

HOWCUEME `serve` 模式默认在 `127.0.0.1:8752` 提供一个小型 HTTP API（可用 `--host/--port` 覆盖）。所有响应均为 JSON。进程内部同时运行规则轮询 —— 一个进程两种角色。

## Endpoints / 端点

| Endpoint / 端点 | Method / 方法 | Description / 说明 |
| --- | --- | --- |
| `/health` | GET | Liveness probe. Returns / 返回 `{"ok":true}` |
| `/rules` | GET | All rules + persisted state (last trigger per rule, file mtime snapshots) / 全部规则与持久化状态 |
| `/invoke` | POST | BIT Remote protocol entry / BIT Remote 协议入口 |

## POST /invoke

Request body (BIT Remote payload) / 请求体（BIT Remote 载荷）:

```json
{
  "tool_id": "tool-uuid",
  "tool": "howcueme",
  "invoked_by": "bit-agent",
  "params": { "action": "fire", "rule": "wake-bit-daily" }
}
```

Routing reads `params.action`, falling back to `params.tool`. Valid actions / 路由读取 `params.action`（回退 `params.tool`）。合法 action：

| action | Params / 参数 | Response payload / 响应载荷 |
| --- | --- | --- |
| `status` | — | `{"ok":true,"status":{...}}` — daemon/poller state / 守护与轮询状态 |
| `list` | — | `{"ok":true,"rules":[...], "state":{...}}` — rules + last trigger times / 规则与最近触发时间 |
| `fire` | `rule`（必填 / required） | `{"ok":true,"event":{...}}` — force-triggers the rule now, bypassing condition **and** cooldown; the event carries `"forced":true` / 强制触发，绕过条件与冷却 |
| `validate` | — | `{"ok":true,"rules_count":N,"rules":[...],"errors":[]}` — same shape as `howcueme validate --json` / 与 CLI 校验输出同构 |

### Errors / 错误

| HTTP | Meaning / 含义 |
| --- | --- |
| 200 | Success; rule evaluation results are data, not errors / 成功；规则评估结果是数据不是错误 |
| 400 | Unknown `action`, or `fire` without `params.rule` — `{"ok":false,"error":"..."}` / 未知动作，或 `fire` 缺 `params.rule` |
| 404 | `fire` with an unknown rule name / `fire` 指定的规则不存在 |

## Outbound protocol: `wake_bit` / 出站协议：wake_bit

The `wake_bit` action POSTs to BIT's remote chat endpoint / 该动作 POST 到 BIT 的远程对话端点:

```
POST {bit_url}/api/chat
Authorization: Bearer <client_key>      # BIT's client key / BIT 的 client key
Content-Type: application/json

{"message": "<prompt>"}
# -> {"reply":"...","messages":[...]}
```

Verify BIT is reachable first / 先验证 BIT 可达:

```bash
curl -s -X POST http://127.0.0.1:8600/api/chat \
  -H "Authorization: Bearer <BIT_client_key>" \
  -H "Content-Type: application/json" \
  -d '{"message": "hello from howcueme"}'
```

## Trigger output contract / 触发输出契约

A human log line goes to **stderr**; the action result JSON line goes to **stdout**:

人读日志走 **stderr**；动作结果 JSON 行走 **stdout**：

```json
{"rule":"manual-test","triggered_at":"2026-09-04T09:20:02Z","ok":true,"forced":false,"action":{"type":"command","cmd":"echo","args":["woken-up"]},"when":{"type":"file","path":"...","op":"exists"},"when_result":{"exists":true,"mtime_nanos":1788513602920677035},"result":{"exit_code":0,"stdout":"woken-up\n","stderr":""}}
```

- `forced`: `true` when triggered via `fire` / 经 `fire` 触发时为 true
- `when_result`: the observation that matched (file mtime, HTTP status, process check...) / 命中的观测值
- `result`: the action result (HTTP response for `webhook`/`wake_bit`, exit code + stdio for `command`) / 动作结果

## CLI exit codes / CLI 退出码

| Command / 命令 | Codes / 退出码 |
| --- | --- |
| `validate` | `0` rules valid / 规则合法 · `1` errors found / 有错误 |
| `run --once` | `0` (rule firings are data / 触发是数据不是错误) |
| other commands / 其他命令 | `0` success / `2` error |

## Example session / 示例会话

```bash
howcueme serve --port 8752

curl -s http://127.0.0.1:8752/health
# -> {"ok":true}

curl -s http://127.0.0.1:8752/rules

curl -s -X POST http://127.0.0.1:8752/invoke -H 'Content-Type: application/json' \
     -d '{"params":{"action":"status"}}'

curl -s -X POST http://127.0.0.1:8752/invoke -H 'Content-Type: application/json' \
     -d '{"tool_id":"t1","tool":"howcueme","invoked_by":"manual","params":{"action":"fire","rule":"wake-bit-daily"}}'
# -> {"ok":true,"event":{"rule":"wake-bit-daily","forced":true,"ok":true,"result":{...}}}

curl -s -X POST http://127.0.0.1:8752/invoke -H 'Content-Type: application/json' \
     -d '{"params":{"action":"validate"}}'
```
