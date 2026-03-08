use codex_config::ConfigLayerStack;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::engine::ClaudeHooksEngine;
use crate::engine::CommandShell;
use crate::events::session_start::SessionStartOutcome;
use crate::events::session_start::SessionStartRequest;
use crate::events::stop::StopOutcome;
use crate::events::stop::StopRequest;
use crate::types::Hook;
use crate::types::HookCommandConfig;
use crate::types::HookCommandFailureMode;
use crate::types::HookEvent;
use crate::types::HookPayload;
use crate::types::HookResponse;
use crate::types::HookResult;

#[derive(Default, Clone)]
pub struct HooksConfig {
    pub legacy_notify_argv: Option<Vec<String>>,
    pub feature_enabled: bool,
    pub config_layer_stack: Option<ConfigLayerStack>,
    pub shell_program: Option<String>,
    pub shell_args: Vec<String>,
    pub session_start: Vec<HookCommandConfig>,
    pub approval_requested: Vec<HookCommandConfig>,
    pub user_prompt_submit: Vec<HookCommandConfig>,
    pub tool_use_failure: Vec<HookCommandConfig>,
    pub pre_tool_use: Vec<HookCommandConfig>,
    pub agent_turn_complete: Vec<HookCommandConfig>,
    pub subagent_start: Vec<HookCommandConfig>,
    pub subagent_stop: Vec<HookCommandConfig>,
    pub tool_use_complete: Vec<HookCommandConfig>,
}

#[derive(Clone)]
pub struct Hooks {
    engine: ClaudeHooksEngine,
    session_start: Vec<Hook>,
    approval_requested: Vec<Hook>,
    user_prompt_submit: Vec<Hook>,
    tool_use_failure: Vec<Hook>,
    pre_tool_use: Vec<Hook>,
    agent_turn_complete: Vec<Hook>,
    subagent_start: Vec<Hook>,
    subagent_stop: Vec<Hook>,
    tool_use_complete: Vec<Hook>,
}

impl Default for Hooks {
    fn default() -> Self {
        Self::new(HooksConfig::default())
    }
}

impl Hooks {
    pub fn new(config: HooksConfig) -> Self {
        let session_start = config
            .session_start
            .into_iter()
            .filter_map(command_hook)
            .collect();
        let approval_requested = config
            .approval_requested
            .into_iter()
            .filter_map(command_hook)
            .collect();
        let user_prompt_submit = config
            .user_prompt_submit
            .into_iter()
            .filter_map(command_hook)
            .collect();
        let tool_use_failure = config
            .tool_use_failure
            .into_iter()
            .filter_map(command_hook)
            .collect();
        let subagent_start = config
            .subagent_start
            .into_iter()
            .filter_map(command_hook)
            .collect();
        let subagent_stop = config
            .subagent_stop
            .into_iter()
            .filter_map(command_hook)
            .collect();
        let pre_tool_use = config
            .pre_tool_use
            .into_iter()
            .filter_map(command_hook)
            .collect();
        let mut agent_turn_complete: Vec<Hook> = config
            .agent_turn_complete
            .into_iter()
            .filter_map(command_hook)
            .collect();
        let engine = ClaudeHooksEngine::new(
            config.feature_enabled,
            config.config_layer_stack.as_ref(),
            CommandShell {
                program: config.shell_program.unwrap_or_default(),
                args: config.shell_args,
            },
        );
        agent_turn_complete.extend(
            config
                .legacy_notify_argv
                .filter(|argv| !argv.is_empty() && !argv[0].is_empty())
                .map(crate::notify_hook),
        );
        let tool_use_complete = config
            .tool_use_complete
            .into_iter()
            .filter_map(command_hook)
            .collect();

        Self {
            engine,
            session_start,
            approval_requested,
            user_prompt_submit,
            tool_use_failure,
            pre_tool_use,
            agent_turn_complete,
            subagent_start,
            subagent_stop,
            tool_use_complete,
        }
    }

    pub fn startup_warnings(&self) -> &[String] {
        self.engine.warnings()
    }

    fn hooks_for_event(&self, hook_event: &HookEvent) -> &[Hook] {
        match hook_event {
            HookEvent::SessionStart { .. } => &self.session_start,
            HookEvent::ApprovalRequested { .. } => &self.approval_requested,
            HookEvent::UserPromptSubmit { .. } => &self.user_prompt_submit,
            HookEvent::ToolUseFailure { .. } => &self.tool_use_failure,
            HookEvent::BeforeToolUse { .. } => &self.pre_tool_use,
            HookEvent::AfterAgent { .. } => &self.agent_turn_complete,
            HookEvent::SubagentStart { .. } => &self.subagent_start,
            HookEvent::SubagentStop { .. } => &self.subagent_stop,
            HookEvent::AfterToolUse { .. } => &self.tool_use_complete,
        }
    }

    pub async fn dispatch(&self, hook_payload: HookPayload) -> Vec<HookResponse> {
        let hooks = self.hooks_for_event(&hook_payload.hook_event);
        let mut outcomes = Vec::with_capacity(hooks.len());
        for hook in hooks {
            let outcome = hook.execute(&hook_payload).await;
            let should_abort_operation = outcome.result.should_abort_operation();
            outcomes.push(outcome);
            if should_abort_operation {
                break;
            }
        }

        outcomes
    }

    pub fn preview_session_start(
        &self,
        request: &SessionStartRequest,
    ) -> Vec<codex_protocol::protocol::HookRunSummary> {
        self.engine.preview_session_start(request)
    }

    pub async fn run_session_start(
        &self,
        request: SessionStartRequest,
        turn_id: Option<String>,
    ) -> SessionStartOutcome {
        self.engine.run_session_start(request, turn_id).await
    }

    pub fn preview_stop(
        &self,
        request: &StopRequest,
    ) -> Vec<codex_protocol::protocol::HookRunSummary> {
        self.engine.preview_stop(request)
    }

    pub async fn run_stop(&self, request: StopRequest) -> StopOutcome {
        self.engine.run_stop(request).await
    }
}

fn command_hook(config: HookCommandConfig) -> Option<Hook> {
    let program = config.command.first()?;
    if program.is_empty() {
        return None;
    }
    let hook_name = config.name.clone().or_else(|| Some(program.clone()))?;
    let config = std::sync::Arc::new(config);
    Some(Hook {
        name: hook_name,
        func: std::sync::Arc::new(move |payload: &HookPayload| {
            let config = std::sync::Arc::clone(&config);
            Box::pin(async move {
                let Some(mut command) = command_from_argv(&config.command) else {
                    return HookResult::Success;
                };
                command
                    .current_dir(&payload.cwd)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());

                let json = match serde_json::to_vec(payload) {
                    Ok(json) => json,
                    Err(error) => return failure_result(config.on_failure, error.into()),
                };

                let run_hook = async {
                    let mut child = command.spawn()?;
                    if let Some(mut stdin) = child.stdin.take() {
                        stdin.write_all(&json).await?;
                    }
                    let output = child.wait_with_output().await?;
                    if output.status.success() {
                        Ok(())
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                        let detail = if stderr.is_empty() {
                            format!("hook exited with status {}", output.status)
                        } else {
                            format!("hook exited with status {}: {stderr}", output.status)
                        };
                        Err(std::io::Error::other(detail))
                    }
                };

                let result = if let Some(timeout_ms) = config.timeout_ms {
                    match timeout(Duration::from_millis(timeout_ms), run_hook).await {
                        Ok(result) => result,
                        Err(_) => Err(std::io::Error::other(format!(
                            "hook timed out after {timeout_ms}ms"
                        ))),
                    }
                } else {
                    run_hook.await
                };

                match result {
                    Ok(()) => HookResult::Success,
                    Err(error) => failure_result(config.on_failure, error.into()),
                }
            })
        }),
    })
}

fn failure_result(
    on_failure: HookCommandFailureMode,
    error: Box<dyn std::error::Error + Send + Sync + 'static>,
) -> HookResult {
    match on_failure {
        HookCommandFailureMode::Continue => HookResult::FailedContinue(error),
        HookCommandFailureMode::Abort => HookResult::FailedAbort(error),
    }
}

pub fn command_from_argv(argv: &[String]) -> Option<Command> {
    let (program, args) = argv.split_first()?;
    if program.is_empty() {
        return None;
    }
    let mut command = Command::new(program);
    command.args(args);
    Some(command)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use anyhow::Result;
    use chrono::TimeZone;
    use chrono::Utc;
    use codex_protocol::ThreadId;
    use pretty_assertions::assert_eq;
    use serde_json::to_string;
    use tempfile::tempdir;
    use tokio::time::timeout;

    use super::*;
    use crate::types::HookApprovalKind;
    use crate::types::HookEventAfterAgent;
    use crate::types::HookEventAfterToolUse;
    use crate::types::HookEventApprovalRequested;
    use crate::types::HookEventBeforeToolUse;
    use crate::types::HookEventSessionStart;
    use crate::types::HookEventSubagentStart;
    use crate::types::HookEventSubagentStop;
    use crate::types::HookEventUserPromptSubmit;
    use crate::types::HookResult;
    use crate::types::HookToolInput;
    use crate::types::HookToolKind;

    const CWD: &str = "/tmp";
    const INPUT_MESSAGE: &str = "hello";

    fn hook_payload(label: &str) -> HookPayload {
        HookPayload {
            session_id: ThreadId::new(),
            cwd: PathBuf::from(CWD),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::AfterAgent {
                event: HookEventAfterAgent {
                    thread_id: ThreadId::new(),
                    turn_id: format!("turn-{label}"),
                    input_messages: vec![INPUT_MESSAGE.to_string()],
                    last_assistant_message: Some("hi".to_string()),
                },
            },
        }
    }

    fn session_start_payload(label: &str) -> HookPayload {
        let thread_id = ThreadId::new();
        HookPayload {
            session_id: thread_id,
            cwd: PathBuf::from(CWD),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::SessionStart {
                event: HookEventSessionStart {
                    thread_id,
                    session_source: format!("cli-{label}"),
                    model: "gpt-5-codex".to_string(),
                    model_provider_id: "openai".to_string(),
                    approval_policy: "on-request".to_string(),
                    sandbox_policy: "workspace-write".to_string(),
                },
            },
        }
    }

    fn approval_requested_payload(label: &str) -> HookPayload {
        HookPayload {
            session_id: ThreadId::new(),
            cwd: PathBuf::from(CWD),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::ApprovalRequested {
                event: HookEventApprovalRequested {
                    turn_id: format!("turn-{label}"),
                    approval_id: format!("approval-{label}"),
                    kind: HookApprovalKind::ExecCommand,
                    call_id: Some(format!("call-{label}")),
                    reason: Some("need approval".to_string()),
                    command: Some(vec!["git".to_string(), "commit".to_string()]),
                    cwd: Some(PathBuf::from("repo")),
                    changed_paths: None,
                    server_name: None,
                    request_id: None,
                },
            },
        }
    }

    fn subagent_start_payload(label: &str) -> HookPayload {
        HookPayload {
            session_id: ThreadId::new(),
            cwd: PathBuf::from(CWD),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::SubagentStart {
                event: HookEventSubagentStart {
                    parent_thread_id: ThreadId::new(),
                    child_thread_id: ThreadId::new(),
                    agent_nickname: Some("Scout".to_string()),
                    agent_role: Some("explorer".to_string()),
                    prompt: format!("prompt-{label}"),
                },
            },
        }
    }

    fn subagent_stop_payload(_label: &str) -> HookPayload {
        HookPayload {
            session_id: ThreadId::new(),
            cwd: PathBuf::from(CWD),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::SubagentStop {
                event: HookEventSubagentStop {
                    parent_thread_id: ThreadId::new(),
                    child_thread_id: ThreadId::new(),
                    agent_nickname: Some("Scout".to_string()),
                    agent_role: Some("explorer".to_string()),
                    status: "completed".to_string(),
                },
            },
        }
    }

    fn counting_success_hook(calls: &Arc<AtomicUsize>, name: &str) -> Hook {
        let hook_name = name.to_string();
        let calls = Arc::clone(calls);
        Hook {
            name: hook_name,
            func: Arc::new(move |_| {
                let calls = Arc::clone(&calls);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    HookResult::Success
                })
            }),
        }
    }

    fn failing_continue_hook(calls: &Arc<AtomicUsize>, name: &str, message: &str) -> Hook {
        let hook_name = name.to_string();
        let message = message.to_string();
        let calls = Arc::clone(calls);
        Hook {
            name: hook_name,
            func: Arc::new(move |_| {
                let calls = Arc::clone(&calls);
                let message = message.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    HookResult::FailedContinue(std::io::Error::other(message).into())
                })
            }),
        }
    }

    fn failing_abort_hook(calls: &Arc<AtomicUsize>, name: &str, message: &str) -> Hook {
        let hook_name = name.to_string();
        let message = message.to_string();
        let calls = Arc::clone(calls);
        Hook {
            name: hook_name,
            func: Arc::new(move |_| {
                let calls = Arc::clone(&calls);
                let message = message.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    HookResult::FailedAbort(std::io::Error::other(message).into())
                })
            }),
        }
    }

    fn after_tool_use_payload(label: &str) -> HookPayload {
        HookPayload {
            session_id: ThreadId::new(),
            cwd: PathBuf::from(CWD),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::AfterToolUse {
                event: HookEventAfterToolUse {
                    turn_id: format!("turn-{label}"),
                    call_id: format!("call-{label}"),
                    tool_name: "apply_patch".to_string(),
                    tool_kind: HookToolKind::Custom,
                    tool_input: HookToolInput::Custom {
                        input: "*** Begin Patch".to_string(),
                    },
                    executed: true,
                    success: true,
                    duration_ms: 1,
                    mutating: true,
                    sandbox: "none".to_string(),
                    sandbox_policy: "danger-full-access".to_string(),
                    output_preview: "ok".to_string(),
                },
            },
        }
    }

    fn tool_use_failure_payload(label: &str) -> HookPayload {
        HookPayload {
            session_id: ThreadId::new(),
            cwd: PathBuf::from(CWD),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::ToolUseFailure {
                event: HookEventAfterToolUse {
                    turn_id: format!("turn-{label}"),
                    call_id: format!("call-{label}"),
                    tool_name: "apply_patch".to_string(),
                    tool_kind: HookToolKind::Custom,
                    tool_input: HookToolInput::Custom {
                        input: "*** Begin Patch".to_string(),
                    },
                    executed: true,
                    success: false,
                    duration_ms: 1,
                    mutating: true,
                    sandbox: "none".to_string(),
                    sandbox_policy: "danger-full-access".to_string(),
                    output_preview: "failed".to_string(),
                },
            },
        }
    }

    fn before_tool_use_payload(label: &str) -> HookPayload {
        HookPayload {
            session_id: ThreadId::new(),
            cwd: PathBuf::from(CWD),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::BeforeToolUse {
                event: HookEventBeforeToolUse {
                    turn_id: format!("turn-{label}"),
                    call_id: format!("call-{label}"),
                    tool_name: "apply_patch".to_string(),
                    tool_kind: HookToolKind::Custom,
                    tool_input: HookToolInput::Custom {
                        input: "*** Begin Patch".to_string(),
                    },
                    mutating: true,
                    sandbox: "none".to_string(),
                    sandbox_policy: "danger-full-access".to_string(),
                },
            },
        }
    }

    fn user_prompt_submit_payload(label: &str) -> HookPayload {
        HookPayload {
            session_id: ThreadId::new(),
            cwd: PathBuf::from(CWD),
            client: None,
            triggered_at: Utc
                .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
                .single()
                .expect("valid timestamp"),
            hook_event: HookEvent::UserPromptSubmit {
                event: HookEventUserPromptSubmit {
                    turn_id: format!("turn-{label}"),
                    items: vec![codex_protocol::user_input::UserInput::Text {
                        text: INPUT_MESSAGE.to_string(),
                        text_elements: Vec::new(),
                    }],
                    model: "gpt-5-codex".to_string(),
                    approval_policy: "on-request".to_string(),
                    sandbox_policy: "workspace-write".to_string(),
                },
            },
        }
    }

    #[test]
    fn command_from_argv_returns_none_for_empty_args() {
        assert!(command_from_argv(&[]).is_none());
        assert!(command_from_argv(&["".to_string()]).is_none());
    }

    #[tokio::test]
    async fn command_from_argv_builds_command() -> Result<()> {
        let argv = if cfg!(windows) {
            vec![
                "cmd".to_string(),
                "/C".to_string(),
                "echo hello world".to_string(),
            ]
        } else {
            vec!["echo".to_string(), "hello".to_string(), "world".to_string()]
        };
        let mut command = command_from_argv(&argv).ok_or_else(|| anyhow::anyhow!("command"))?;
        let output = command.stdout(Stdio::piped()).output().await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim_end_matches(['\r', '\n']);
        assert_eq!(trimmed, "hello world");
        Ok(())
    }

    #[test]
    fn hooks_new_requires_program_name() {
        assert!(
            Hooks::new(HooksConfig::default())
                .agent_turn_complete
                .is_empty()
        );
        assert!(
            Hooks::new(HooksConfig {
                legacy_notify_argv: Some(vec![]),
                ..HooksConfig::default()
            })
            .agent_turn_complete
            .is_empty()
        );
        assert!(
            Hooks::new(HooksConfig {
                legacy_notify_argv: Some(vec!["".to_string()]),
                ..HooksConfig::default()
            })
            .agent_turn_complete
            .is_empty()
        );
        assert_eq!(
            Hooks::new(HooksConfig {
                legacy_notify_argv: Some(vec!["notify-send".to_string()]),
                ..HooksConfig::default()
            })
            .agent_turn_complete
            .len(),
            1
        );
    }

    #[tokio::test]
    async fn dispatch_executes_hook() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            agent_turn_complete: vec![counting_success_hook(&calls, "counting")],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(hook_payload("1")).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].hook_name, "counting");
        assert!(matches!(outcomes[0].result, HookResult::Success));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_executes_session_start_hooks() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            session_start: vec![counting_success_hook(&calls, "counting")],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(session_start_payload("session")).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].hook_name, "counting");
        assert!(matches!(outcomes[0].result, HookResult::Success));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_executes_subagent_start_hooks() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            subagent_start: vec![counting_success_hook(&calls, "counting")],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(subagent_start_payload("start")).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].hook_name, "counting");
        assert!(matches!(outcomes[0].result, HookResult::Success));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_executes_subagent_stop_hooks() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            subagent_stop: vec![counting_success_hook(&calls, "counting")],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(subagent_stop_payload("stop")).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].hook_name, "counting");
        assert!(matches!(outcomes[0].result, HookResult::Success));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_executes_approval_requested_hooks() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            approval_requested: vec![counting_success_hook(&calls, "counting")],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(approval_requested_payload("approval")).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].hook_name, "counting");
        assert!(matches!(outcomes[0].result, HookResult::Success));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_executes_user_prompt_submit_hooks() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            user_prompt_submit: vec![counting_success_hook(&calls, "counting")],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(user_prompt_submit_payload("user")).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].hook_name, "counting");
        assert!(matches!(outcomes[0].result, HookResult::Success));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn default_hook_is_noop_and_continues() {
        let payload = hook_payload("d");
        let outcome = Hook::default().execute(&payload).await;
        assert_eq!(outcome.hook_name, "default");
        assert!(matches!(outcome.result, HookResult::Success));
    }

    #[tokio::test]
    async fn dispatch_executes_multiple_hooks_for_same_event() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            agent_turn_complete: vec![
                counting_success_hook(&calls, "counting-1"),
                counting_success_hook(&calls, "counting-2"),
            ],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(hook_payload("2")).await;
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].hook_name, "counting-1");
        assert_eq!(outcomes[1].hook_name, "counting-2");
        assert!(matches!(outcomes[0].result, HookResult::Success));
        assert!(matches!(outcomes[1].result, HookResult::Success));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn dispatch_stops_when_hook_requests_abort() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            agent_turn_complete: vec![
                failing_abort_hook(&calls, "abort", "hook failed"),
                counting_success_hook(&calls, "counting"),
            ],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(hook_payload("3")).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].hook_name, "abort");
        assert!(matches!(outcomes[0].result, HookResult::FailedAbort(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_executes_after_tool_use_hooks() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            tool_use_complete: vec![counting_success_hook(&calls, "counting")],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(after_tool_use_payload("p")).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].hook_name, "counting");
        assert!(matches!(outcomes[0].result, HookResult::Success));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_executes_tool_use_failure_hooks() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            tool_use_failure: vec![counting_success_hook(&calls, "counting")],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(tool_use_failure_payload("fail")).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].hook_name, "counting");
        assert!(matches!(outcomes[0].result, HookResult::Success));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_executes_pre_tool_use_hooks() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            pre_tool_use: vec![counting_success_hook(&calls, "counting")],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(before_tool_use_payload("pre")).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].hook_name, "counting");
        assert!(matches!(outcomes[0].result, HookResult::Success));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatch_continues_after_continueable_failure() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            agent_turn_complete: vec![
                failing_continue_hook(&calls, "failing", "hook failed"),
                counting_success_hook(&calls, "counting"),
            ],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(hook_payload("err")).await;
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].hook_name, "failing");
        assert!(matches!(outcomes[0].result, HookResult::FailedContinue(_)));
        assert_eq!(outcomes[1].hook_name, "counting");
        assert!(matches!(outcomes[1].result, HookResult::Success));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn dispatch_returns_after_tool_use_failure_outcome() {
        let calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            tool_use_complete: vec![failing_continue_hook(
                &calls,
                "failing",
                "after_tool_use hook failed",
            )],
            ..Hooks::default()
        };

        let outcomes = hooks.dispatch(after_tool_use_payload("err-tool")).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].hook_name, "failing");
        assert!(matches!(outcomes[0].result, HookResult::FailedContinue(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn command_hook_abort_mode_returns_failed_abort() {
        let hooks = Hooks::new(HooksConfig {
            agent_turn_complete: vec![HookCommandConfig {
                name: Some("missing".to_string()),
                command: vec!["definitely-missing-codex-hook-command".to_string()],
                timeout_ms: None,
                on_failure: HookCommandFailureMode::Abort,
            }],
            ..HooksConfig::default()
        });

        let outcomes = hooks.dispatch(hook_payload("abort-mode")).await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].hook_name, "missing");
        assert!(matches!(outcomes[0].result, HookResult::FailedAbort(_)));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn command_hook_reads_payload_from_stdin_unix() -> Result<()> {
        let temp_dir = tempdir()?;
        let payload_path = temp_dir.path().join("payload.json");
        let payload_path_arg = payload_path.to_string_lossy().into_owned();
        let payload = hook_payload("stdin-unix");
        let expected = to_string(&payload)?;
        let hooks = Hooks::new(HooksConfig {
            agent_turn_complete: vec![HookCommandConfig {
                name: Some("stdin-writer".to_string()),
                command: vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "cat > \"$1\"".to_string(),
                    "sh".to_string(),
                    payload_path_arg,
                ],
                timeout_ms: Some(2_000),
                on_failure: HookCommandFailureMode::Abort,
            }],
            ..HooksConfig::default()
        });

        let outcomes = hooks.dispatch(payload).await;
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0].result, HookResult::Success));
        assert_eq!(fs::read_to_string(payload_path)?, expected);
        Ok(())
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn command_hook_reads_payload_from_stdin_windows() -> Result<()> {
        let temp_dir = tempdir()?;
        let payload_path = temp_dir.path().join("payload.json");
        let payload_path_arg = payload_path.to_string_lossy().into_owned();
        let script_path = temp_dir.path().join("write_stdin.ps1");
        fs::write(
            &script_path,
            "$input | Set-Content -NoNewline -Encoding utf8 $args[0]",
        )?;
        let script_path_arg = script_path.to_string_lossy().into_owned();
        let payload = hook_payload("stdin-windows");
        let expected = to_string(&payload)?;
        let hooks = Hooks::new(HooksConfig {
            agent_turn_complete: vec![HookCommandConfig {
                name: Some("stdin-writer".to_string()),
                command: vec![
                    "powershell.exe".to_string(),
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-ExecutionPolicy".to_string(),
                    "Bypass".to_string(),
                    "-File".to_string(),
                    script_path_arg,
                    payload_path_arg,
                ],
                timeout_ms: Some(2_000),
                on_failure: HookCommandFailureMode::Abort,
            }],
            ..HooksConfig::default()
        });

        let outcomes = hooks.dispatch(payload).await;
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0].result, HookResult::Success));
        assert_eq!(fs::read_to_string(payload_path)?, expected);
        Ok(())
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn hook_executes_program_with_payload_argument_unix() -> Result<()> {
        let temp_dir = tempdir()?;
        let payload_path = temp_dir.path().join("payload.json");
        let payload_path_arg = payload_path.to_string_lossy().into_owned();
        let hook = Hook {
            name: "write_payload".to_string(),
            func: Arc::new(move |payload: &HookPayload| {
                let payload_path_arg = payload_path_arg.clone();
                Box::pin(async move {
                    let json = to_string(payload).expect("serialize hook payload");
                    let mut command = command_from_argv(&[
                        "/bin/sh".to_string(),
                        "-c".to_string(),
                        "printf '%s' \"$2\" > \"$1\"".to_string(),
                        "sh".to_string(),
                        payload_path_arg,
                        json,
                    ])
                    .expect("build command");
                    command.status().await.expect("run hook command");
                    HookResult::Success
                })
            }),
        };

        let payload = hook_payload("4");
        let expected = to_string(&payload)?;

        let hooks = Hooks {
            agent_turn_complete: vec![hook],
            ..Hooks::default()
        };
        let outcomes = hooks.dispatch(payload).await;
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0].result, HookResult::Success));

        let contents = timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(contents) = fs::read_to_string(&payload_path)
                    && !contents.is_empty()
                {
                    return contents;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;

        assert_eq!(contents, expected);
        Ok(())
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn hook_executes_program_with_payload_argument_windows() -> Result<()> {
        let temp_dir = tempdir()?;
        let payload_path = temp_dir.path().join("payload.json");
        let payload_path_arg = payload_path.to_string_lossy().into_owned();
        let script_path = temp_dir.path().join("write_payload.ps1");
        fs::write(&script_path, "[IO.File]::WriteAllText($args[0], $args[1])")?;
        let script_path_arg = script_path.to_string_lossy().into_owned();
        let hook = Hook {
            name: "write_payload".to_string(),
            func: Arc::new(move |payload: &HookPayload| {
                let payload_path_arg = payload_path_arg.clone();
                let script_path_arg = script_path_arg.clone();
                Box::pin(async move {
                    let json = to_string(payload).expect("serialize hook payload");
                    let mut command = command_from_argv(&[
                        "powershell.exe".to_string(),
                        "-NoLogo".to_string(),
                        "-NoProfile".to_string(),
                        "-ExecutionPolicy".to_string(),
                        "Bypass".to_string(),
                        "-File".to_string(),
                        script_path_arg,
                        payload_path_arg,
                        json,
                    ])
                    .expect("build command");
                    command.status().await.expect("run hook command");
                    HookResult::Success
                })
            }),
        };

        let payload = hook_payload("4");
        let expected = to_string(&payload)?;

        let hooks = Hooks {
            agent_turn_complete: vec![hook],
            ..Hooks::default()
        };
        let outcomes = hooks.dispatch(payload).await;
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0].result, HookResult::Success));

        let contents = timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(contents) = fs::read_to_string(&payload_path)
                    && !contents.is_empty()
                {
                    return contents;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;

        assert_eq!(contents, expected);
        Ok(())
    }
}
