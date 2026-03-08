# Configuration

For basic configuration instructions, see [this documentation](https://developers.openai.com/codex/config-basic).

For advanced configuration instructions, see [this documentation](https://developers.openai.com/codex/config-advanced).

For a full configuration reference, see [this documentation](https://developers.openai.com/codex/config-reference).

## Connecting to MCP servers

Codex can connect to MCP servers configured in `~/.codex/config.toml`. See the configuration reference for the latest MCP server options:

- https://developers.openai.com/codex/config-reference

## Apps (Connectors)

Use `$` in the composer to insert a ChatGPT connector; the popover lists accessible
apps. The `/apps` command lists available and installed apps. Connected apps appear first
and are labeled as connected; others are marked as can be installed.

## Notify

Codex can run a notification hook when the agent finishes a turn. See the configuration reference for the latest notification settings:

- https://developers.openai.com/codex/config-reference

When Codex knows which client started the turn, the legacy notify JSON payload also includes a top-level `client` field. The TUI reports `codex-tui`, and the app server reports the `clientInfo.name` value from `initialize`.

## Hooks

Codex also supports command hooks for selected lifecycle events through the `[hooks]` section in `config.toml`.

Current events:

- `approval_requested`: runs when Codex asks the user to approve an action
- `session_start`: runs when a session is created
- `user_prompt_submit`: runs when a user submits a turn prompt
- `pre_tool_use`: runs before a tool call executes
- `tool_use_failure`: runs after a tool call fails
- `subagent_start`: runs when Codex spawns a subagent
- `subagent_stop`: runs when a spawned subagent reaches a final status
- `compact_start`: runs before Codex starts a compaction task
- `agent_turn_complete`: runs after a turn finishes successfully
- `tool_use_complete`: runs after a tool call finishes

Example:

```toml
[[hooks.session_start]]
command = ["./scripts/session-start.sh"]

[[hooks.approval_requested]]
command = ["./scripts/check-approval.sh"]

[[hooks.user_prompt_submit]]
command = ["./scripts/check-prompt.sh"]

[[hooks.pre_tool_use]]
command = ["./scripts/check-tool.sh"]

[[hooks.tool_use_failure]]
command = ["./scripts/check-failed-tool.sh"]

[[hooks.subagent_start]]
command = ["./scripts/check-subagent-start.sh"]

[[hooks.subagent_stop]]
command = ["./scripts/check-subagent-stop.sh"]

[[hooks.compact_start]]
command = ["./scripts/check-compact.sh"]

[[hooks.agent_turn_complete]]
command = ["notify-send", "Codex turn complete"]

[[hooks.tool_use_complete]]
name = "tool-audit"
command = ["./scripts/audit-tool.sh"]
timeout_ms = 5000
on_failure = "abort"
```

Hook commands receive the hook payload JSON on `stdin`. The legacy `notify = [...]` setting is still supported and behaves as a compatibility wrapper for turn-complete notifications.

## JSON Schema

The generated JSON Schema for `config.toml` lives at `codex-rs/core/config.schema.json`.

## SQLite State DB

Codex stores the SQLite-backed state DB under `sqlite_home` (config key) or the
`CODEX_SQLITE_HOME` environment variable. When unset, WorkspaceWrite sandbox
sessions default to a temp directory; other modes default to `CODEX_HOME`.

## Custom CA Certificates

Codex can trust a custom root CA bundle for outbound HTTPS and secure websocket
connections when enterprise proxies or gateways intercept TLS. This applies to
login flows and to Codex's other external connections, including Codex
components that build reqwest clients or secure websocket clients through the
shared `codex-client` CA-loading path and remote MCP connections that use it.

Set `CODEX_CA_CERTIFICATE` to the path of a PEM file containing one or more
certificate blocks to use a Codex-specific CA bundle. If
`CODEX_CA_CERTIFICATE` is unset, Codex falls back to `SSL_CERT_FILE`. If
neither variable is set, Codex uses the system root certificates.

`CODEX_CA_CERTIFICATE` takes precedence over `SSL_CERT_FILE`. Empty values are
treated as unset.

The PEM file may contain multiple certificates. Codex also tolerates OpenSSL
`TRUSTED CERTIFICATE` labels and ignores well-formed `X509 CRL` sections in the
same bundle. If the file is empty, unreadable, or malformed, the affected Codex
HTTP or secure websocket connection reports a user-facing error that points
back to these environment variables.

## Notices

Codex stores "do not show again" flags for some UI prompts under the `[notice]` table.

## Plan mode defaults

`plan_mode_reasoning_effort` lets you set a Plan-mode-specific default reasoning
effort override. When unset, Plan mode uses the built-in Plan preset default
(currently `medium`). When explicitly set (including `none`), it overrides the
Plan preset. The string value `none` means "no reasoning" (an explicit Plan
override), not "inherit the global default". There is currently no separate
config value for "follow the global default in Plan mode".

## Realtime start instructions

`experimental_realtime_start_instructions` lets you replace the built-in
developer message Codex inserts when realtime becomes active. It only affects
the realtime start message in prompt history and does not change websocket
backend prompt settings or the realtime end/inactive message.

Ctrl+C/Ctrl+D quitting uses a ~1 second double-press hint (`ctrl + c again to quit`).
