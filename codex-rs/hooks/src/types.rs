use std::path::PathBuf;
use std::sync::Arc;

use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use codex_protocol::ThreadId;
use codex_protocol::models::SandboxPermissions;
use codex_protocol::user_input::UserInput;
use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde::Serializer;

pub type HookFn = Arc<dyn for<'a> Fn(&'a HookPayload) -> BoxFuture<'a, HookResult> + Send + Sync>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HookCommandFailureMode {
    #[default]
    Continue,
    Abort,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct HookCommandConfig {
    /// Optional name used in logs and surfaced hook outcomes. Defaults to the
    /// executable path when omitted.
    pub name: Option<String>,

    /// Full argv vector for the hook command.
    #[serde(default)]
    pub command: Vec<String>,

    /// Optional timeout. When omitted, the hook may run until completion.
    pub timeout_ms: Option<u64>,

    /// Whether failures should abort the current operation or merely be
    /// reported and allow execution to continue.
    #[serde(default)]
    pub on_failure: HookCommandFailureMode,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct HooksToml {
    /// Hooks that run when a session is created.
    #[serde(default)]
    pub session_start: Vec<HookCommandConfig>,

    /// Hooks that run when a user submits a prompt for a turn.
    #[serde(default)]
    pub user_prompt_submit: Vec<HookCommandConfig>,

    /// Hooks that run after a tool call fails.
    #[serde(default)]
    pub tool_use_failure: Vec<HookCommandConfig>,

    /// Hooks that run before a tool call executes.
    #[serde(default)]
    pub pre_tool_use: Vec<HookCommandConfig>,

    /// Hooks that run after a turn completes successfully.
    #[serde(default)]
    pub agent_turn_complete: Vec<HookCommandConfig>,

    /// Hooks that run after a tool call finishes.
    #[serde(default)]
    pub tool_use_complete: Vec<HookCommandConfig>,
}

#[derive(Debug)]
pub enum HookResult {
    /// Success: hook completed successfully.
    Success,
    /// FailedContinue: hook failed, but other subsequent hooks should still execute and the
    /// operation should continue.
    FailedContinue(Box<dyn std::error::Error + Send + Sync + 'static>),
    /// FailedAbort: hook failed, other subsequent hooks should not execute, and the operation
    /// should be aborted.
    FailedAbort(Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl HookResult {
    pub fn should_abort_operation(&self) -> bool {
        matches!(self, Self::FailedAbort(_))
    }
}

#[derive(Debug)]
pub struct HookResponse {
    pub hook_name: String,
    pub result: HookResult,
}

#[derive(Clone)]
pub struct Hook {
    pub name: String,
    pub func: HookFn,
}

impl Default for Hook {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            func: Arc::new(|_| Box::pin(async { HookResult::Success })),
        }
    }
}

impl Hook {
    pub async fn execute(&self, payload: &HookPayload) -> HookResponse {
        HookResponse {
            hook_name: self.name.clone(),
            result: (self.func)(payload).await,
        }
    }
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "snake_case")]
pub struct HookPayload {
    pub session_id: ThreadId,
    pub cwd: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    #[serde(serialize_with = "serialize_triggered_at")]
    pub triggered_at: DateTime<Utc>,
    pub hook_event: HookEvent,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct HookEventSessionStart {
    pub thread_id: ThreadId,
    pub session_source: String,
    pub model: String,
    pub model_provider_id: String,
    pub approval_policy: String,
    pub sandbox_policy: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct HookEventAfterAgent {
    pub thread_id: ThreadId,
    pub turn_id: String,
    pub input_messages: Vec<String>,
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookToolKind {
    Function,
    Custom,
    LocalShell,
    Mcp,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct HookToolInputLocalShell {
    pub command: Vec<String>,
    pub workdir: Option<String>,
    pub timeout_ms: Option<u64>,
    pub sandbox_permissions: Option<SandboxPermissions>,
    pub prefix_rule: Option<Vec<String>>,
    pub justification: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "input_type", rename_all = "snake_case")]
pub enum HookToolInput {
    Function {
        arguments: String,
    },
    Custom {
        input: String,
    },
    LocalShell {
        params: HookToolInputLocalShell,
    },
    Mcp {
        server: String,
        tool: String,
        arguments: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct HookEventUserPromptSubmit {
    pub turn_id: String,
    pub items: Vec<UserInput>,
    pub model: String,
    pub approval_policy: String,
    pub sandbox_policy: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct HookEventBeforeToolUse {
    pub turn_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub tool_kind: HookToolKind,
    pub tool_input: HookToolInput,
    pub mutating: bool,
    pub sandbox: String,
    pub sandbox_policy: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct HookEventAfterToolUse {
    pub turn_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub tool_kind: HookToolKind,
    pub tool_input: HookToolInput,
    pub executed: bool,
    pub success: bool,
    pub duration_ms: u64,
    pub mutating: bool,
    pub sandbox: String,
    pub sandbox_policy: String,
    pub output_preview: String,
}

fn serialize_triggered_at<S>(value: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&value.to_rfc3339_opts(SecondsFormat::Secs, true))
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum HookEvent {
    SessionStart {
        #[serde(flatten)]
        event: HookEventSessionStart,
    },
    UserPromptSubmit {
        #[serde(flatten)]
        event: HookEventUserPromptSubmit,
    },
    BeforeToolUse {
        #[serde(flatten)]
        event: HookEventBeforeToolUse,
    },
    ToolUseFailure {
        #[serde(flatten)]
        event: HookEventAfterToolUse,
    },
    AfterAgent {
        #[serde(flatten)]
        event: HookEventAfterAgent,
    },
    AfterToolUse {
        #[serde(flatten)]
        event: HookEventAfterToolUse,
    },
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::TimeZone;
    use chrono::Utc;
    use codex_protocol::ThreadId;
    use codex_protocol::models::SandboxPermissions;
    use codex_protocol::user_input::UserInput;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::HookEvent;
    use super::HookEventAfterAgent;
    use super::HookEventAfterToolUse;
    use super::HookEventBeforeToolUse;
    use super::HookEventSessionStart;
    use super::HookEventUserPromptSubmit;
    use super::HookPayload;
    use super::HookToolInput;
    use super::HookToolInputLocalShell;
    use super::HookToolKind;

    #[test]
    fn hook_payload_serializes_stable_wire_shape() {
        let session_id = ThreadId::new();
        let thread_id = ThreadId::new();
        let payload = HookPayload {
            session_id,
            cwd: PathBuf::from("tmp"),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::AfterAgent {
                event: HookEventAfterAgent {
                    thread_id,
                    turn_id: "turn-1".to_string(),
                    input_messages: vec!["hello".to_string()],
                    last_assistant_message: Some("hi".to_string()),
                },
            },
        };

        let actual = serde_json::to_value(payload).expect("serialize hook payload");
        let expected = json!({
            "session_id": session_id.to_string(),
            "cwd": "tmp",
            "triggered_at": "2025-01-01T00:00:00Z",
            "hook_event": {
                "event_type": "after_agent",
                "thread_id": thread_id.to_string(),
                "turn_id": "turn-1",
                "input_messages": ["hello"],
                "last_assistant_message": "hi",
            },
        });

        assert_eq!(actual, expected);
    }

    #[test]
    fn session_start_payload_serializes_stable_wire_shape() {
        let session_id = ThreadId::new();
        let payload = HookPayload {
            session_id,
            cwd: PathBuf::from("tmp"),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::SessionStart {
                event: HookEventSessionStart {
                    thread_id: session_id,
                    session_source: "cli".to_string(),
                    model: "gpt-5-codex".to_string(),
                    model_provider_id: "openai".to_string(),
                    approval_policy: "on-request".to_string(),
                    sandbox_policy: "workspace-write".to_string(),
                },
            },
        };

        let actual = serde_json::to_value(payload).expect("serialize hook payload");
        let expected = json!({
            "session_id": session_id.to_string(),
            "cwd": "tmp",
            "triggered_at": "2025-01-01T00:00:00Z",
            "hook_event": {
                "event_type": "session_start",
                "thread_id": session_id.to_string(),
                "session_source": "cli",
                "model": "gpt-5-codex",
                "model_provider_id": "openai",
                "approval_policy": "on-request",
                "sandbox_policy": "workspace-write",
            },
        });

        assert_eq!(actual, expected);
    }

    #[test]
    fn after_tool_use_payload_serializes_stable_wire_shape() {
        let session_id = ThreadId::new();
        let payload = HookPayload {
            session_id,
            cwd: PathBuf::from("tmp"),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::AfterToolUse {
                event: HookEventAfterToolUse {
                    turn_id: "turn-2".to_string(),
                    call_id: "call-1".to_string(),
                    tool_name: "local_shell".to_string(),
                    tool_kind: HookToolKind::LocalShell,
                    tool_input: HookToolInput::LocalShell {
                        params: HookToolInputLocalShell {
                            command: vec!["cargo".to_string(), "fmt".to_string()],
                            workdir: Some("codex-rs".to_string()),
                            timeout_ms: Some(60_000),
                            sandbox_permissions: Some(SandboxPermissions::UseDefault),
                            justification: None,
                            prefix_rule: None,
                        },
                    },
                    executed: true,
                    success: true,
                    duration_ms: 42,
                    mutating: true,
                    sandbox: "none".to_string(),
                    sandbox_policy: "danger-full-access".to_string(),
                    output_preview: "ok".to_string(),
                },
            },
        };

        let actual = serde_json::to_value(payload).expect("serialize hook payload");
        let expected = json!({
            "session_id": session_id.to_string(),
            "cwd": "tmp",
            "triggered_at": "2025-01-01T00:00:00Z",
            "hook_event": {
                "event_type": "after_tool_use",
                "turn_id": "turn-2",
                "call_id": "call-1",
                "tool_name": "local_shell",
                "tool_kind": "local_shell",
                "tool_input": {
                    "input_type": "local_shell",
                    "params": {
                        "command": ["cargo", "fmt"],
                        "workdir": "codex-rs",
                        "timeout_ms": 60000,
                        "sandbox_permissions": "use_default",
                        "justification": null,
                        "prefix_rule": null,
                    },
                },
                "executed": true,
                "success": true,
                "duration_ms": 42,
                "mutating": true,
                "sandbox": "none",
                "sandbox_policy": "danger-full-access",
                "output_preview": "ok",
            },
        });

        assert_eq!(actual, expected);
    }

    #[test]
    fn tool_use_failure_payload_serializes_stable_wire_shape() {
        let session_id = ThreadId::new();
        let payload = HookPayload {
            session_id,
            cwd: PathBuf::from("tmp"),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::ToolUseFailure {
                event: HookEventAfterToolUse {
                    turn_id: "turn-fail".to_string(),
                    call_id: "call-fail".to_string(),
                    tool_name: "local_shell".to_string(),
                    tool_kind: HookToolKind::LocalShell,
                    tool_input: HookToolInput::LocalShell {
                        params: HookToolInputLocalShell {
                            command: vec!["cargo".to_string(), "fmt".to_string()],
                            workdir: Some("codex-rs".to_string()),
                            timeout_ms: Some(60_000),
                            sandbox_permissions: Some(SandboxPermissions::UseDefault),
                            justification: None,
                            prefix_rule: None,
                        },
                    },
                    executed: true,
                    success: false,
                    duration_ms: 42,
                    mutating: true,
                    sandbox: "none".to_string(),
                    sandbox_policy: "danger-full-access".to_string(),
                    output_preview: "failed".to_string(),
                },
            },
        };

        let actual = serde_json::to_value(payload).expect("serialize hook payload");
        let expected = json!({
            "session_id": session_id.to_string(),
            "cwd": "tmp",
            "triggered_at": "2025-01-01T00:00:00Z",
            "hook_event": {
                "event_type": "tool_use_failure",
                "turn_id": "turn-fail",
                "call_id": "call-fail",
                "tool_name": "local_shell",
                "tool_kind": "local_shell",
                "tool_input": {
                    "input_type": "local_shell",
                    "params": {
                        "command": ["cargo", "fmt"],
                        "workdir": "codex-rs",
                        "timeout_ms": 60000,
                        "sandbox_permissions": "use_default",
                        "justification": null,
                        "prefix_rule": null,
                    },
                },
                "executed": true,
                "success": false,
                "duration_ms": 42,
                "mutating": true,
                "sandbox": "none",
                "sandbox_policy": "danger-full-access",
                "output_preview": "failed",
            },
        });

        assert_eq!(actual, expected);
    }

    #[test]
    fn before_tool_use_payload_serializes_stable_wire_shape() {
        let session_id = ThreadId::new();
        let payload = HookPayload {
            session_id,
            cwd: PathBuf::from("tmp"),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::BeforeToolUse {
                event: HookEventBeforeToolUse {
                    turn_id: "turn-0".to_string(),
                    call_id: "call-0".to_string(),
                    tool_name: "local_shell".to_string(),
                    tool_kind: HookToolKind::LocalShell,
                    tool_input: HookToolInput::LocalShell {
                        params: HookToolInputLocalShell {
                            command: vec!["cargo".to_string(), "fmt".to_string()],
                            workdir: Some("codex-rs".to_string()),
                            timeout_ms: Some(60_000),
                            sandbox_permissions: Some(SandboxPermissions::UseDefault),
                            justification: None,
                            prefix_rule: None,
                        },
                    },
                    mutating: true,
                    sandbox: "none".to_string(),
                    sandbox_policy: "danger-full-access".to_string(),
                },
            },
        };

        let actual = serde_json::to_value(payload).expect("serialize hook payload");
        let expected = json!({
            "session_id": session_id.to_string(),
            "cwd": "tmp",
            "triggered_at": "2025-01-01T00:00:00Z",
            "hook_event": {
                "event_type": "before_tool_use",
                "turn_id": "turn-0",
                "call_id": "call-0",
                "tool_name": "local_shell",
                "tool_kind": "local_shell",
                "tool_input": {
                    "input_type": "local_shell",
                    "params": {
                        "command": ["cargo", "fmt"],
                        "workdir": "codex-rs",
                        "timeout_ms": 60000,
                        "sandbox_permissions": "use_default",
                        "justification": null,
                        "prefix_rule": null,
                    },
                },
                "mutating": true,
                "sandbox": "none",
                "sandbox_policy": "danger-full-access",
            },
        });

        assert_eq!(actual, expected);
    }

    #[test]
    fn user_prompt_submit_payload_serializes_stable_wire_shape() {
        let session_id = ThreadId::new();
        let payload = HookPayload {
            session_id,
            cwd: PathBuf::from("tmp"),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::UserPromptSubmit {
                event: HookEventUserPromptSubmit {
                    turn_id: "turn-user".to_string(),
                    items: vec![UserInput::Text {
                        text: "hello".to_string(),
                        text_elements: Vec::new(),
                    }],
                    model: "gpt-5-codex".to_string(),
                    approval_policy: "on-request".to_string(),
                    sandbox_policy: "workspace-write".to_string(),
                },
            },
        };

        let actual = serde_json::to_value(payload).expect("serialize hook payload");
        let expected = json!({
            "session_id": session_id.to_string(),
            "cwd": "tmp",
            "triggered_at": "2025-01-01T00:00:00Z",
            "hook_event": {
                "event_type": "user_prompt_submit",
                "turn_id": "turn-user",
                "items": [{
                    "type": "text",
                    "text": "hello",
                    "text_elements": [],
                }],
                "model": "gpt-5-codex",
                "approval_policy": "on-request",
                "sandbox_policy": "workspace-write",
            },
        });

        assert_eq!(actual, expected);
    }
}
