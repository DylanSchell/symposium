//! Pi's hook wire format.
//!
//! Pi has no shell hooks, so the other half of this protocol is symposium's own
//! TypeScript extension (`src/agents/assets/pi-extension.ts`, installed to
//! `<agent-dir>/extensions/symposium.ts`). It spawns
//! `cargo-agents hook pi <event>` with one of the payloads below on stdin and
//! applies the JSON on stdout to the running session.
//!
//! Because symposium owns both halves, the format stays close to the canonical
//! one: Claude-style snake_case input fields, and for output the two things Pi
//! can actually act on — `{ block, reason, updatedInput }` on a tool call, and
//! `additionalContext` everywhere else. Pi has no permission-prompt concept for
//! a rewritten call, so `updatedInput` applies directly (unlike Claude, which
//! needs `permissionDecision: "ask"` to show a rewrite to the user).

use serde::{Deserialize, Serialize};

use crate::hook_schema::{
    Agent, AgentHookEvent, AgentHookInput, AgentHookOutput, ErasedAgentHookEvent,
    erase_agent_hook_event, symposium,
};

pub struct Pi;
impl Agent for Pi {
    fn event(&self, event: super::HookEvent) -> Option<Box<dyn ErasedAgentHookEvent>> {
        match event {
            super::HookEvent::PreToolUse => Some(erase_agent_hook_event(PiPreToolUseEvent)),
            super::HookEvent::PostToolUse => Some(erase_agent_hook_event(PiPostToolUseEvent)),
            super::HookEvent::UserPromptSubmit => {
                Some(erase_agent_hook_event(PiUserPromptSubmitEvent))
            }
            super::HookEvent::SessionStart => Some(erase_agent_hook_event(PiSessionStartEvent)),
            super::HookEvent::Stop => Some(erase_agent_hook_event(PiStopEvent)),
            _ => None,
        }
    }
}

macro_rules! pi_event {
    ($event:ident, $input:ident, $output:ident) => {
        pub struct $event;
        impl AgentHookEvent for $event {
            type Input = $input;
            type Output = $output;
        }
    };
}

pi_event!(PiPreToolUseEvent, PiPreToolUseInput, PiPreToolUseOutput);
pi_event!(PiPostToolUseEvent, PiPostToolUseInput, PiPostToolUseOutput);
pi_event!(
    PiUserPromptSubmitEvent,
    PiUserPromptSubmitInput,
    PiUserPromptSubmitOutput
);
pi_event!(
    PiSessionStartEvent,
    PiSessionStartInput,
    PiSessionStartOutput
);
pi_event!(PiStopEvent, PiStopInput, PiStopOutput);

/// `session_id` / `cwd` arrive in the flattened tail of every payload.
fn session_id(rest: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    rest.get("session_id")
        .and_then(|v| v.as_str())
        .map(String::from)
}

fn cwd(rest: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    rest.get("cwd").and_then(|v| v.as_str()).map(String::from)
}

fn insert_context(
    rest: &mut serde_json::Map<String, serde_json::Value>,
    session_id: Option<&str>,
    cwd: Option<&str>,
) {
    if let Some(s) = session_id {
        rest.insert(
            "session_id".into(),
            serde_json::Value::String(s.to_string()),
        );
    }
    if let Some(c) = cwd {
        rest.insert("cwd".into(), serde_json::Value::String(c.to_string()));
    }
}

/// The one thing symposium has to say on the events that are not tool calls.
/// Pi has no hook-event envelope, so it is a bare `additionalContext` field.
fn context_only(event: &symposium::OutputEvent) -> Option<String> {
    event.additional_context().map(String::from)
}

macro_rules! simple_output {
    ($output:ident, $variant:ident, $type:ident) => {
        impl AgentHookOutput for $output {
            fn parse_output(output: &[u8]) -> anyhow::Result<Self> {
                if output.is_empty() {
                    return Ok(Self::default());
                }
                Ok(serde_json::from_slice(output)?)
            }
            fn from_symposium(event: &symposium::OutputEvent) -> Self {
                Self {
                    additional_context: context_only(event),
                    rest: serde_json::Map::new(),
                }
            }
            fn to_symposium(&self) -> symposium::OutputEvent {
                symposium::OutputEvent::$variant(symposium::$type::new(
                    self.additional_context.clone(),
                ))
            }
            fn to_hook_output(&self) -> serde_json::Value {
                serde_json::to_value(self).unwrap()
            }
            fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
                self
            }
        }
    };
}

// ── PreToolUse ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiPreToolUseInput {
    pub hook_event_name: String,
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: serde_json::Value,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

/// Pi's `tool_call` handler answers with `{ block, reason }` and reads rewritten
/// arguments from `updatedInput`, which it applies by mutating `event.input`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PiPreToolUseOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(
        rename = "updatedInput",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub updated_input: Option<serde_json::Value>,
    #[serde(
        rename = "additionalContext",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub additional_context: Option<String>,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

impl AgentHookInput for PiPreToolUseInput {
    fn parse_input(payload: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(payload)?)
    }
    fn to_symposium(&self) -> symposium::InputEvent {
        symposium::InputEvent::PreToolUse(symposium::PreToolUseInput::new(
            self.tool_name.clone(),
            self.tool_input.clone(),
            session_id(&self.rest),
            cwd(&self.rest),
        ))
    }
    fn from_symposium(event: &symposium::InputEvent) -> Self {
        let symposium::InputEvent::PreToolUse(p) = event else {
            panic!("wrong event type")
        };
        let mut rest = serde_json::Map::new();
        insert_context(&mut rest, p.session_id.as_deref(), p.cwd.as_deref());
        Self {
            hook_event_name: "PreToolUse".into(),
            tool_name: p.tool_name.clone(),
            tool_input: p.tool_input.clone(),
            rest,
        }
    }
    fn to_string(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(Into::into)
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

impl AgentHookOutput for PiPreToolUseOutput {
    fn parse_output(output: &[u8]) -> anyhow::Result<Self> {
        if output.is_empty() {
            return Ok(Self::default());
        }
        Ok(serde_json::from_slice(output)?)
    }
    fn from_symposium(event: &symposium::OutputEvent) -> Self {
        let symposium::OutputEvent::PreToolUse(o) = event else {
            return Self::default();
        };
        let denied = matches!(o.decision, symposium_sdk::hook::Decision::Deny);
        if !denied && o.updated_input.is_none() && o.additional_context.is_none() {
            return Self::default();
        }
        Self {
            block: denied.then_some(true),
            // A denial's reason is what Pi shows the model, so the plugin's
            // context becomes the reason rather than riding alongside it.
            reason: denied.then(|| o.additional_context.clone()).flatten(),
            updated_input: (!denied).then(|| o.updated_input.clone()).flatten(),
            additional_context: (!denied).then(|| o.additional_context.clone()).flatten(),
            rest: serde_json::Map::new(),
        }
    }
    fn to_symposium(&self) -> symposium::OutputEvent {
        let blocked = self.block == Some(true);
        let context = match blocked {
            true => self
                .reason
                .clone()
                .or_else(|| self.additional_context.clone()),
            false => self.additional_context.clone(),
        };
        symposium::OutputEvent::PreToolUse(symposium::PreToolUseOutput::new(
            if blocked {
                symposium_sdk::hook::Decision::Deny
            } else {
                symposium_sdk::hook::Decision::Allow
            },
            context,
            self.updated_input.clone(),
        ))
    }
    fn to_hook_output(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap()
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

// ── PostToolUse ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiPostToolUseInput {
    pub hook_event_name: String,
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: serde_json::Value,
    /// Pi hands the extension `content` / `isError`; the extension forwards them
    /// under this field so plugins written for other agents see the same shape.
    #[serde(default)]
    pub tool_response: serde_json::Value,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PiPostToolUseOutput {
    #[serde(
        rename = "additionalContext",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub additional_context: Option<String>,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

impl AgentHookInput for PiPostToolUseInput {
    fn parse_input(payload: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(payload)?)
    }
    fn to_symposium(&self) -> symposium::InputEvent {
        symposium::InputEvent::PostToolUse(symposium::PostToolUseInput::new(
            self.tool_name.clone(),
            self.tool_input.clone(),
            self.tool_response.clone(),
            session_id(&self.rest),
            cwd(&self.rest),
        ))
    }
    fn from_symposium(event: &symposium::InputEvent) -> Self {
        let symposium::InputEvent::PostToolUse(p) = event else {
            panic!("wrong event type")
        };
        let mut rest = serde_json::Map::new();
        insert_context(&mut rest, p.session_id.as_deref(), p.cwd.as_deref());
        Self {
            hook_event_name: "PostToolUse".into(),
            tool_name: p.tool_name.clone(),
            tool_input: p.tool_input.clone(),
            tool_response: p.tool_response.clone(),
            rest,
        }
    }
    fn to_string(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(Into::into)
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

simple_output!(PiPostToolUseOutput, PostToolUse, PostToolUseOutput);

// ── UserPromptSubmit ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiUserPromptSubmitInput {
    pub hook_event_name: String,
    #[serde(default)]
    pub prompt: String,
    /// Pi starts a run from a submitted prompt *or* from `pi.sendMessage()` with
    /// `triggerTurn: true`. Only the former is a user prompt; the extension
    /// forwards `messageType` so a custom-message run dispatches nothing.
    #[serde(default = "default_message_type")]
    pub message_type: String,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

fn default_message_type() -> String {
    "user".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PiUserPromptSubmitOutput {
    #[serde(
        rename = "additionalContext",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub additional_context: Option<String>,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

impl AgentHookInput for PiUserPromptSubmitInput {
    fn parse_input(payload: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(payload)?)
    }
    fn to_symposium(&self) -> symposium::InputEvent {
        symposium::InputEvent::UserPromptSubmit(symposium::UserPromptSubmitInput::new(
            self.prompt.clone(),
            session_id(&self.rest),
            cwd(&self.rest),
        ))
    }
    fn from_symposium(event: &symposium::InputEvent) -> Self {
        let symposium::InputEvent::UserPromptSubmit(p) = event else {
            panic!("wrong event type")
        };
        let mut rest = serde_json::Map::new();
        insert_context(&mut rest, p.session_id.as_deref(), p.cwd.as_deref());
        Self {
            hook_event_name: "UserPromptSubmit".into(),
            prompt: p.prompt.clone(),
            message_type: default_message_type(),
            rest,
        }
    }
    fn to_string(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(Into::into)
    }
    fn should_dispatch(&self) -> bool {
        self.message_type == "user"
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

simple_output!(
    PiUserPromptSubmitOutput,
    UserPromptSubmit,
    UserPromptSubmitOutput
);

// ── SessionStart ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiSessionStartInput {
    pub hook_event_name: String,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

/// Pi's `session_start` is notification-only: nothing a handler returns reaches
/// the conversation. The extension therefore holds this context and injects it
/// as a message on the first `before_agent_start` of the session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PiSessionStartOutput {
    #[serde(
        rename = "additionalContext",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub additional_context: Option<String>,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

impl AgentHookInput for PiSessionStartInput {
    fn parse_input(payload: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(payload)?)
    }
    fn to_symposium(&self) -> symposium::InputEvent {
        symposium::InputEvent::SessionStart(symposium::SessionStartInput::new(
            session_id(&self.rest),
            cwd(&self.rest),
        ))
    }
    fn from_symposium(event: &symposium::InputEvent) -> Self {
        let symposium::InputEvent::SessionStart(p) = event else {
            panic!("wrong event type")
        };
        let mut rest = serde_json::Map::new();
        insert_context(&mut rest, p.session_id.as_deref(), p.cwd.as_deref());
        Self {
            hook_event_name: "SessionStart".into(),
            rest,
        }
    }
    fn to_string(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(Into::into)
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

simple_output!(PiSessionStartOutput, SessionStart, SessionStartOutput);

// ── Stop ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiStopInput {
    pub hook_event_name: String,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PiStopOutput {
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

impl AgentHookInput for PiStopInput {
    fn parse_input(payload: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(payload)?)
    }
    fn to_symposium(&self) -> symposium::InputEvent {
        symposium::InputEvent::Stop(symposium::StopInput::new(
            session_id(&self.rest),
            cwd(&self.rest),
        ))
    }
    fn from_symposium(event: &symposium::InputEvent) -> Self {
        let symposium::InputEvent::Stop(p) = event else {
            panic!("wrong event type")
        };
        let mut rest = serde_json::Map::new();
        insert_context(&mut rest, p.session_id.as_deref(), p.cwd.as_deref());
        Self {
            hook_event_name: "Stop".into(),
            rest,
        }
    }
    fn to_string(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).map_err(Into::into)
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

impl AgentHookOutput for PiStopOutput {
    fn parse_output(output: &[u8]) -> anyhow::Result<Self> {
        if output.is_empty() {
            return Ok(Self::default());
        }
        Ok(serde_json::from_slice(output)?)
    }
    fn from_symposium(_event: &symposium::OutputEvent) -> Self {
        // `agent_end` is notification-only, and Stop hooks inject no context.
        Self::default()
    }
    fn to_symposium(&self) -> symposium::OutputEvent {
        symposium::OutputEvent::Stop(symposium::StopOutput::default())
    }
    fn to_hook_output(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap()
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook_schema::HookEvent;

    fn handler(event: HookEvent) -> Box<dyn ErasedAgentHookEvent> {
        Pi.event(event).expect("pi supports this event")
    }

    /// The extension's payloads, pinned: it is shipped as a string asset, so a
    /// renamed field here would break at runtime with nothing in Rust to catch it.
    #[test]
    fn pre_tool_use_input_parses_the_extensions_payload() {
        let payload = r#"{
            "hook_event_name": "PreToolUse",
            "tool_name": "bash",
            "tool_input": { "command": "cargo test" },
            "session_id": "01JABC",
            "cwd": "/work/project"
        }"#;
        let parsed = handler(HookEvent::PreToolUse).parse_input(payload).unwrap();
        let symposium::InputEvent::PreToolUse(input) = parsed.to_symposium() else {
            panic!("wrong event type")
        };
        assert_eq!(input.tool_name, "bash");
        assert_eq!(input.tool_input["command"], "cargo test");
        assert_eq!(input.session_id.as_deref(), Some("01JABC"));
        assert_eq!(input.cwd.as_deref(), Some("/work/project"));
    }

    #[test]
    fn post_tool_use_input_carries_the_tool_response() {
        let payload = r#"{
            "hook_event_name": "PostToolUse",
            "tool_name": "bash",
            "tool_input": { "command": "cargo test" },
            "tool_response": { "content": [{ "type": "text", "text": "ok" }], "isError": false }
        }"#;
        let parsed = handler(HookEvent::PostToolUse)
            .parse_input(payload)
            .unwrap();
        let symposium::InputEvent::PostToolUse(input) = parsed.to_symposium() else {
            panic!("wrong event type")
        };
        assert_eq!(input.tool_response["isError"], false);
        assert_eq!(input.tool_response["content"][0]["text"], "ok");
    }

    #[test]
    fn lifecycle_inputs_read_session_id_and_cwd() {
        let payload = r#"{"hook_event_name":"SessionStart","session_id":"S","cwd":"/w"}"#;
        let parsed = handler(HookEvent::SessionStart)
            .parse_input(payload)
            .unwrap();
        let symposium::InputEvent::SessionStart(input) = parsed.to_symposium() else {
            panic!("wrong event type")
        };
        assert_eq!(input.session_id.as_deref(), Some("S"));
        assert_eq!(input.cwd.as_deref(), Some("/w"));
    }

    /// A denial has to reach Pi as `block: true`, and its reason as `reason`:
    /// Pi reads no exit code from an extension handler.
    #[test]
    fn deny_becomes_a_block_with_a_reason() {
        let deny = symposium::OutputEvent::PreToolUse(symposium::PreToolUseOutput::deny(
            "do not rm -rf".to_string(),
        ));
        let out = handler(HookEvent::PreToolUse)
            .translate_output(&deny)
            .to_hook_output();
        assert_eq!(out["block"], true);
        assert_eq!(out["reason"], "do not rm -rf");
        assert!(out.get("updatedInput").is_none());
    }

    /// Pi patches arguments in place with no user prompt, so a rewrite goes
    /// through as `updatedInput` on its own.
    #[test]
    fn rewritten_input_is_returned_without_a_decision() {
        let rewrite = symposium::OutputEvent::PreToolUse(symposium::PreToolUseOutput::new(
            symposium_sdk::hook::Decision::Allow,
            None,
            Some(serde_json::json!({ "command": "cargo nextest run" })),
        ));
        let out = handler(HookEvent::PreToolUse)
            .translate_output(&rewrite)
            .to_hook_output();
        assert!(out.get("block").is_none());
        assert_eq!(out["updatedInput"]["command"], "cargo nextest run");

        let back = handler(HookEvent::PreToolUse)
            .parse_output(&serde_json::to_vec(&out).unwrap())
            .unwrap()
            .to_symposium();
        let symposium::OutputEvent::PreToolUse(back) = back else {
            panic!("wrong event type")
        };
        assert_eq!(back.decision, symposium_sdk::hook::Decision::Allow);
        assert_eq!(
            back.updated_input,
            Some(serde_json::json!({ "command": "cargo nextest run" }))
        );
    }

    #[test]
    fn an_allow_with_nothing_to_say_emits_an_empty_object() {
        let allow = symposium::OutputEvent::PreToolUse(symposium::PreToolUseOutput::new(
            symposium_sdk::hook::Decision::Allow,
            None,
            None,
        ));
        let out = handler(HookEvent::PreToolUse)
            .translate_output(&allow)
            .to_hook_output();
        assert_eq!(out, serde_json::json!({}));
    }

    #[test]
    fn context_is_returned_as_additional_context() {
        for (event, output) in [
            (
                HookEvent::SessionStart,
                symposium::OutputEvent::SessionStart(symposium::SessionStartOutput::new(Some(
                    "symposium context".into(),
                ))),
            ),
            (
                HookEvent::UserPromptSubmit,
                symposium::OutputEvent::UserPromptSubmit(symposium::UserPromptSubmitOutput::new(
                    Some("symposium context".into()),
                )),
            ),
            (
                HookEvent::PostToolUse,
                symposium::OutputEvent::PostToolUse(symposium::PostToolUseOutput::new(Some(
                    "symposium context".into(),
                ))),
            ),
        ] {
            let out = handler(event).translate_output(&output).to_hook_output();
            assert_eq!(out["additionalContext"], "symposium context", "{event:?}");
        }
    }

    #[test]
    fn stop_emits_nothing() {
        let out = handler(HookEvent::Stop)
            .translate_output(&symposium::OutputEvent::Stop(
                symposium::StopOutput::default(),
            ))
            .to_hook_output();
        assert_eq!(out, serde_json::json!({}));
    }

    /// Pi starts a run for `pi.sendMessage()` calls too, which are not user
    /// prompts: those must reach neither sync nor a plugin hook.
    #[test]
    fn a_custom_message_run_is_not_a_user_prompt() {
        let custom = handler(HookEvent::UserPromptSubmit)
            .parse_input(
                r#"{"hook_event_name":"UserPromptSubmit","prompt":"x","message_type":"custom"}"#,
            )
            .unwrap();
        assert!(!custom.should_dispatch());

        let user = handler(HookEvent::UserPromptSubmit)
            .parse_input(r#"{"hook_event_name":"UserPromptSubmit","prompt":"x"}"#)
            .unwrap();
        assert!(user.should_dispatch(), "message_type defaults to user");
    }
}
