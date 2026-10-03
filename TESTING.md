# HOWCUEME 测试说明

单 crate（`src/main.rs` 二进制 + 内嵌模块）。单元测试留在各 `src/*.rs` 的 `#[cfg(test)]`，
集成测试放仓库根 `tests/`（`cli.rs`、`mcp.rs` 既有；本次新增 `injection.rs`、`hooks.rs`）。

## 测试放在哪里

| 位置 | 类型 | 覆盖 |
|---|---|---|
| `src/config.rs` | 单元 | 规则 TOML 解析、语义校验、URL scheme 守卫 |
| `src/cond.rs` | 单元 | interval/daily/file/http/process 条件求值 |
| `src/action.rs` | 单元 | webhook/command/wake_bit 动作执行与错误报告 |
| `src/{state,poll,mcp}.rs` | 单元 | 状态、轮询、MCP 工具注册表 |
| `tests/cli.rs` | 集成 | CLI 端到端 |
| `tests/mcp.rs` | 集成 | MCP 握手/status/list/fire/validate 全链路 |
| `tests/injection.rs` | **集成（注入）** | 畸形 JSON、XSS 工具名、路径穿越/命令串规则名 |
| `tests/hooks.rs` | **集成（钩子）** | 工具注册表稳定顺序、未注册拒绝、失败隔离 |

## 怎么运行

```powershell
# 全量（交互式终端请给空 stdin，避免子进程阻塞读 stdin）
cargo test < NUL

# 仅单元
cargo test --bin howcueme

# 仅新增两类
cargo test --test injection   # 注入安全
cargo test --test hooks       # 钩子/规则触发
```

## 预期结果（本地基线）

`cargo test < NUL` 全部通过、0 失败：

- 单元：27 passed
- 集成：`cli` 4 + `mcp` 3 + `injection` 4 + `hooks` 3

### 本次补强新增（相对原有 29 个用例）

**单元 +5**（`src`）：
- `config::tests::validate_rejects_non_http_and_evil_schemes` — `file://` / `javascript:` / `gopher://` 被拒（SSRF/协议注入守卫）
- `config::tests::xss_rule_name_is_opaque_data_duplicates_still_detected` — XSS 规则名作为数据参与重名检测
- `action::tests::webhook_payload_safely_escapes_xss_and_quotes` — webhook body 始终合法 JSON，引号/脚本转义为字符串值
- `action::tests::command_args_are_literal_no_shell_injection` — `; && |` 参数字面传递，不经 shell
- `cond::tests::file_condition_handles_traversal_and_missing_paths_safely` — 穿越/缺失路径只报告不触发

**集成（注入）+4**（`tests/injection.rs`）：
`malformed_json_to_mcp_is_parse_error_not_crash`、`xss_tool_name_is_safely_embedded_in_json_error`、
`fire_with_traversal_or_command_string_name_is_not_found`、`unknown_action_to_invoke_is_400`。

**集成（钩子）+3**（`tests/hooks.rs`，规则/工具注册表）：
`registered_tools_listed_in_stable_order_and_each_routes`、
`unregistered_tool_is_rejected`、
`failed_trigger_does_not_break_sibling_tools`（fire 一个不存在规则失败后，status/list 兄弟工具仍可用）。
