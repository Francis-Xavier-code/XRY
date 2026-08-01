use crate::agent::{Agent, AgentEvent, AgentMode, AgentTurnControl};
use crate::cli::{build_tool_registry, WebArgs};
use crate::config::{ActiveProviderModelConfig, AppConfig};
use crate::llm::{ChatResult, ChatStreamKind, OpenAiCompatibleClient, Usage};
use crate::memory::MemoryStore;
use crate::paths::GqyPaths;
use crate::question::{self, QuestionAnswers, QuestionRequest, QuestionResponse};
use crate::state::{
    ImageAsset, QueuedPrompt, StateStore, Turn, TurnFollowup, TurnStatus, UsageSnapshot,
};
use crate::tools::{self, CommandOutputStream};
use anyhow::{Context, Result};
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, HOST,
    ORIGIN, REFERRER_POLICY, RETRY_AFTER, SET_COOKIE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::stream::{self, Stream};
use futures_util::{SinkExt, StreamExt};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::future::IntoFuture;
use std::io::{self, IsTerminal, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path as FilePath, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, oneshot};

const JSON_BODY_LIMIT: usize = 4 * 1024 * 1024;
const MAX_CONTENT_CHARS: usize = 20_000;
const MAX_PROMPT_DOCUMENT_CHARS: usize = 200_000;
const MAX_PROMPT_DOCUMENTS: usize = 128;
const MAX_SECRET_CHARS: usize = 100_000;
const EVENT_CAPACITY: usize = 4096;
const AUTH_COOKIE: &str = "hilia_session";
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const LOGIN_ATTEMPT_LIMIT: u8 = 5;

const INDEX_HTML: &str = include_str!("../web/index.html");
const STYLES_CSS: &str = include_str!("../web/styles.css");
const APP_JS: &str = include_str!("../web/app.js");
const MINI_HTML: &str = include_str!("../web/mini.html");
const MINI_CSS: &str = include_str!("../web/mini.css");
const MINI_JS: &str = include_str!("../web/mini.js");
const CREDITS_HTML: &str = include_str!("../web/credits.html");
const CREDITS_JS: &str = include_str!("../web/credits.js");
const HILIA_LOGO: &[u8] = include_bytes!("../pics/Hilia-avatar.png");
const HILIA_WALLPAPER: &[u8] = include_bytes!("../pics/Hilia-image.png");
const PROVIDER_ICONS: &str = include_str!("../web/assets/provider-icons.svg");
const QRCODE_JS: &str = include_str!("../web/assets/qrcode.min.js");

#[derive(Clone)]
struct WebState {
    auth: WebAuth,
    boot_id: Arc<str>,
    paths: GqyPaths,
    manager: Arc<Mutex<ManagerState>>,
    state_store: StateStore,
    events: EventHub,
    questions: QuestionBroker,
    actor_tx: mpsc::UnboundedSender<ActorCommand>,
    /// 局域网直连（APK 不经公网中继）：配对码 / 设备 / 在线连接
    direct: Arc<DirectHub>,
    /// 实际监听端口（pairing_create 生成直连地址用）
    listen_port: u16,
}

#[derive(Clone)]
struct WebAuth {
    password_digest: Option<[u8; 32]>,
    sessions: Arc<Mutex<HashSet<String>>>,
    attempts: Arc<Mutex<HashMap<IpAddr, LoginAttempt>>>,
}

#[derive(Clone, Copy)]
struct LoginAttempt {
    window_started: Instant,
    failures: u8,
}

#[derive(Debug, Clone, Copy)]
enum LoginFailure {
    Invalid,
    RateLimited,
}

impl WebAuth {
    fn new(password: Option<&str>) -> Self {
        let password_digest = password.map(|password| {
            let mut digest = Sha256::new();
            digest.update(password.as_bytes());
            digest.finalize().into()
        });
        Self {
            password_digest,
            sessions: Arc::new(Mutex::new(HashSet::new())),
            attempts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn required(&self) -> bool {
        self.password_digest.is_some()
    }

    fn is_authenticated(&self, supplied: Option<&str>) -> bool {
        if !self.required() {
            return true;
        }
        supplied.is_some_and(|token| self.sessions.lock().unwrap().contains(token))
    }

    fn login(&self, peer: IpAddr, password: &str) -> std::result::Result<String, LoginFailure> {
        let Some(expected) = self.password_digest else {
            return Ok(String::new());
        };
        let now = Instant::now();
        {
            let mut attempts = self.attempts.lock().unwrap();
            let entry = attempts.entry(peer).or_insert(LoginAttempt {
                window_started: now,
                failures: 0,
            });
            if now.duration_since(entry.window_started) >= LOGIN_WINDOW {
                entry.window_started = now;
                entry.failures = 0;
            }
            if entry.failures >= LOGIN_ATTEMPT_LIMIT {
                return Err(LoginFailure::RateLimited);
            }
        }

        let mut digest = Sha256::new();
        digest.update(password.as_bytes());
        let supplied: [u8; 32] = digest.finalize().into();
        if !constant_time_eq(&supplied, &expected) {
            let mut attempts = self.attempts.lock().unwrap();
            if let Some(entry) = attempts.get_mut(&peer) {
                entry.failures = entry.failures.saturating_add(1);
            }
            return Err(LoginFailure::Invalid);
        }

        let token = random_token(32);
        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(token.clone());
        if sessions.len() > 64 {
            sessions.clear();
            sessions.insert(token.clone());
        }
        Ok(token)
    }
}

struct ManagerState {
    config: AppConfig,
    active_run_id: Option<String>,
    admin_busy: bool,
    context: ContextSnapshot,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct ContextSnapshot {
    tokens: u64,
    window: Option<usize>,
}

enum ActorCommand {
    StartTurn {
        run_id: String,
        content: String,
        mode: AgentMode,
    },
    Cancel {
        run_id: String,
    },
    SetModels {
        models: Vec<ActiveProviderModelConfig>,
        reply: oneshot::Sender<std::result::Result<(), AdminFailure>>,
    },
    ApplyConfig {
        config: AppConfig,
        prompts: PromptDocuments,
        reset_conversation: bool,
        reply: oneshot::Sender<std::result::Result<(), AdminFailure>>,
    },
    ResetConversation {
        reply: oneshot::Sender<std::result::Result<(), AdminFailure>>,
    },
    Shutdown,
}

#[derive(Debug)]
enum AdminFailure {
    Invalid(String),
    Internal(String),
}

#[derive(Clone, Debug)]
struct EventRecord {
    id: u64,
    kind: String,
    data: String,
}

#[derive(Clone)]
struct EventHub {
    inner: Arc<Mutex<EventHubInner>>,
    sender: broadcast::Sender<EventRecord>,
}

struct EventHubInner {
    next_id: u64,
    records: VecDeque<EventRecord>,
}

struct EventSubscription {
    pending: VecDeque<EventRecord>,
    receiver: broadcast::Receiver<EventRecord>,
}

impl EventHub {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            inner: Arc::new(Mutex::new(EventHubInner {
                next_id: 1,
                records: VecDeque::with_capacity(EVENT_CAPACITY),
            })),
            sender,
        }
    }

    fn publish(&self, kind: impl Into<String>, data: Value) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id;
        inner.next_id = inner.next_id.saturating_add(1);
        let record = EventRecord {
            id,
            kind: kind.into(),
            data: serde_json::to_string(&data)
                .unwrap_or_else(|_| "{\"error\":\"event serialization failed\"}".to_string()),
        };
        if inner.records.len() == EVENT_CAPACITY {
            inner.records.pop_front();
        }
        inner.records.push_back(record.clone());
        let _ = self.sender.send(record);
        id
    }

    fn latest_id(&self) -> u64 {
        self.inner.lock().unwrap().next_id.saturating_sub(1)
    }

    fn subscribe_after(&self, after: u64) -> EventSubscription {
        let mut inner = self.inner.lock().unwrap();
        let receiver = self.sender.subscribe();
        let pending = replay_records(&mut inner, after);
        EventSubscription { pending, receiver }
    }

    fn replay_after(&self, after: u64) -> VecDeque<EventRecord> {
        replay_records(&mut self.inner.lock().unwrap(), after)
    }
}

fn replay_records(inner: &mut EventHubInner, after: u64) -> VecDeque<EventRecord> {
    if after > inner.next_id.saturating_sub(1) {
        return resync_record(inner);
    }
    let Some(oldest) = inner.records.front().map(|record| record.id) else {
        return VecDeque::new();
    };
    if after < oldest.saturating_sub(1) {
        return resync_record(inner);
    }
    inner
        .records
        .iter()
        .filter(|record| record.id > after)
        .cloned()
        .collect()
}

fn resync_record(inner: &mut EventHubInner) -> VecDeque<EventRecord> {
    let id = inner.next_id;
    inner.next_id = inner.next_id.saturating_add(1);
    VecDeque::from([EventRecord {
        id,
        kind: "resync_required".to_string(),
        data: json!({ "latest_event_id": id }).to_string(),
    }])
}

#[derive(Clone)]
struct QuestionBroker {
    pending: Arc<Mutex<HashMap<String, PendingQuestion>>>,
}

struct PendingQuestion {
    run_id: String,
    request: QuestionRequest,
    responder: oneshot::Sender<QuestionResponse>,
}

#[derive(Debug)]
enum AnswerFailure {
    NotFound,
    Invalid(String),
    Gone,
}

impl QuestionBroker {
    fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn insert(
        &self,
        run_id: &str,
        request: QuestionRequest,
        responder: oneshot::Sender<QuestionResponse>,
    ) -> String {
        let mut pending = self.pending.lock().unwrap();
        loop {
            let question_id = random_id("question", 18);
            if !pending.contains_key(&question_id) {
                pending.insert(
                    question_id.clone(),
                    PendingQuestion {
                        run_id: run_id.to_string(),
                        request,
                        responder,
                    },
                );
                return question_id;
            }
        }
    }

    fn answer<F>(
        &self,
        question_id: &str,
        answers: QuestionAnswers,
        before_resume: F,
    ) -> std::result::Result<(), AnswerFailure>
    where
        F: FnOnce(&str, &QuestionAnswers),
    {
        let mut all_pending = self.pending.lock().unwrap();
        let request = all_pending
            .get(question_id)
            .map(|pending| pending.request.clone())
            .ok_or(AnswerFailure::NotFound)?;
        let answers = normalize_answers(&request, answers).map_err(AnswerFailure::Invalid)?;
        let pending = all_pending
            .remove(question_id)
            .ok_or(AnswerFailure::NotFound)?;
        let run_id = pending.run_id;
        pending
            .responder
            .send(QuestionResponse::Answered(answers.clone()))
            .map_err(|_| AnswerFailure::Gone)?;
        before_resume(&run_id, &answers);
        Ok(())
    }

    fn cancel_run(&self, run_id: &str) {
        let cancelled = {
            let mut pending = self.pending.lock().unwrap();
            let ids = pending
                .iter()
                .filter(|(_, question)| question.run_id == run_id)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id))
                .collect::<Vec<_>>()
        };
        for pending in cancelled {
            let _ = pending.responder.send(QuestionResponse::Cancelled);
        }
    }
}

struct RunEventMapper {
    run_id: String,
    events: EventHub,
    questions: QuestionBroker,
    state_store: StateStore,
    turn_id: Option<String>,
    tool_counter: u64,
    active_tool: Option<ActiveTool>,
}

struct ActiveTool {
    id: String,
    name: String,
    event_name: String,
}

impl RunEventMapper {
    fn new(
        run_id: String,
        events: EventHub,
        questions: QuestionBroker,
        state_store: StateStore,
    ) -> Self {
        Self {
            run_id,
            events,
            questions,
            state_store,
            turn_id: None,
            tool_counter: 0,
            active_tool: None,
        }
    }

    fn publish(&self, kind: &str, data: Value) {
        self.events.publish(kind, data);
    }

    fn next_tool(&mut self, event_name: String) -> ActiveTool {
        self.tool_counter = self.tool_counter.saturating_add(1);
        ActiveTool {
            id: format!("{}_tool_{}", self.run_id, self.tool_counter),
            name: real_tool_name(&event_name).to_string(),
            event_name,
        }
    }

    fn handle(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStarted { turn_id } => {
                self.turn_id = Some(turn_id.clone());
                self.publish(
                    "turn.started",
                    json!({ "run_id": self.run_id, "turn_id": turn_id }),
                );
            }
            AgentEvent::Chunk(chunk) => match chunk.kind {
                ChatStreamKind::Content => self.publish(
                    "assistant.delta",
                    json!({ "run_id": self.run_id, "delta": chunk.text }),
                ),
                ChatStreamKind::Reasoning => self.publish(
                    "reasoning.delta",
                    json!({ "run_id": self.run_id, "delta": chunk.text }),
                ),
                _ => {}
            },
            AgentEvent::ReasoningStart { .. } => {
                self.publish("reasoning.start", json!({ "run_id": self.run_id }))
            }
            AgentEvent::ReasoningReset { .. } => {
                self.publish("reasoning.reset", json!({ "run_id": self.run_id }))
            }
            AgentEvent::ReasoningPartStart { .. } => {
                self.publish("reasoning.part_start", json!({ "run_id": self.run_id }))
            }
            AgentEvent::ReasoningPartEnd { .. } => {
                self.publish("reasoning.part_end", json!({ "run_id": self.run_id }))
            }
            AgentEvent::ReasoningTitle(title) => self.publish(
                "reasoning.title",
                json!({ "run_id": self.run_id, "title": title }),
            ),
            AgentEvent::ToolCall { name, arguments } => {
                let tool = self.next_tool(name);
                self.publish(
                    "tool.started",
                    json!({
                        "run_id": self.run_id,
                        "tool_id": tool.id,
                        "name": tool.name,
                        "display_name": tools::readable_tool_name(&tool.event_name),
                        "arguments": arguments,
                    }),
                );
                self.active_tool = Some(tool);
            }
            AgentEvent::ToolProgress { name, message } => {
                let (tool_id, tool_name) = self.tool_identity(&name);
                self.publish(
                    "tool.progress",
                    json!({
                        "run_id": self.run_id,
                        "tool_id": tool_id,
                        "name": tool_name,
                        "message": message,
                    }),
                );
            }
            AgentEvent::CommandOutput {
                name,
                stream,
                chunk,
            } => {
                let (tool_id, tool_name) = self.tool_identity(&name);
                let stream = match stream {
                    CommandOutputStream::Stdout => "stdout",
                    CommandOutputStream::Stderr => "stderr",
                };
                self.publish(
                    "tool.output",
                    json!({
                        "run_id": self.run_id,
                        "tool_id": tool_id,
                        "name": tool_name,
                        "stream": stream,
                        "output": String::from_utf8_lossy(&chunk),
                    }),
                );
            }
            AgentEvent::ToolResult { name, ok, output } => {
                let tool = self
                    .active_tool
                    .take()
                    .unwrap_or_else(|| self.next_tool(name));
                self.publish(
                    "tool.finished",
                    json!({
                        "run_id": self.run_id,
                        "tool_id": tool.id,
                        "name": tool.name,
                        "ok": ok,
                        "output": output,
                    }),
                );
            }
            AgentEvent::PrepareForExternalOutput { ready } => {
                let _ = ready.send(false);
            }
            AgentEvent::Image { name, path, alt } => {
                let (tool_id, tool_name) = self.tool_identity(&name);
                let hide_caption = tool_name == "show_meme";
                let Some(turn_id) = self.turn_id.as_deref() else {
                    self.publish(
                        "tool.image",
                        json!({
                            "run_id": self.run_id,
                            "tool_id": tool_id,
                            "name": tool_name,
                            "error": "image could not be associated with the current turn",
                        }),
                    );
                    return;
                };
                match self
                    .state_store
                    .save_image_asset(turn_id, Some(&tool_id), &path, &alt)
                {
                    Ok(asset) => self.publish(
                        "tool.image",
                        json!({
                            "run_id": self.run_id,
                            "tool_id": tool_id,
                            "name": tool_name,
                            "asset": SafeImageAsset::from_asset(asset, hide_caption),
                        }),
                    ),
                    Err(error) => {
                        tracing::warn!(
                            run_id = %self.run_id,
                            tool = %tool_name,
                            error = %error,
                            "failed to persist a WebUI image"
                        );
                        self.publish(
                            "tool.image",
                            json!({
                                "run_id": self.run_id,
                                "tool_id": tool_id,
                                "name": tool_name,
                                "error": "image could not be added to the WebUI",
                            }),
                        );
                    }
                }
            }
            AgentEvent::AskQuestion { request, responder } => {
                let question_id = self
                    .questions
                    .insert(&self.run_id, request.clone(), responder);
                let (tool_id, tool_name) = self.tool_identity("ask_question");
                self.publish(
                    "question.requested",
                    json!({
                        "run_id": self.run_id,
                        "question_id": question_id,
                        "tool_id": tool_id,
                        "name": tool_name,
                        "questions": request.questions,
                    }),
                );
            }
            AgentEvent::QueuedPromptsConsumed {
                prompt_ids,
                mode,
                provider_id,
                model,
            } => self.publish(
                "queue.consumed",
                json!({
                    "run_id": self.run_id,
                    "prompt_ids": prompt_ids,
                    "mode": mode_name(mode),
                    "provider_id": provider_id,
                    "model": model,
                }),
            ),
            AgentEvent::SpinnerTick => {}
            AgentEvent::CompactStart => {
                self.publish("context.compact_start", json!({ "run_id": self.run_id }))
            }
            AgentEvent::CompactChunk(chunk) => self.publish(
                "context.compact_delta",
                json!({ "run_id": self.run_id, "delta": chunk.text }),
            ),
            AgentEvent::CompactEnd => {
                self.publish("context.compact_end", json!({ "run_id": self.run_id }))
            }
            AgentEvent::PopStart => {
                self.publish("context.pop_start", json!({ "run_id": self.run_id }))
            }
            AgentEvent::PopEnd => self.publish("context.pop_end", json!({ "run_id": self.run_id })),
        }
    }

    fn tool_identity(&self, fallback: &str) -> (String, String) {
        self.active_tool
            .as_ref()
            .map(|tool| (tool.id.clone(), tool.name.clone()))
            .unwrap_or_else(|| {
                (
                    format!(
                        "{}_tool_{}",
                        self.run_id,
                        self.tool_counter.saturating_add(1)
                    ),
                    real_tool_name(fallback).to_string(),
                )
            })
    }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "WebUI request failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
    }

    fn bad_request(error: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::BAD_REQUEST, error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": { "message": self.message } })),
        )
            .into_response()
    }
}

#[derive(Default, Deserialize)]
struct EventsQuery {
    #[serde(default)]
    after: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateTurnRequest {
    content: String,
    mode: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueuePromptRequest {
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerQuestionRequest {
    answers: QuestionAnswers,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetModelsRequest {
    models: Vec<ActiveProviderModelConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateConfigRequest {
    config: Value,
    #[serde(default)]
    secrets: HashMap<String, SecretMutation>,
    prompts: PromptDocuments,
    #[serde(default)]
    reset_conversation: bool,
}

#[derive(Deserialize)]
#[serde(tag = "action", content = "value", rename_all = "snake_case")]
enum SecretMutation {
    Set(String),
    Clear,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PromptDocuments {
    #[serde(default)]
    personas: Vec<PromptDocument>,
    #[serde(default)]
    identities: Vec<PromptDocument>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PromptDocument {
    name: String,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_name: Option<String>,
}

#[derive(Serialize)]
struct ConfigResponse {
    config: Value,
    secret_states: HashMap<String, bool>,
    prompts: PromptDocuments,
    models: Vec<SafeModel>,
    multimodal_models: Vec<SafeModel>,
    display: WebDisplayConfig,
    context: ContextSnapshot,
}

#[derive(Serialize)]
struct BootstrapResponse {
    version: &'static str,
    boot_id: String,
    latest_event_id: u64,
    active_run_id: Option<String>,
    running_turn_id: Option<String>,
    external_queue_available: bool,
    turns: Vec<SafeTurn>,
    queued_prompts: Vec<SafeQueuedPrompt>,
    models: Vec<SafeModel>,
    display: WebDisplayConfig,
    context: ContextSnapshot,
    usage: SafeUsageSnapshot,
    capabilities: Capabilities,
}

#[derive(Serialize)]
struct Capabilities {
    multi_conversation: bool,
    attachments: bool,
    queue: bool,
}

#[derive(Clone, Serialize)]
struct WebDisplayConfig {
    reasoning: String,
    tool_calls: String,
    readable_tool_names: bool,
    command_output_lines: usize,
    mixed_model_endpoint_display: String,
    show_mixed_model_endpoint: bool,
}

#[derive(Clone, Serialize)]
struct SafeQueuedPrompt {
    id: String,
    content: String,
    submitted_at: String,
}

#[derive(Serialize)]
struct SafeModel {
    provider_id: String,
    provider_name: String,
    model: String,
    active: bool,
}

#[derive(Serialize)]
struct SafeTurn {
    id: String,
    seq: i64,
    status: &'static str,
    active_context: bool,
    user_content: String,
    assistant_content: String,
    assistant_reasoning: Option<String>,
    provider_id: Option<String>,
    model: Option<String>,
    user_timestamp: String,
    assistant_timestamp: Option<String>,
    token_total: u64,
    token_usage_estimated: bool,
    question_exchanges: Vec<crate::question::QuestionExchange>,
    followups: Vec<SafeFollowup>,
    assets: Vec<SafeImageAsset>,
}

#[derive(Serialize)]
struct SafeFollowup {
    id: String,
    content: String,
    submitted_at: String,
    preceding_assistant_content: Option<String>,
    preceding_assistant_reasoning: Option<String>,
    provider_id: Option<String>,
    model: Option<String>,
}

#[derive(Serialize)]
struct SafeImageAsset {
    id: String,
    url: String,
    mime: String,
    width: u32,
    height: u32,
    alt: String,
    hide_caption: bool,
}

#[derive(Serialize)]
struct SafeUsageSnapshot {
    requests: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    last_usage: Option<Usage>,
    last_conversation_usage: Option<Usage>,
}

#[derive(Serialize)]
struct ModelResponse {
    models: Vec<SafeModel>,
    display: WebDisplayConfig,
    context: ContextSnapshot,
}

pub async fn run(paths: GqyPaths, args: WebArgs) -> Result<()> {
    let password = resolve_web_password(&args)?;
    let bind_ip: IpAddr = args
        .host
        .parse()
        .with_context(|| format!("invalid WebUI host: {}", args.host))?;
    if !bind_ip.is_loopback() && password.is_none() {
        // 无密码时面板 API 仍仅限本机（lan_guard 中间件），局域网仅开放 APK 直连配对/消息
        println!(
            "⚠ 未设置访问密码：面板仅本机（127.0.0.1）可用，局域网仅支持 APK 直连配对与消息"
        );
    }
    AppConfig::init_files(&paths)?;
    let config = AppConfig::load_or_default(&paths)?;
    let state_store = StateStore::new(&paths)?;
    state_store.init_files()?;
    let client = OpenAiCompatibleClient::from_config(&config, &paths)?;
    let registry = build_tool_registry(&config, &paths, AgentMode::Normal, true)?;
    let agent = Agent::new(
        config.clone(),
        &paths,
        state_store.clone(),
        client,
        registry,
        AgentMode::Normal,
    )?;
    let context = ContextSnapshot {
        tokens: agent.effective_context_tokens()?,
        window: agent.context_window(),
    };

    let listener = tokio::net::TcpListener::bind(SocketAddr::new(bind_ip, args.port))
        .await
        .with_context(|| format!("binding 希尔娅 WebUI to {}:{}", args.host, args.port))?;
    let port = listener.local_addr()?.port();
    let boot_id: Arc<str> = random_id("boot", 18).into();
    let events = EventHub::new();
    let questions = QuestionBroker::new();
    let manager = Arc::new(Mutex::new(ManagerState {
        config: config.clone(),
        active_run_id: None,
        admin_busy: false,
        context,
    }));
    let (actor_tx, actor_join) = spawn_actor(
        agent,
        config,
        paths.clone(),
        state_store.clone(),
        manager.clone(),
        events.clone(),
        questions.clone(),
    )?;

    // 配置文件热重载：检测 HILIA_HOME/config/config.jsonc 被外部修改
    // （CLI `hilia config set`、直接编辑等），自动重建 agent 并通知前端，
    // 让菜单栏 / CLI / 面板三端配置始终同步。
    spawn_config_watcher(paths.clone(), actor_tx.clone(), events.clone());

    let state = WebState {
        auth: WebAuth::new(password.as_deref()),
        boot_id,
        paths,
        manager,
        state_store,
        events,
        questions,
        actor_tx: actor_tx.clone(),
        direct: Arc::new(DirectHub::new()),
        listen_port: port,
    };
    let app = router(state);
    // 只有设置了密码才把局域网地址列出来（无密码时仅回环可达）
    let urls = web_access_urls(port, password.is_some());
    for url in &urls {
        println!("希尔娅 WebUI: {url}");
    }
    std::io::stdout().flush().ok();
    if !args.no_open {
        open_browser(&format!("http://127.0.0.1:{port}"));
    }

    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .into_future();
    tokio::pin!(server);
    let serve_result = tokio::select! {
        result = &mut server => result,
        _ = shutdown_signal() => Ok(()),
    };
    let _ = actor_tx.send(ActorCommand::Shutdown);
    let actor_result = tokio::task::spawn_blocking(move || actor_join.join())
        .await
        .context("joining WebUI actor task")?
        .map_err(|_| anyhow::anyhow!("WebUI actor thread panicked"))?;
    serve_result.context("serving 希尔娅 WebUI")?;
    actor_result
}

fn router(state: WebState) -> Router {
    let app = Router::new()
        .route("/", get(index_asset))
        .route("/styles.css", get(styles_asset))
        .route("/app.js", get(app_asset))
        .route("/mini", get(mini_asset))
        .route("/mini.css", get(mini_css_asset))
        .route("/mini.js", get(mini_js_asset))
        .route("/credits", get(credits_asset))
        .route("/credits.js", get(credits_js_asset))
        .route("/api/credits/overview", get(credits_overview))
        .route("/api/credits/classes", get(credits_classes).post(credits_class_add))
        .route(
            "/api/credits/classes/{id}",
            put(credits_class_update).delete(credits_class_delete),
        )
        .route("/api/credits/types", get(credits_types).post(credits_type_add))
        .route("/api/credits/students", get(credits_students).post(credits_student_add))
        .route(
            "/api/credits/students/{id}",
            put(credits_student_update).delete(credits_student_delete),
        )
        .route("/api/credits/records", get(credits_records).post(credits_record_add))
        .route(
            "/api/credits/records/{id}",
            put(credits_record_update).delete(credits_record_delete),
        )
        .route("/api/credits/import", post(credits_import))
        .route("/api/credits/export", post(credits_export))
        .route("/api/update/check", get(update_check_api))
        .route("/api/pairing/create", post(pairing_create))
        .route("/api/pairing/request", post(pairing_request))
        .route("/api/pairing/confirm", post(pairing_confirm))
        .route("/assets/hilia-logo.png", get(logo_asset))
        .route("/assets/hilia-wallpaper.png", get(wallpaper_asset))
        .route("/assets/provider-icons.svg", get(provider_icons_asset))
        .route("/assets/qrcode.min.js", get(qrcode_js_asset))
        .route("/api/health", get(health))
        .route("/api/auth/login", post(auth_login))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/config", get(get_config).put(update_config))
        .route("/api/events", get(events))
        .route("/api/assets/{asset_id}", get(image_asset))
        .route("/api/turns", post(create_turn))
        .route("/api/queue", post(queue_prompt))
        .route("/api/queue/{prompt_id}", delete(remove_queue_prompt))
        .route("/api/runs/{run_id}/cancel", post(cancel_run))
        .route("/api/questions/{question_id}/answer", post(answer_question))
        .route("/api/models/active", put(set_models))
        .route("/api/conversation/reset", post(reset_conversation))
        .route("/api/alarms", get(list_alarms_web))
        .route("/api/state", get(session_state))
        .route("/api/alarms/{alarm_id}", delete(cancel_alarm_web))
        // APK 对接：直连配对确认 + 中继结构化消息统一入口（本机/已认证）
        .route("/api/pairing/confirm_direct", post(pairing_confirm_direct))
        .route("/api/mobile/dispatch", post(mobile_dispatch))
        .layer(DefaultBodyLimit::max(JSON_BODY_LIMIT));
    // 无密码时：非回环来源仅允许 /pair/ws（APK 直连配对与消息），面板 API 一律 403
    let guarded = app.layer(middleware::from_fn_with_state(state.clone(), lan_guard));
    Router::new()
        .route("/pair/ws", get(pair_ws))
        .merge(guarded)
        .with_state(state)
}

async fn index_asset() -> Response {
    text_asset(INDEX_HTML, "text/html; charset=utf-8")
}

async fn styles_asset() -> Response {
    text_asset(STYLES_CSS, "text/css; charset=utf-8")
}

async fn app_asset() -> Response {
    text_asset(APP_JS, "application/javascript; charset=utf-8")
}

async fn mini_asset() -> Response {
    text_asset(MINI_HTML, "text/html; charset=utf-8")
}

async fn mini_css_asset() -> Response {
    text_asset(MINI_CSS, "text/css; charset=utf-8")
}

async fn mini_js_asset() -> Response {
    text_asset(MINI_JS, "application/javascript; charset=utf-8")
}

async fn logo_asset() -> Response {
    binary_asset(HILIA_LOGO, "image/png")
}

async fn wallpaper_asset() -> Response {
    binary_asset(HILIA_WALLPAPER, "image/png")
}

async fn provider_icons_asset() -> Response {
    text_asset(PROVIDER_ICONS, "image/svg+xml; charset=utf-8")
}

async fn qrcode_js_asset() -> Response {
    text_asset(QRCODE_JS, "application/javascript; charset=utf-8")
}

fn text_asset(content: &'static str, content_type: &'static str) -> Response {
    asset_response(content.as_bytes(), content_type)
}

fn binary_asset(content: &'static [u8], content_type: &'static str) -> Response {
    asset_response(content, content_type)
}

fn asset_response(content: &'static [u8], content_type: &'static str) -> Response {
    let mut response = content.into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response.headers_mut().insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self'; style-src 'self'; script-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
    );
    response
        .headers_mut()
        .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}

async fn auth_login(
    State(state): State<WebState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> std::result::Result<Response, ApiError> {
    if !origin_is_allowed(&headers) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "request origin is not allowed",
        ));
    }
    if !state.auth.required() {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    if request.password.chars().count() > 1_024 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "password is too long",
        ));
    }
    let session = match state.auth.login(peer.ip(), &request.password) {
        Ok(session) => session,
        Err(LoginFailure::Invalid) => {
            return Err(ApiError::new(StatusCode::UNAUTHORIZED, "invalid password"));
        }
        Err(LoginFailure::RateLimited) => {
            let mut response = ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too many login attempts; try again shortly",
            )
            .into_response();
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from_static("60"));
            return Ok(response);
        }
    };
    let cookie =
        format!("{AUTH_COOKIE}={session}; HttpOnly; SameSite=Strict; Path=/; Max-Age=86400");
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(ApiError::internal)?,
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn resolve_web_password(args: &WebArgs) -> Result<Option<String>> {
    let password = if let Some(path) = &args.password_file {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("reading WebUI password file: {}", path.display()))?;
        Some(contents.trim_end_matches(['\r', '\n']).to_string())
    } else {
        match &args.password {
            Some(password) if !password.is_empty() => Some(password.clone()),
            Some(_) if io::stdin().is_terminal() => {
                Some(rpassword::prompt_password("WebUI password: ")?)
            }
            Some(_) => {
                anyhow::bail!("-p requires an interactive terminal or an explicit password value")
            }
            None => None,
        }
    };
    if let Some(password) = &password {
        if password.is_empty() {
            anyhow::bail!("WebUI password cannot be empty");
        }
        if password.chars().count() > 1_024 {
            anyhow::bail!("WebUI password cannot exceed 1,024 characters");
        }
    }
    Ok(password)
}

fn web_access_urls(port: u16, include_lan: bool) -> Vec<String> {
    let mut addresses = BTreeSet::new();
    addresses.insert(Ipv4Addr::LOCALHOST);
    if include_lan {
        if let Ok(interfaces) = if_addrs::get_if_addrs() {
            for interface in interfaces {
                if let if_addrs::IfAddr::V4(address) = interface.addr {
                    if !address.ip.is_unspecified() {
                        addresses.insert(address.ip);
                    }
                }
            }
        }
    }
    addresses
        .into_iter()
        .map(|address| format!("http://{address}:{port}"))
        .collect()
}

async fn health() -> Json<Value> {
    Json(json!({
        "status": "ready",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// 会话状态（供终端/面板同步轮询）：当前会话最大 seq + 是否有运行中的轮次。
/// 终端 `hilia` 与面板共享同一 conversation.db；前端轮询此接口，
/// 发现 seq 变化即重载历史，实现双端同步。
async fn session_state(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let entries = state.state_store.load_conversation().map_err(ApiError::internal)?;
    let last_seq = entries.len() as i64;
    let running = state
        .state_store
        .has_running_turns()
        .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "ok": true,
        "last_seq": last_seq,
        "running": running,
    }))
    .into_response())
}

/// 取消定时任务（面板「取消」按钮）。
async fn cancel_alarm_web(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(alarm_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let cancelled = crate::alarm::cancel(&state.paths, &alarm_id).map_err(ApiError::internal)?;
    if !cancelled {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "alarm not found"));
    }
    Ok(Json(json!({ "ok": true, "id": alarm_id })).into_response())
}

/// 定时任务（闹钟/番茄钟）列表，供面板可视化。
async fn list_alarms_web(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let records = crate::alarm::cleanup_dead(&state.paths).map_err(ApiError::internal)?;
    let alarms = records
        .into_iter()
        .map(|record| {
            json!({
                "id": record.id,
                "label": record.label,
                "time": record.time,
                "due_at": record.due_at,
                "due_at_local": crate::alarm::format_due_at(record.due_at),
                "repeat_seconds": record.repeat_seconds,
                "status": match record.status {
                    crate::alarm::AlarmStatus::Scheduled => "scheduled",
                    crate::alarm::AlarmStatus::Ringing => "ringing",
                },
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "ok": true, "alarms": alarms })).into_response())
}

async fn bootstrap(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    state
        .state_store
        .recover_stale_turns()
        .map_err(ApiError::internal)?;
    let (config, active_run_id, context) = {
        let manager = state.manager.lock().unwrap();
        (
            manager.config.clone(),
            manager.active_run_id.clone(),
            manager.context,
        )
    };
    let running_target = state
        .state_store
        .running_turn_queue_target()
        .map_err(ApiError::internal)?;
    let external_target = active_run_id
        .is_none()
        .then_some(running_target.as_ref())
        .flatten();
    let mut assets_by_turn = HashMap::<String, Vec<ImageAsset>>::new();
    for asset in state
        .state_store
        .load_image_assets()
        .map_err(ApiError::internal)?
    {
        assets_by_turn
            .entry(asset.turn_id.clone())
            .or_default()
            .push(asset);
    }
    let turns = state
        .state_store
        .load_turns()
        .map_err(ApiError::internal)?
        .into_iter()
        .filter(|turn| !turn.is_summary)
        .map(|turn| {
            let assets = assets_by_turn.remove(&turn.turn_id).unwrap_or_default();
            SafeTurn::from_turn(turn, assets)
        })
        .collect();
    let usage = state
        .state_store
        .usage_snapshot()
        .map_err(ApiError::internal)?
        .into();
    let queued_prompts = match external_target {
        Some(target) => state
            .state_store
            .load_queued_prompts_for_target(target)
            .map_err(ApiError::internal)?,
        None => state
            .state_store
            .load_queued_prompts()
            .map_err(ApiError::internal)?,
    }
    .into_iter()
    .map(SafeQueuedPrompt::from)
    .collect();
    let running_turn_id = running_target.as_ref().map(|target| target.turn_id.clone());
    let external_queue_available = external_target
        .is_some_and(|target| target.queue_session_id.is_some() && target.owner_pid.is_some());
    let mut response = Json(BootstrapResponse {
        version: env!("CARGO_PKG_VERSION"),
        boot_id: state.boot_id.to_string(),
        latest_event_id: state.events.latest_id(),
        active_run_id,
        running_turn_id,
        external_queue_available,
        turns,
        queued_prompts,
        models: safe_models(&config),
        display: web_display_config(&config),
        context,
        usage,
        capabilities: Capabilities {
            multi_conversation: false,
            attachments: false,
            queue: true,
        },
    })
    .into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn get_config(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    let (config, context) = {
        let manager = state.manager.lock().unwrap();
        (manager.config.clone(), manager.context)
    };
    let mut response = Json(config_response(&config, context, &state.paths)?).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn update_config(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<UpdateConfigRequest>,
) -> std::result::Result<Json<ConfigResponse>, ApiError> {
    require_mutation(&headers, &state)?;
    require_no_running_turn(&state.state_store)?;

    let current = state.manager.lock().unwrap().config.clone();
    let current_prompts =
        read_prompt_documents(&current, &state.paths).map_err(ApiError::internal)?;
    let mut candidate: AppConfig = serde_json::from_value(request.config).map_err(|error| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid configuration: {}", safe_error_message(error)),
        )
    })?;
    restore_config_secrets(&mut candidate, &current, &request.secrets)?;
    validate_config_candidate(&candidate)?;
    validate_prompt_documents(&candidate, &request.prompts)?;
    let prompt_changed = prompt_configuration_changed(&current, &candidate)
        || prompt_documents_changed(&current_prompts, &request.prompts);
    if prompt_changed && !request.reset_conversation {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "prompt changes require explicit confirmation to reset the conversation",
        ));
    }

    reserve_admin(&state.manager)?;
    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::ApplyConfig {
            config: candidate,
            prompts: request.prompts,
            reset_conversation: prompt_changed,
            reply,
        })
        .is_err()
    {
        release_admin(&state.manager);
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent worker is unavailable",
        ));
    }
    match receiver.await {
        Ok(Ok(())) => {}
        Ok(Err(AdminFailure::Invalid(message))) => {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, message));
        }
        Ok(Err(AdminFailure::Internal(message))) => {
            tracing::error!(error = %message, "WebUI configuration update failed");
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                safe_error_message(&message),
            ));
        }
        Err(_) => {
            release_admin(&state.manager);
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "agent worker stopped before updating the configuration",
            ));
        }
    }
    let manager = state.manager.lock().unwrap();
    Ok(Json(config_response(
        &manager.config,
        manager.context,
        &state.paths,
    )?))
}

async fn image_asset(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(asset_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_auth(&headers, &state)?;
    if asset_id.len() > 96
        || asset_id.is_empty()
        || !asset_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "image asset not found",
        ));
    }
    let Some(asset) = state
        .state_store
        .load_image_asset(&asset_id)
        .map_err(ApiError::internal)?
    else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "image asset not found",
        ));
    };
    let mut response = asset.bytes.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_str(&asset.asset.mime).map_err(ApiError::internal)?,
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400"),
    );
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}

async fn events(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> std::result::Result<Sse<impl Stream<Item = std::result::Result<Event, Infallible>>>, ApiError>
{
    require_auth(&headers, &state)?;
    let header_after = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let after = query.after.max(header_after);
    let subscription = state.events.subscribe_after(after);
    let stream_state = SseStreamState {
        pending: subscription.pending,
        receiver: subscription.receiver,
        events: state.events,
        last_id: after,
    };
    let events = stream::unfold(stream_state, |mut state| async move {
        loop {
            if let Some(record) = state.pending.pop_front() {
                if record.kind == "resync_required" {
                    state.last_id = record.id;
                    return Some((Ok(record_to_sse(record)), state));
                }
                if record.id <= state.last_id {
                    continue;
                }
                state.last_id = record.id;
                return Some((Ok(record_to_sse(record)), state));
            }
            match state.receiver.recv().await {
                Ok(record) if record.id > state.last_id => {
                    state.last_id = record.id;
                    return Some((Ok(record_to_sse(record)), state));
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    state.pending = state.events.replay_after(state.last_id);
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    let ready =
        stream::once(async { Ok::<Event, Infallible>(Event::default().comment("connected")) });
    let stream = ready.chain(events);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

struct SseStreamState {
    pending: VecDeque<EventRecord>,
    receiver: broadcast::Receiver<EventRecord>,
    events: EventHub,
    last_id: u64,
}

fn record_to_sse(record: EventRecord) -> Event {
    Event::default()
        .id(record.id.to_string())
        .event(record.kind)
        .data(record.data)
}

fn enqueue_running_prompt(
    state: &WebState,
    content: &str,
) -> std::result::Result<(Option<String>, Option<String>, SafeQueuedPrompt), ApiError> {
    let active_run_id = {
        let manager = state.manager.lock().unwrap();
        if manager.admin_busy {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "希尔娅 is busy with another operation",
            ));
        }
        manager.active_run_id.clone()
    };
    let prompt_id = random_id("queued", 18);
    if let Some(run_id) = active_run_id {
        let prompt = state
            .state_store
            .enqueue_prompt(&prompt_id, content, content, &[])
            .map_err(ApiError::internal)?;
        return Ok((Some(run_id), None, SafeQueuedPrompt::from(prompt)));
    }

    state
        .state_store
        .recover_stale_turns()
        .map_err(ApiError::internal)?;
    let target = state
        .state_store
        .running_turn_queue_target()
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "there is no active reply to follow up",
            )
        })?;
    if target.queue_session_id.is_none() || target.owner_pid.is_none() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "the running turn cannot accept messages from this WebUI",
        ));
    }
    let prompt = state
        .state_store
        .enqueue_prompt_for_target(&target, &prompt_id, content, content, &[])
        .map_err(ApiError::internal)?;
    Ok((None, Some(target.turn_id), SafeQueuedPrompt::from(prompt)))
}

fn publish_queued_prompt(
    state: &WebState,
    run_id: Option<&str>,
    turn_id: Option<&str>,
    prompt: &SafeQueuedPrompt,
) {
    state.events.publish(
        "queue.added",
        json!({ "run_id": run_id, "turn_id": turn_id, "prompt": prompt }),
    );
}

async fn create_turn(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<CreateTurnRequest>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    let content = validate_content(request.content)?;
    let mode = parse_mode(&request.mode)?;
    state
        .state_store
        .recover_stale_turns()
        .map_err(ApiError::internal)?;
    if state
        .state_store
        .has_running_turns()
        .map_err(ApiError::internal)?
    {
        let (run_id, turn_id, prompt) = enqueue_running_prompt(&state, &content)?;
        publish_queued_prompt(&state, run_id.as_deref(), turn_id.as_deref(), &prompt);
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({
                "queued": true,
                "prompt": prompt,
                "run_id": run_id,
                "running_turn_id": turn_id,
            })),
        )
            .into_response());
    }
    let run_id = random_id("run", 18);
    {
        let mut manager = state.manager.lock().unwrap();
        if manager.active_run_id.is_some() || manager.admin_busy {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "希尔娅 is busy with another operation",
            ));
        }
        manager.active_run_id = Some(run_id.clone());
    }
    if state
        .actor_tx
        .send(ActorCommand::StartTurn {
            run_id: run_id.clone(),
            content,
            mode,
        })
        .is_err()
    {
        finish_run(&state.manager, &run_id, None);
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent worker is unavailable",
        ));
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "run_id": run_id }))).into_response())
}

async fn queue_prompt(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<QueuePromptRequest>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    let content = validate_content(request.content)?;
    let (run_id, turn_id, safe) = enqueue_running_prompt(&state, &content)?;
    publish_queued_prompt(&state, run_id.as_deref(), turn_id.as_deref(), &safe);
    Ok((StatusCode::ACCEPTED, Json(safe)).into_response())
}

async fn remove_queue_prompt(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(prompt_id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    require_mutation(&headers, &state)?;
    if prompt_id.len() > 96
        || prompt_id.is_empty()
        || !prompt_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "queued prompt not found",
        ));
    }
    let run_id = state.manager.lock().unwrap().active_run_id.clone();
    let target = if run_id.is_none() {
        state
            .state_store
            .running_turn_queue_target()
            .map_err(ApiError::internal)?
    } else {
        None
    };
    let removed = match target.as_ref() {
        Some(target) => state
            .state_store
            .remove_queued_prompt_for_target(target, &prompt_id)
            .map_err(ApiError::internal)?,
        None => state
            .state_store
            .remove_queued_prompt(&prompt_id)
            .map_err(ApiError::internal)?,
    };
    if !removed {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "queued prompt not found",
        ));
    }
    state.events.publish(
        "queue.removed",
        json!({
            "run_id": run_id,
            "turn_id": target.as_ref().map(|target| target.turn_id.as_str()),
            "prompt_id": prompt_id,
        }),
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn cancel_run(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    let matches_active =
        state.manager.lock().unwrap().active_run_id.as_deref() == Some(run_id.as_str());
    if !matches_active {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "active run not found"));
    }
    state
        .actor_tx
        .send(ActorCommand::Cancel {
            run_id: run_id.clone(),
        })
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "agent worker is unavailable",
            )
        })?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "run_id": run_id,
            "cancellation_requested": true,
        })),
    )
        .into_response())
}

async fn answer_question(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(question_id): Path<String>,
    Json(request): Json<AnswerQuestionRequest>,
) -> std::result::Result<StatusCode, ApiError> {
    require_mutation(&headers, &state)?;
    match state
        .questions
        .answer(&question_id, request.answers, |run_id, answers| {
            state.events.publish(
                "question.answered",
                json!({
                    "run_id": run_id,
                    "question_id": question_id,
                    "answers": answers,
                }),
            );
        }) {
        Ok(()) => {}
        Err(AnswerFailure::NotFound) => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "pending question not found",
            ));
        }
        Err(AnswerFailure::Invalid(message)) => {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, message));
        }
        Err(AnswerFailure::Gone) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "the question is no longer awaiting an answer",
            ));
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn set_models(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(request): Json<SetModelsRequest>,
) -> std::result::Result<Json<ModelResponse>, ApiError> {
    require_mutation(&headers, &state)?;
    let models = validate_model_selection(request.models)?;
    require_no_running_turn(&state.state_store)?;
    reserve_admin(&state.manager)?;
    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::SetModels { models, reply })
        .is_err()
    {
        release_admin(&state.manager);
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent worker is unavailable",
        ));
    }
    match receiver.await {
        Ok(Ok(())) => {}
        Ok(Err(AdminFailure::Invalid(message))) => {
            return Err(ApiError::new(StatusCode::BAD_REQUEST, message));
        }
        Ok(Err(AdminFailure::Internal(message))) => {
            tracing::error!(error = %message, "WebUI model update failed");
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                safe_error_message(&message),
            ));
        }
        Err(_) => {
            release_admin(&state.manager);
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "agent worker stopped before updating the model",
            ));
        }
    }
    let manager = state.manager.lock().unwrap();
    Ok(Json(ModelResponse {
        models: safe_models(&manager.config),
        display: web_display_config(&manager.config),
        context: manager.context,
    }))
}

async fn reset_conversation(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> std::result::Result<StatusCode, ApiError> {
    require_mutation(&headers, &state)?;
    require_no_running_turn(&state.state_store)?;
    reserve_admin(&state.manager)?;
    let (reply, receiver) = oneshot::channel();
    if state
        .actor_tx
        .send(ActorCommand::ResetConversation { reply })
        .is_err()
    {
        release_admin(&state.manager);
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "agent worker is unavailable",
        ));
    }
    match receiver.await {
        Ok(Ok(())) => Ok(StatusCode::NO_CONTENT),
        Ok(Err(AdminFailure::Invalid(message))) => {
            Err(ApiError::new(StatusCode::CONFLICT, message))
        }
        Ok(Err(AdminFailure::Internal(message))) => {
            tracing::error!(error = %message, "WebUI conversation reset failed");
            Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                safe_error_message(&message),
            ))
        }
        Err(_) => {
            release_admin(&state.manager);
            Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "agent worker stopped before resetting the conversation",
            ))
        }
    }
}

/// 配置文件热重载：每 2 秒检查 config.jsonc 的修改时间，
/// 变化时重新加载配置并重建 agent（不重置会话），同时发布事件让前端刷新。
fn spawn_config_watcher(paths: GqyPaths, actor_tx: mpsc::UnboundedSender<ActorCommand>, events: EventHub) {
    let mut last_mtime = std::fs::metadata(&paths.config_file)
        .and_then(|meta| meta.modified())
        .ok();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            interval.tick().await;
            let mtime = std::fs::metadata(&paths.config_file)
                .and_then(|meta| meta.modified())
                .ok();
            if mtime.is_none() {
                continue;
            }
            if mtime == last_mtime {
                continue;
            }
            let Some(mtime) = mtime else { continue };
            last_mtime = Some(mtime);
            let Ok(config) = AppConfig::load(&paths) else {
                tracing::warn!("config watcher: failed to reload configuration");
                continue;
            };
            let Ok(prompts) = read_prompt_documents(&config, &paths) else {
                continue;
            };
            let (reply, _receiver) = tokio::sync::oneshot::channel();
            if actor_tx
                .send(ActorCommand::ApplyConfig {
                    config,
                    prompts,
                    reset_conversation: false,
                    reply,
                })
                .is_ok()
            {
                tracing::info!("config watcher: configuration reloaded from file");
                events.publish("config.reloaded", serde_json::json!({}));
            }
        }
    });
}

fn spawn_actor(
    agent: Agent,
    config: AppConfig,
    paths: GqyPaths,
    state_store: StateStore,
    manager: Arc<Mutex<ManagerState>>,
    events: EventHub,
    questions: QuestionBroker,
) -> Result<(mpsc::UnboundedSender<ActorCommand>, JoinHandle<Result<()>>)> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let join = std::thread::Builder::new()
        .name("hilia-web-agent".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("building WebUI agent runtime")?;
            runtime.block_on(actor_loop(
                agent,
                config,
                paths,
                state_store,
                manager,
                events,
                questions,
                receiver,
            ));
            Ok(())
        })
        .context("starting WebUI agent thread")?;
    Ok((sender, join))
}

#[allow(clippy::too_many_arguments)]
async fn actor_loop(
    mut agent: Agent,
    mut config: AppConfig,
    paths: GqyPaths,
    state_store: StateStore,
    manager: Arc<Mutex<ManagerState>>,
    events: EventHub,
    questions: QuestionBroker,
    mut receiver: mpsc::UnboundedReceiver<ActorCommand>,
) {
    while let Some(command) = receiver.recv().await {
        match command {
            ActorCommand::StartTurn {
                run_id,
                content,
                mode,
            } => {
                let keep_running = run_agent_turn(
                    &mut agent,
                    &config,
                    &paths,
                    &state_store,
                    &manager,
                    &events,
                    &questions,
                    &mut receiver,
                    run_id,
                    content,
                    mode,
                )
                .await;
                if !keep_running {
                    break;
                }
            }
            ActorCommand::Cancel { .. } => {}
            ActorCommand::SetModels { models, reply } => {
                let result = rebuild_for_models(
                    &mut agent,
                    &mut config,
                    &paths,
                    &state_store,
                    &manager,
                    &models,
                );
                release_admin(&manager);
                let _ = reply.send(result);
            }
            ActorCommand::ApplyConfig {
                config: next_config,
                prompts,
                reset_conversation,
                reply,
            } => {
                let result = rebuild_for_config(
                    &mut agent,
                    &mut config,
                    &paths,
                    &state_store,
                    &manager,
                    &events,
                    next_config,
                    &prompts,
                    reset_conversation,
                );
                release_admin(&manager);
                let _ = reply.send(result);
            }
            ActorCommand::ResetConversation { reply } => {
                let result = reset_actor_conversation(
                    &mut agent,
                    &config,
                    &paths,
                    &state_store,
                    &manager,
                    &events,
                );
                release_admin(&manager);
                let _ = reply.send(result);
            }
            ActorCommand::Shutdown => break,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_agent_turn(
    agent: &mut Agent,
    config: &AppConfig,
    paths: &GqyPaths,
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
    questions: &QuestionBroker,
    receiver: &mut mpsc::UnboundedReceiver<ActorCommand>,
    run_id: String,
    content: String,
    mode: AgentMode,
) -> bool {
    events.publish(
        "run.started",
        json!({ "run_id": run_id, "mode": mode_name(mode) }),
    );
    let setup = (|| -> Result<AgentTurnControl> {
        let normal_tools = build_tool_registry(config, paths, AgentMode::Normal, true)?;
        let plan_tools = build_tool_registry(config, paths, AgentMode::Plan, true)?;
        let chat_tools = build_tool_registry(config, paths, AgentMode::Chat, true)?;
        let active_tools = match mode {
            AgentMode::Normal => normal_tools.clone(),
            AgentMode::Plan => plan_tools.clone(),
            AgentMode::Chat => chat_tools.clone(),
        };
        agent.switch_mode(mode, active_tools);
        agent.prepare_for_turn()?;
        Ok(AgentTurnControl::new(
            mode,
            normal_tools,
            plan_tools,
            chat_tools,
        ))
    })();
    let control = match setup {
        Ok(control) => control,
        Err(error) => {
            finish_failed_run(manager, events, questions, agent, &run_id, &error);
            return true;
        }
    };

    let mapper = Arc::new(Mutex::new(RunEventMapper::new(
        run_id.clone(),
        events.clone(),
        questions.clone(),
        state_store.clone(),
    )));
    let chat_outcome = {
        let callback_mapper = mapper.clone();
        let chat = agent.chat_stream_with_control(&content, &[], &control, move |event| {
            callback_mapper.lock().unwrap().handle(event);
            Ok(())
        });
        tokio::pin!(chat);
        loop {
            tokio::select! {
                biased;
                result = &mut chat => break TurnOutcome::Finished(result),
                command = receiver.recv() => {
                    match active_directive(command, &run_id, manager) {
                        ActiveDirective::Continue => {}
                        ActiveDirective::Cancel => {
                            questions.cancel_run(&run_id);
                            break TurnOutcome::Cancelled;
                        }
                        ActiveDirective::Shutdown => {
                            questions.cancel_run(&run_id);
                            break TurnOutcome::Shutdown;
                        }
                    }
                }
            }
        }
    };

    let result = match chat_outcome {
        TurnOutcome::Cancelled => {
            finish_cancelled_run(manager, events, agent, &run_id);
            return true;
        }
        TurnOutcome::Shutdown => {
            finish_cancelled_run(manager, events, agent, &run_id);
            return false;
        }
        TurnOutcome::Finished(Err(error)) if question::is_question_cancelled(&error) => {
            questions.cancel_run(&run_id);
            finish_cancelled_run(manager, events, agent, &run_id);
            return true;
        }
        TurnOutcome::Finished(Err(error)) => {
            finish_failed_run(manager, events, questions, agent, &run_id, &error);
            return true;
        }
        TurnOutcome::Finished(Ok(result)) => result,
    };

    questions.cancel_run(&run_id);
    let context_tokens = match agent.effective_context_tokens() {
        Ok(tokens) => tokens,
        Err(error) => {
            finish_completed_with_context_error(manager, events, agent, &run_id, &result, &error);
            return true;
        }
    };
    let overflow_outcome = {
        let callback_mapper = mapper;
        let overflow = agent.handle_overflow_after_turn(context_tokens, move |event| {
            callback_mapper.lock().unwrap().handle(event);
            Ok(())
        });
        tokio::pin!(overflow);
        loop {
            tokio::select! {
                biased;
                result = &mut overflow => break OverflowOutcome::Finished(result),
                command = receiver.recv() => {
                    match active_directive(command, &run_id, manager) {
                        ActiveDirective::Continue => {}
                        ActiveDirective::Cancel => break OverflowOutcome::Cancelled,
                        ActiveDirective::Shutdown => break OverflowOutcome::Shutdown,
                    }
                }
            }
        }
    };
    match overflow_outcome {
        OverflowOutcome::Cancelled => {
            let context =
                current_context(agent).unwrap_or_else(|_| manager.lock().unwrap().context);
            finish_run(manager, &run_id, Some(context));
            publish_completed(events, &run_id, &result, context);
            return true;
        }
        OverflowOutcome::Shutdown => {
            let context =
                current_context(agent).unwrap_or_else(|_| manager.lock().unwrap().context);
            finish_run(manager, &run_id, Some(context));
            publish_completed(events, &run_id, &result, context);
            return false;
        }
        OverflowOutcome::Finished(Err(error)) => {
            finish_completed_with_context_error(manager, events, agent, &run_id, &result, &error);
            return true;
        }
        OverflowOutcome::Finished(Ok(_)) => {}
    }
    let context = match current_context(agent) {
        Ok(context) => context,
        Err(error) => {
            finish_completed_with_context_error(manager, events, agent, &run_id, &result, &error);
            return true;
        }
    };
    finish_run(manager, &run_id, Some(context));
    publish_completed(events, &run_id, &result, context);
    true
}

enum TurnOutcome {
    Finished(Result<ChatResult>),
    Cancelled,
    Shutdown,
}

enum OverflowOutcome {
    Finished(Result<Option<ChatResult>>),
    Cancelled,
    Shutdown,
}

enum ActiveDirective {
    Continue,
    Cancel,
    Shutdown,
}

fn active_directive(
    command: Option<ActorCommand>,
    run_id: &str,
    manager: &Arc<Mutex<ManagerState>>,
) -> ActiveDirective {
    match command {
        Some(ActorCommand::Cancel { run_id: requested }) if requested == run_id => {
            ActiveDirective::Cancel
        }
        Some(ActorCommand::Cancel { .. }) => ActiveDirective::Continue,
        Some(ActorCommand::Shutdown) | None => ActiveDirective::Shutdown,
        Some(ActorCommand::SetModels { reply, .. }) => {
            release_admin(manager);
            let _ = reply.send(Err(AdminFailure::Invalid(
                "the model cannot be changed while a turn is running".to_string(),
            )));
            ActiveDirective::Continue
        }
        Some(ActorCommand::ApplyConfig { reply, .. }) => {
            release_admin(manager);
            let _ = reply.send(Err(AdminFailure::Invalid(
                "the configuration cannot be changed while a turn is running".to_string(),
            )));
            ActiveDirective::Continue
        }
        Some(ActorCommand::ResetConversation { reply }) => {
            release_admin(manager);
            let _ = reply.send(Err(AdminFailure::Invalid(
                "the conversation cannot be reset while a turn is running".to_string(),
            )));
            ActiveDirective::Continue
        }
        Some(ActorCommand::StartTurn {
            run_id: rejected, ..
        }) => {
            finish_run(manager, &rejected, None);
            ActiveDirective::Continue
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rebuild_for_models(
    agent: &mut Agent,
    config: &mut AppConfig,
    paths: &GqyPaths,
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    models: &[ActiveProviderModelConfig],
) -> std::result::Result<(), AdminFailure> {
    let mut next_config = config.clone();
    next_config
        .set_active_provider_models(models)
        .map_err(|error| AdminFailure::Invalid(safe_error_message(&error)))?;
    if next_config.active_provider_models == config.active_provider_models {
        return Ok(());
    }
    let client = OpenAiCompatibleClient::from_config(&next_config, paths)
        .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
    let registry = build_tool_registry(&next_config, paths, AgentMode::Normal, true)
        .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
    let next_agent = Agent::new(
        next_config.clone(),
        paths,
        state_store.clone(),
        client,
        registry,
        AgentMode::Normal,
    )
    .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
    let context = current_context(&next_agent)
        .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
    next_config
        .save(paths)
        .map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
    *agent = next_agent;
    *config = next_config.clone();
    let mut manager = manager.lock().unwrap();
    manager.config = next_config;
    manager.context = context;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn rebuild_for_config(
    agent: &mut Agent,
    config: &mut AppConfig,
    paths: &GqyPaths,
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
    next_config: AppConfig,
    prompts: &PromptDocuments,
    reset_conversation: bool,
) -> std::result::Result<(), AdminFailure> {
    let previous_prompts = read_prompt_documents(config, paths)
        .map_err(|error| AdminFailure::Internal(safe_error_message(error)))?;
    let prompt_backups =
        apply_prompt_documents(config, &next_config, &previous_prompts, prompts, paths)
            .map_err(|error| AdminFailure::Internal(safe_error_message(error)))?;
    let scope_backups = match apply_persona_scope_changes(
        config,
        &next_config,
        &previous_prompts,
        prompts,
        paths,
    ) {
        Ok(backups) => backups,
        Err(error) => {
            restore_file_backups(&prompt_backups);
            return Err(AdminFailure::Internal(safe_error_message(error)));
        }
    };
    let config_backup = FileBackup {
        path: paths.config_file.clone(),
        content: std::fs::read(&paths.config_file).ok(),
    };
    let system_prompt_backup = next_config.system_prompt.as_ref().map(|_| FileBackup {
        path: next_config.system_prompt_path(paths),
        content: std::fs::read(next_config.system_prompt_path(paths)).ok(),
    });

    let build_agent = || -> Result<Agent> {
        let client = OpenAiCompatibleClient::from_config(&next_config, paths)?;
        let registry = build_tool_registry(&next_config, paths, AgentMode::Normal, true)?;
        Agent::new(
            next_config.clone(),
            paths,
            state_store.clone(),
            client,
            registry,
            AgentMode::Normal,
        )
    };
    let mut next_agent = match build_agent() {
        Ok(agent) => agent,
        Err(error) => {
            restore_file_backups(&prompt_backups);
            restore_persona_scope_backups(&scope_backups);
            return Err(AdminFailure::Invalid(safe_error_message(error)));
        }
    };
    let mut context = match current_context(&next_agent) {
        Ok(context) => context,
        Err(error) => {
            restore_file_backups(&prompt_backups);
            restore_persona_scope_backups(&scope_backups);
            return Err(AdminFailure::Invalid(safe_error_message(error)));
        }
    };
    if let Err(error) = next_config.save(paths) {
        restore_file_backups(&prompt_backups);
        restore_persona_scope_backups(&scope_backups);
        restore_file_backups(std::slice::from_ref(&config_backup));
        if let Some(backup) = &system_prompt_backup {
            restore_file_backups(std::slice::from_ref(backup));
        }
        return Err(AdminFailure::Internal(safe_error_message(error)));
    }

    if reset_conversation {
        let reset = (|| -> Result<()> {
            state_store.reset_conversation()?;
            let memory = MemoryStore::new(&next_config, paths);
            memory.clear_evicted_context()?;
            memory.clear_pending_events()?;
            next_agent.reset_memory()?;
            next_agent.prepare_for_turn()?;
            context = current_context(&next_agent)?;
            Ok(())
        })();
        if let Err(error) = reset {
            restore_file_backups(&prompt_backups);
            restore_persona_scope_backups(&scope_backups);
            restore_file_backups(std::slice::from_ref(&config_backup));
            if let Some(backup) = &system_prompt_backup {
                restore_file_backups(std::slice::from_ref(backup));
            }
            return Err(AdminFailure::Internal(safe_error_message(error)));
        }
    }

    *agent = next_agent;
    *config = next_config.clone();
    let mut manager = manager.lock().unwrap();
    manager.config = next_config;
    manager.context = context;
    drop(manager);
    if reset_conversation {
        events.publish("conversation.reset", json!({}));
    }
    finalize_persona_scope_backups(&scope_backups);
    Ok(())
}

fn reset_actor_conversation(
    agent: &mut Agent,
    config: &AppConfig,
    paths: &GqyPaths,
    state_store: &StateStore,
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
) -> std::result::Result<(), AdminFailure> {
    let mut reset = || -> Result<ContextSnapshot> {
        state_store.reset_conversation()?;
        let memory = MemoryStore::new(config, paths);
        memory.clear_evicted_context()?;
        memory.clear_pending_events()?;
        agent.reset_memory()?;
        agent.prepare_for_turn()?;
        current_context(agent)
    };
    let context = reset().map_err(|error| AdminFailure::Internal(safe_error_message(&error)))?;
    manager.lock().unwrap().context = context;
    events.publish("conversation.reset", json!({}));
    Ok(())
}

fn finish_cancelled_run(
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
    agent: &Agent,
    run_id: &str,
) {
    let context = current_context(agent).ok();
    finish_run(manager, run_id, context);
    events.publish("run.cancelled", json!({ "run_id": run_id }));
}

fn finish_failed_run(
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
    questions: &QuestionBroker,
    agent: &Agent,
    run_id: &str,
    error: &anyhow::Error,
) {
    questions.cancel_run(run_id);
    let context = current_context(agent).ok();
    finish_run(manager, run_id, context);
    let message = safe_error_message(error);
    tracing::error!(run_id, error = %error, "WebUI agent run failed");
    events.publish(
        "run.failed",
        json!({ "run_id": run_id, "message": message }),
    );
}

fn finish_completed_with_context_error(
    manager: &Arc<Mutex<ManagerState>>,
    events: &EventHub,
    agent: &Agent,
    run_id: &str,
    result: &ChatResult,
    error: &anyhow::Error,
) {
    let message = safe_error_message(error);
    tracing::error!(run_id, error = %error, "WebUI post-turn context maintenance failed");
    events.publish(
        "context.error",
        json!({ "run_id": run_id, "message": message }),
    );
    let context = current_context(agent).unwrap_or_else(|_| manager.lock().unwrap().context);
    finish_run(manager, run_id, Some(context));
    publish_completed(events, run_id, result, context);
}

fn finish_run(manager: &Arc<Mutex<ManagerState>>, run_id: &str, context: Option<ContextSnapshot>) {
    let mut manager = manager.lock().unwrap();
    if let Some(context) = context {
        manager.context = context;
    }
    if manager.active_run_id.as_deref() == Some(run_id) {
        manager.active_run_id = None;
    }
}

fn publish_completed(
    events: &EventHub,
    run_id: &str,
    result: &ChatResult,
    context: ContextSnapshot,
) {
    events.publish(
        "run.completed",
        json!({
            "run_id": run_id,
            "usage": result.usage,
            "usage_estimated": result.usage_estimated,
            "provider_id": result.provider_id,
            "model": result.model,
            "context_tokens": context.tokens,
            "context_window": context.window,
        }),
    );
}

fn current_context(agent: &Agent) -> Result<ContextSnapshot> {
    Ok(ContextSnapshot {
        tokens: agent.effective_context_tokens()?,
        window: agent.context_window(),
    })
}

fn reserve_admin(manager: &Arc<Mutex<ManagerState>>) -> std::result::Result<(), ApiError> {
    let mut manager = manager.lock().unwrap();
    if manager.active_run_id.is_some() || manager.admin_busy {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "希尔娅 is busy with another operation",
        ));
    }
    manager.admin_busy = true;
    Ok(())
}

fn require_no_running_turn(state_store: &StateStore) -> std::result::Result<(), ApiError> {
    if state_store
        .has_running_turns()
        .map_err(ApiError::internal)?
    {
        Err(ApiError::new(
            StatusCode::CONFLICT,
            "a conversation turn is already running",
        ))
    } else {
        Ok(())
    }
}

fn release_admin(manager: &Arc<Mutex<ManagerState>>) {
    manager.lock().unwrap().admin_busy = false;
}

fn config_response(
    config: &AppConfig,
    context: ContextSnapshot,
    paths: &GqyPaths,
) -> std::result::Result<ConfigResponse, ApiError> {
    let mut redacted = config.clone();
    let mut secret_states = HashMap::new();
    for (index, provider) in redacted.providers.iter_mut().enumerate() {
        secret_states.insert(
            format!("providers.{index}.api_key"),
            provider
                .api_key
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
        );
        provider.api_key = None;
    }
    redact_secret_list(
        &mut secret_states,
        "plugins.web.tavily_api_keys",
        &mut redacted.plugins.web.tavily_api_keys,
    );
    redact_secret_list(
        &mut secret_states,
        "plugins.web.firecrawl_api_keys",
        &mut redacted.plugins.web.firecrawl_api_keys,
    );
    redact_secret_list(
        &mut secret_states,
        "plugins.web.anysearch_api_keys",
        &mut redacted.plugins.web.anysearch_api_keys,
    );
    secret_states.insert(
        "plugins.exchange_rate.api_key".to_string(),
        !redacted.plugins.exchange_rate.api_key.trim().is_empty(),
    );
    redacted.plugins.exchange_rate.api_key.clear();
    redact_secret_list(
        &mut secret_states,
        "plugins.image_generation.api_keys",
        &mut redacted.plugins.image_generation.api_keys,
    );
    let mut config_value = serde_json::to_value(&redacted).map_err(ApiError::internal)?;
    if let Value::Object(config_object) = &mut config_value {
        config_object.insert(
            "memory".to_string(),
            serde_json::to_value(redacted.memory_config()).map_err(ApiError::internal)?,
        );
    }
    let prompts = read_prompt_documents(config, paths).map_err(ApiError::internal)?;
    Ok(ConfigResponse {
        config: config_value,
        secret_states,
        prompts,
        models: safe_models(config),
        multimodal_models: safe_multimodal_models(config),
        display: web_display_config(config),
        context,
    })
}

fn redact_secret_list(states: &mut HashMap<String, bool>, key: &str, values: &mut Vec<String>) {
    states.insert(
        key.to_string(),
        values.iter().any(|value| !value.trim().is_empty()),
    );
    values.clear();
}

fn restore_config_secrets(
    candidate: &mut AppConfig,
    current: &AppConfig,
    mutations: &HashMap<String, SecretMutation>,
) -> std::result::Result<(), ApiError> {
    let mut recognized = HashSet::new();
    for (index, provider) in candidate.providers.iter_mut().enumerate() {
        let key = format!("providers.{index}.api_key");
        recognized.insert(key.clone());
        let existing = current
            .providers
            .iter()
            .find(|item| item.id == provider.id)
            .and_then(|item| item.api_key.clone());
        provider.api_key = match mutations.get(&key) {
            Some(SecretMutation::Set(value)) => normalize_single_secret(value, &key)?,
            Some(SecretMutation::Clear) => None,
            None => existing,
        };
    }

    restore_secret_list(
        candidate,
        current,
        mutations,
        &mut recognized,
        "plugins.web.tavily_api_keys",
        |config| &mut config.plugins.web.tavily_api_keys,
        |config| &config.plugins.web.tavily_api_keys,
    )?;
    restore_secret_list(
        candidate,
        current,
        mutations,
        &mut recognized,
        "plugins.web.firecrawl_api_keys",
        |config| &mut config.plugins.web.firecrawl_api_keys,
        |config| &config.plugins.web.firecrawl_api_keys,
    )?;
    restore_secret_list(
        candidate,
        current,
        mutations,
        &mut recognized,
        "plugins.web.anysearch_api_keys",
        |config| &mut config.plugins.web.anysearch_api_keys,
        |config| &config.plugins.web.anysearch_api_keys,
    )?;
    restore_secret_list(
        candidate,
        current,
        mutations,
        &mut recognized,
        "plugins.image_generation.api_keys",
        |config| &mut config.plugins.image_generation.api_keys,
        |config| &config.plugins.image_generation.api_keys,
    )?;

    let exchange_key = "plugins.exchange_rate.api_key";
    recognized.insert(exchange_key.to_string());
    candidate.plugins.exchange_rate.api_key = match mutations.get(exchange_key) {
        Some(SecretMutation::Set(value)) => {
            normalize_single_secret(value, exchange_key)?.unwrap_or_default()
        }
        Some(SecretMutation::Clear) => String::new(),
        None => current.plugins.exchange_rate.api_key.clone(),
    };

    if let Some(key) = mutations.keys().find(|key| !recognized.contains(*key)) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("unknown secret field: {key}"),
        ));
    }
    Ok(())
}

fn restore_secret_list<Mut, Ref>(
    candidate: &mut AppConfig,
    current: &AppConfig,
    mutations: &HashMap<String, SecretMutation>,
    recognized: &mut HashSet<String>,
    key: &str,
    candidate_values: Mut,
    current_values: Ref,
) -> std::result::Result<(), ApiError>
where
    Mut: FnOnce(&mut AppConfig) -> &mut Vec<String>,
    Ref: FnOnce(&AppConfig) -> &Vec<String>,
{
    recognized.insert(key.to_string());
    *candidate_values(candidate) = match mutations.get(key) {
        Some(SecretMutation::Set(value)) => parse_secret_list(value, key)?,
        Some(SecretMutation::Clear) => Vec::new(),
        None => current_values(current).clone(),
    };
    Ok(())
}

fn normalize_single_secret(
    value: &str,
    field: &str,
) -> std::result::Result<Option<String>, ApiError> {
    validate_secret_text(value, field)?;
    Ok(Some(value.trim().to_string()).filter(|value| !value.is_empty()))
}

fn parse_secret_list(value: &str, field: &str) -> std::result::Result<Vec<String>, ApiError> {
    validate_secret_text(value, field)?;
    Ok(value
        .split(|character| matches!(character, ',' | '\n' | '\r'))
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect())
}

fn validate_secret_text(value: &str, field: &str) -> std::result::Result<(), ApiError> {
    if value.chars().count() > MAX_SECRET_CHARS
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("{field} is invalid"),
        ));
    }
    Ok(())
}

fn validate_config_candidate(config: &AppConfig) -> std::result::Result<(), ApiError> {
    config.validate().map_err(|error| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid configuration: {}", safe_error_message(error)),
        )
    })?;
    let mut provider_ids = HashSet::with_capacity(config.providers.len());
    for provider in &config.providers {
        if !provider_ids.insert(provider.id.trim()) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("duplicate provider id: {}", provider.id),
            ));
        }
    }
    if let Some(active) = &config.active_provider_models {
        let mut checked = config.clone();
        checked
            .set_active_provider_models(active)
            .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, safe_error_message(error)))?;
    }
    if let Some(active) = &config.active_multimodal_provider_models {
        let choices = config.provider_model_choices();
        let mut seen = HashSet::with_capacity(active.len());
        for model in active {
            if !seen.insert((&model.provider_id, &model.model))
                || !choices.iter().any(|choice| {
                    choice.provider_id == model.provider_id && choice.model == model.model
                })
            {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "invalid multimodal provider/model: {} / {}",
                        model.provider_id, model.model
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_prompt_documents(
    config: &AppConfig,
    prompts: &PromptDocuments,
) -> std::result::Result<(), ApiError> {
    validate_prompt_document_list("persona", &prompts.personas)?;
    validate_prompt_document_list("identity", &prompts.identities)?;
    if !config.prompt.active_persona.trim().is_empty()
        && !prompts
            .personas
            .iter()
            .any(|document| document.name == config.prompt.active_persona)
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "the active persona does not exist",
        ));
    }
    if !config.prompt.active_identity.trim().is_empty()
        && !prompts
            .identities
            .iter()
            .any(|document| document.name == config.prompt.active_identity)
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "the active identity does not exist",
        ));
    }
    Ok(())
}

fn validate_prompt_document_list(
    kind: &str,
    documents: &[PromptDocument],
) -> std::result::Result<(), ApiError> {
    if documents.len() > MAX_PROMPT_DOCUMENTS {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("at most {MAX_PROMPT_DOCUMENTS} {kind} documents are allowed"),
        ));
    }
    let mut names = HashSet::with_capacity(documents.len());
    let mut original_names = HashSet::with_capacity(documents.len());
    for document in documents {
        validate_prompt_document_name(&document.name, kind)?;
        if !names.insert(document.name.as_str()) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("duplicate {kind} document: {}", document.name),
            ));
        }
        if document.content.chars().count() > MAX_PROMPT_DOCUMENT_CHARS
            || document.content.contains('\0')
        {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("{kind} document is too large: {}", document.name),
            ));
        }
        if let Some(original) = document.original_name.as_deref() {
            validate_prompt_document_name(original, kind)?;
            if !original_names.insert(original) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("duplicate original {kind} document: {original}"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_prompt_document_name(name: &str, kind: &str) -> std::result::Result<(), ApiError> {
    let valid = name == name.trim()
        && name.ends_with(".md")
        && name.len() <= 240
        && name.len() > 3
        && !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
        && FilePath::new(name)
            .file_name()
            .and_then(|value| value.to_str())
            == Some(name);
    if !valid {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("invalid {kind} document name: {name}"),
        ));
    }
    Ok(())
}

fn read_prompt_documents(config: &AppConfig, paths: &GqyPaths) -> Result<PromptDocuments> {
    Ok(PromptDocuments {
        personas: read_prompt_document_dir(&config.prompts_dir_path(paths))?,
        identities: read_prompt_document_dir(&config.identities_dir_path(paths))?,
    })
}

fn read_prompt_document_dir(dir: &FilePath) -> Result<Vec<PromptDocument>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut documents = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") {
            continue;
        }
        let content = std::fs::read_to_string(entry.path())?;
        documents.push(PromptDocument {
            original_name: Some(name.clone()),
            name,
            content,
        });
    }
    documents.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(documents)
}

fn prompt_configuration_changed(current: &AppConfig, candidate: &AppConfig) -> bool {
    serde_json::to_value(&current.prompt).ok() != serde_json::to_value(&candidate.prompt).ok()
        || current.system_prompt_file != candidate.system_prompt_file
        || current.system_prompt != candidate.system_prompt
}

fn prompt_documents_changed(current: &PromptDocuments, candidate: &PromptDocuments) -> bool {
    canonical_prompt_documents(&current.personas) != canonical_prompt_documents(&candidate.personas)
        || canonical_prompt_documents(&current.identities)
            != canonical_prompt_documents(&candidate.identities)
}

fn canonical_prompt_documents(documents: &[PromptDocument]) -> Vec<(String, String)> {
    let mut values = documents
        .iter()
        .map(|document| (document.name.clone(), document.content.clone()))
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.0.cmp(&right.0));
    values
}

struct FileBackup {
    path: PathBuf,
    content: Option<Vec<u8>>,
}

struct PersonaScopeBackup {
    original: PathBuf,
    staged: PathBuf,
    destination: Option<PathBuf>,
}

fn apply_prompt_documents(
    current_config: &AppConfig,
    next_config: &AppConfig,
    current: &PromptDocuments,
    next: &PromptDocuments,
    paths: &GqyPaths,
) -> Result<Vec<FileBackup>> {
    let mut mutations = HashMap::<PathBuf, Option<Vec<u8>>>::new();
    collect_prompt_file_mutations(
        &current.personas,
        &next.personas,
        &current_config.prompts_dir_path(paths),
        &next_config.prompts_dir_path(paths),
        &mut mutations,
    );
    collect_prompt_file_mutations(
        &current.identities,
        &next.identities,
        &current_config.identities_dir_path(paths),
        &next_config.identities_dir_path(paths),
        &mut mutations,
    );
    let backups = mutations
        .keys()
        .map(|path| FileBackup {
            path: path.clone(),
            content: std::fs::read(path).ok(),
        })
        .collect::<Vec<_>>();
    for (path, content) in mutations {
        let result = if let Some(content) = content {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, content)
        } else if path.exists() {
            std::fs::remove_file(&path)
        } else {
            Ok(())
        };
        if let Err(error) = result {
            restore_file_backups(&backups);
            return Err(error.into());
        }
    }
    Ok(backups)
}

fn apply_persona_scope_changes(
    current_config: &AppConfig,
    next_config: &AppConfig,
    current: &PromptDocuments,
    next: &PromptDocuments,
    paths: &GqyPaths,
) -> Result<Vec<PersonaScopeBackup>> {
    let mut changes = Vec::<(String, Option<String>)>::new();
    for document in &current.personas {
        let represented = next.personas.iter().find(|next_document| {
            next_document.original_name.as_deref() == Some(document.name.as_str())
                || next_document.original_name.is_none() && next_document.name == document.name
        });
        match represented {
            Some(next_document) if next_document.name != document.name => {
                changes.push((document.name.clone(), Some(next_document.name.clone())));
            }
            None => changes.push((document.name.clone(), None)),
            _ => {}
        }
    }

    let mut backups = Vec::new();
    let stage_result = (|| -> Result<()> {
        for (change_index, (old_name, new_name)) in changes.iter().enumerate() {
            let old_paths = [
                current_config.persona_memory_data_dir(paths, old_name),
                current_config.persona_memory_state_dir(paths, old_name),
                current_config.persona_skills_dir(paths, old_name),
            ];
            let new_paths = new_name.as_ref().map(|name| {
                [
                    next_config.persona_memory_data_dir(paths, name),
                    next_config.persona_memory_state_dir(paths, name),
                    next_config.persona_skills_dir(paths, name),
                ]
            });
            for (scope_index, original) in old_paths.into_iter().enumerate() {
                if !original.exists() {
                    continue;
                }
                let parent = original
                    .parent()
                    .context("persona scope path has no parent")?;
                let staged = parent.join(format!(
                    ".hilia-web-scope-{}-{change_index}-{scope_index}",
                    random_token(10)
                ));
                std::fs::rename(&original, &staged)?;
                backups.push(PersonaScopeBackup {
                    original,
                    staged,
                    destination: new_paths.as_ref().map(|paths| paths[scope_index].clone()),
                });
            }
        }
        Ok(())
    })();
    if let Err(error) = stage_result {
        restore_persona_scope_backups(&backups);
        return Err(error);
    }

    let result = (|| -> Result<()> {
        for backup in &backups {
            let Some(destination) = &backup.destination else {
                continue;
            };
            if destination.exists() {
                anyhow::bail!(
                    "persona scope destination already exists: {}",
                    destination.display()
                );
            }
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&backup.staged, destination)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        restore_persona_scope_backups(&backups);
        return Err(error);
    }
    Ok(backups)
}

fn restore_persona_scope_backups(backups: &[PersonaScopeBackup]) {
    for backup in backups.iter().rev() {
        if let Some(destination) = &backup.destination {
            if destination.exists() && !backup.staged.exists() {
                let _ = std::fs::rename(destination, &backup.staged);
            }
        }
        if backup.staged.exists() {
            if let Some(parent) = backup.original.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::rename(&backup.staged, &backup.original);
        }
    }
}

fn finalize_persona_scope_backups(backups: &[PersonaScopeBackup]) {
    for backup in backups {
        if backup.destination.is_none() && backup.staged.exists() {
            let _ = std::fs::remove_dir_all(&backup.staged);
        }
    }
}

fn collect_prompt_file_mutations(
    current: &[PromptDocument],
    next: &[PromptDocument],
    current_dir: &FilePath,
    next_dir: &FilePath,
    mutations: &mut HashMap<PathBuf, Option<Vec<u8>>>,
) {
    for document in next {
        let content = document.content.trim_end();
        let content = if content.is_empty() {
            Vec::new()
        } else {
            format!("{content}\n").into_bytes()
        };
        mutations.insert(next_dir.join(&document.name), Some(content));
    }
    for document in current {
        let represented = next.iter().find(|next_document| {
            next_document.original_name.as_deref() == Some(document.name.as_str())
                || next_document.original_name.is_none() && next_document.name == document.name
        });
        let old_path = current_dir.join(&document.name);
        let retained_at_same_path = represented
            .map(|next_document| next_dir.join(&next_document.name) == old_path)
            .unwrap_or(false);
        if !retained_at_same_path {
            mutations.entry(old_path).or_insert(None);
        }
    }
}

fn restore_file_backups(backups: &[FileBackup]) {
    for backup in backups {
        restore_optional_file(&backup.path, backup.content.as_deref());
    }
}

fn restore_optional_file(path: &FilePath, content: Option<&[u8]>) {
    if let Some(content) = content {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, content);
    } else if path.exists() {
        let _ = std::fs::remove_file(path);
    }
}

fn safe_models(config: &AppConfig) -> Vec<SafeModel> {
    config
        .provider_model_choices()
        .into_iter()
        .map(|choice| SafeModel {
            active: config.is_active_provider_model(&choice.provider_id, &choice.model),
            provider_id: choice.provider_id,
            provider_name: choice.provider_name,
            model: choice.model,
        })
        .collect()
}

fn web_display_config(config: &AppConfig) -> WebDisplayConfig {
    let mixed_model_endpoint_display = config.display.mixed_model_endpoint_display.clone();
    WebDisplayConfig {
        reasoning: config.display.reasoning.clone(),
        tool_calls: config.display.tool_calls.clone(),
        readable_tool_names: config.display.readable_tool_names,
        command_output_lines: config.display.command_output_lines,
        show_mixed_model_endpoint: config.active_provider_model_choices().len() > 1
            && matches!(mixed_model_endpoint_display.as_str(), "interactive" | "all"),
        mixed_model_endpoint_display,
    }
}

fn safe_multimodal_models(config: &AppConfig) -> Vec<SafeModel> {
    config
        .multimodal_provider_model_choices()
        .into_iter()
        .map(|choice| SafeModel {
            active: config.is_active_multimodal_provider_model(&choice.provider_id, &choice.model),
            provider_id: choice.provider_id,
            provider_name: choice.provider_name,
            model: choice.model,
        })
        .collect()
}

impl SafeTurn {
    fn from_turn(turn: Turn, assets: Vec<ImageAsset>) -> Self {
        let assets = assets
            .into_iter()
            .map(|asset| {
                let hide_caption = meme_asset_caption_hidden(&asset, &turn.tool_reports);
                SafeImageAsset::from_asset(asset, hide_caption)
            })
            .collect();
        Self {
            id: turn.turn_id,
            seq: turn.seq,
            status: match turn.status {
                TurnStatus::Running => "running",
                TurnStatus::Completed => "completed",
                TurnStatus::Interrupted => "interrupted",
            },
            active_context: !turn.hidden,
            user_content: turn.user_content,
            assistant_content: redact_internal_assistant_text(&turn.assistant_content),
            assistant_reasoning: turn
                .assistant_reasoning
                .map(|reasoning| redact_internal_assistant_text(&reasoning)),
            provider_id: turn.assistant_provider_id,
            model: turn.assistant_model,
            user_timestamp: turn.user_timestamp,
            assistant_timestamp: turn.assistant_timestamp,
            token_total: turn.token_total,
            token_usage_estimated: turn.token_usage_estimated,
            question_exchanges: turn.question_exchanges,
            followups: turn.followups.into_iter().map(SafeFollowup::from).collect(),
            assets,
        }
    }
}

impl SafeImageAsset {
    fn from_asset(asset: ImageAsset, hide_caption: bool) -> Self {
        Self {
            url: format!("/api/assets/{}", asset.asset_id),
            id: asset.asset_id,
            mime: asset.mime,
            width: asset.width,
            height: asset.height,
            alt: asset.alt,
            hide_caption,
        }
    }
}

impl From<ImageAsset> for SafeImageAsset {
    fn from(asset: ImageAsset) -> Self {
        Self::from_asset(asset, false)
    }
}

fn meme_asset_caption_hidden(asset: &ImageAsset, reports: &[String]) -> bool {
    const MAX_DESCRIPTION_CHARS: usize = 120;

    let description = asset.alt.split_whitespace().collect::<Vec<_>>().join(" ");
    if description.is_empty() {
        return false;
    }
    let mut characters = description.chars();
    let mut compact = characters
        .by_ref()
        .take(MAX_DESCRIPTION_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        compact.push('…');
    }
    let escaped = compact
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let marker = format!("description={escaped}</sent_meme>");
    reports
        .iter()
        .any(|report| report.starts_with("<sent_meme>") && report.contains(&marker))
}

impl From<TurnFollowup> for SafeFollowup {
    fn from(followup: TurnFollowup) -> Self {
        Self {
            id: followup.prompt_id,
            content: followup.display_content,
            submitted_at: followup.submitted_at,
            preceding_assistant_content: followup
                .preceding_assistant_content
                .map(|content| redact_internal_assistant_text(&content)),
            preceding_assistant_reasoning: followup
                .preceding_assistant_reasoning
                .map(|reasoning| redact_internal_assistant_text(&reasoning)),
            provider_id: followup.preceding_assistant_provider_id,
            model: followup.preceding_assistant_model,
        }
    }
}

impl From<QueuedPrompt> for SafeQueuedPrompt {
    fn from(prompt: QueuedPrompt) -> Self {
        Self {
            id: prompt.prompt_id,
            content: prompt.display_content,
            submitted_at: prompt.submitted_at,
        }
    }
}

impl From<UsageSnapshot> for SafeUsageSnapshot {
    fn from(usage: UsageSnapshot) -> Self {
        Self {
            requests: usage.requests,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            last_usage: usage.last_usage,
            last_conversation_usage: usage.last_conversation_usage,
        }
    }
}

fn redact_internal_assistant_text(value: &str) -> String {
    value
        .replace(crate::state::pending_placeholder(), "")
        .replace(crate::state::interrupted_text(), "")
}

fn normalize_answers(
    request: &QuestionRequest,
    mut answers: QuestionAnswers,
) -> std::result::Result<QuestionAnswers, String> {
    for answer in &mut answers {
        for value in answer {
            *value = value.trim().to_string();
            if value.chars().any(char::is_control) {
                return Err("answers cannot contain control characters".to_string());
            }
        }
    }
    question::validate_answers(request, &answers).map_err(|error| safe_error_message(&error))?;
    Ok(answers)
}

fn validate_content(content: String) -> std::result::Result<String, ApiError> {
    let content = content.trim().to_string();
    if content.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "content cannot be empty",
        ));
    }
    if content.chars().count() > MAX_CONTENT_CHARS {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("content cannot exceed {MAX_CONTENT_CHARS} characters"),
        ));
    }
    if content
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "content contains unsupported control characters",
        ));
    }
    Ok(content)
}

fn validate_short_field(
    value: String,
    field: &str,
    max_chars: usize,
) -> std::result::Result<String, ApiError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("{field} cannot be empty"),
        ));
    }
    if value.chars().count() > max_chars || value.chars().any(char::is_control) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("{field} is invalid"),
        ));
    }
    Ok(value)
}

fn validate_model_selection(
    models: Vec<ActiveProviderModelConfig>,
) -> std::result::Result<Vec<ActiveProviderModelConfig>, ApiError> {
    if models.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "at least one model endpoint must remain active",
        ));
    }
    if models.len() > 64 {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "at most 64 model endpoints can be active",
        ));
    }
    let mut seen = HashSet::with_capacity(models.len());
    let mut validated = Vec::with_capacity(models.len());
    for model in models {
        let provider_id = validate_short_field(model.provider_id, "provider_id", 200)?;
        let model = validate_short_field(model.model, "model", 500)?;
        if !seen.insert((provider_id.clone(), model.clone())) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "duplicate provider/model selection",
            ));
        }
        validated.push(ActiveProviderModelConfig { provider_id, model });
    }
    Ok(validated)
}

fn parse_mode(mode: &str) -> std::result::Result<AgentMode, ApiError> {
    match mode {
        "normal" => Ok(AgentMode::Normal),
        "plan" => Ok(AgentMode::Plan),
        "chat" => Ok(AgentMode::Chat),
        _ => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "mode must be normal, plan, or chat",
        )),
    }
}

fn mode_name(mode: AgentMode) -> &'static str {
    match mode {
        AgentMode::Normal => "normal",
        AgentMode::Plan => "plan",
        AgentMode::Chat => "chat",
    }
}

fn real_tool_name(event_name: &str) -> &str {
    if event_name.starts_with("load_skill:") {
        "load_skill"
    } else if event_name.starts_with("load_tools:") {
        "load_tools"
    } else {
        event_name
    }
}

fn require_auth(headers: &HeaderMap, state: &WebState) -> std::result::Result<(), ApiError> {
    if state
        .auth
        .is_authenticated(cookie_value(headers, AUTH_COOKIE))
    {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "authentication required",
        ))
    }
}

fn require_mutation(headers: &HeaderMap, state: &WebState) -> std::result::Result<(), ApiError> {
    require_auth(headers, state)?;
    if origin_is_allowed(headers) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "request origin is not allowed",
        ))
    }
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    for header in headers.get_all(COOKIE) {
        let Ok(header) = header.to_str() else {
            continue;
        };
        for pair in header.split(';') {
            let Some((key, value)) = pair.trim().split_once('=') else {
                continue;
            };
            if key.trim() == name {
                return Some(value.trim());
            }
        }
    }
    None
}

fn origin_is_allowed(headers: &HeaderMap) -> bool {
    let mut origins = headers.get_all(ORIGIN).iter();
    let Some(origin) = origins.next() else {
        return true;
    };
    if origins.next().is_some() {
        return false;
    }
    let Some(host) = headers.get(HOST).and_then(|host| host.to_str().ok()) else {
        return false;
    };
    let expected = format!("http://{host}");
    origin.to_str().is_ok_and(|origin| origin == expected)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let left = left.get(index).copied().unwrap_or(0);
        let right = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(left ^ right);
    }
    difference == 0
}

fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

fn random_id(prefix: &str, bytes: usize) -> String {
    format!("{prefix}_{}", random_token(bytes))
}

fn safe_error_message(error: impl std::fmt::Display) -> String {
    let message = error
        .to_string()
        .chars()
        .filter(|character| !character.is_control() || *character == '\n' || *character == '\t')
        .take(1000)
        .collect::<String>();
    if message.trim().is_empty() {
        "operation failed".to_string()
    } else {
        message
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn open_browser(url: &str) {
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    if let Ok(mut child) = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn open_browser(_url: &str) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::{QuestionOption, QuestionPrompt};

    #[test]
    fn assistant_sentinels_are_never_exposed() {
        assert_eq!(
            redact_internal_assistant_text(crate::state::pending_placeholder()),
            ""
        );
        assert_eq!(
            redact_internal_assistant_text(crate::state::interrupted_text()),
            ""
        );
        let combined = format!("before {} after", crate::state::interrupted_text());
        let redacted = redact_internal_assistant_text(&combined);
        assert_eq!(redacted, "before  after");
        assert!(!redacted.contains("system-reminder"));
    }

    #[test]
    fn persisted_meme_assets_hide_their_descriptive_caption() {
        let asset = ImageAsset {
            asset_id: "img_test".to_string(),
            turn_id: "turn_test".to_string(),
            tool_id: Some("tool_test".to_string()),
            mime: "image/png".to_string(),
            width: 64,
            height: 64,
            alt: "猫猫 开心 & <得意>".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let reports = vec![
            "<sent_meme>发送了一个表情包：id=sha256:test；description=猫猫 开心 &amp; &lt;得意&gt;</sent_meme>"
                .to_string(),
        ];

        assert!(meme_asset_caption_hidden(&asset, &reports));
        assert!(!meme_asset_caption_hidden(
            &asset,
            &["normal tool output".to_string()]
        ));
    }

    #[test]
    fn cookie_parser_matches_an_exact_cookie_name() {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_static("other=1; hilia_session=secret-token; suffix=2"),
        );
        assert_eq!(cookie_value(&headers, AUTH_COOKIE), Some("secret-token"));
        assert_eq!(cookie_value(&headers, "session"), None);
    }

    #[test]
    fn origin_check_accepts_absent_or_current_host_origin() {
        let mut headers = HeaderMap::new();
        assert!(origin_is_allowed(&headers));
        headers.insert(HOST, HeaderValue::from_static("192.168.1.20:4096"));
        headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:4096"));
        assert!(!origin_is_allowed(&headers));
        headers.insert(ORIGIN, HeaderValue::from_static("http://192.168.1.20:4096"));
        assert!(origin_is_allowed(&headers));
        headers.append(ORIGIN, HeaderValue::from_static("http://192.168.1.20:4096"));
        assert!(!origin_is_allowed(&headers));
    }

    #[test]
    fn optional_password_auth_issues_server_side_sessions_and_limits_failures() {
        let disabled = WebAuth::new(None);
        assert!(disabled.is_authenticated(None));

        let auth = WebAuth::new(Some("correct horse"));
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(!auth.is_authenticated(None));
        assert!(matches!(
            auth.login(peer, "wrong"),
            Err(LoginFailure::Invalid)
        ));
        let token = auth.login(peer, "correct horse").unwrap();
        assert!(auth.is_authenticated(Some(&token)));

        let limited = WebAuth::new(Some("secret"));
        for _ in 0..LOGIN_ATTEMPT_LIMIT {
            assert!(matches!(
                limited.login(peer, "wrong"),
                Err(LoginFailure::Invalid)
            ));
        }
        assert!(matches!(
            limited.login(peer, "secret"),
            Err(LoginFailure::RateLimited)
        ));
    }

    #[test]
    fn model_selection_rejects_empty_and_duplicate_pools() {
        assert!(validate_model_selection(Vec::new()).is_err());
        let model = ActiveProviderModelConfig {
            provider_id: "provider".to_string(),
            model: "model".to_string(),
        };
        assert!(validate_model_selection(vec![model.clone()]).is_ok());
        assert!(validate_model_selection(vec![model.clone(), model]).is_err());
    }

    #[test]
    fn config_response_never_serializes_secret_values() {
        let mut config = AppConfig::default();
        config.providers[0].api_key = Some("provider-secret".to_string());
        config.plugins.web.tavily_api_keys = vec!["tavily-secret".to_string()];
        config.plugins.exchange_rate.api_key = "exchange-secret".to_string();
        config.plugins.image_generation.api_keys = vec!["image-secret".to_string()];
        let paths = tempfile::tempdir().unwrap();
        let paths = GqyPaths {
            config_dir: paths.path().join("config"),
            config_file: paths.path().join("config/config.jsonc"),
            skills_dir: paths.path().join("config/skills"),
            data_dir: paths.path().join("data"),
            cache_dir: paths.path().join("cache"),
            state_dir: paths.path().join("state"),
            pictures_dir: paths.path().join("pictures"),
            fish_hook_file: paths.path().join("fish"),
            bash_hook_file: paths.path().join("bash"),
            zsh_hook_file: paths.path().join("zsh"),
            scripts_dir: paths.path().join("scripts"),
            system_scripts_dir: paths.path().join("system-scripts"),
            share_dir: PathBuf::new(),
            kb_dir: PathBuf::new(),
        };
        let response = config_response(
            &config,
            ContextSnapshot {
                tokens: 0,
                window: None,
            },
            &paths,
        )
        .unwrap();
        let serialized = serde_json::to_string(&response).unwrap();
        assert!(!serialized.contains("provider-secret"));
        assert!(!serialized.contains("tavily-secret"));
        assert!(!serialized.contains("exchange-secret"));
        assert!(!serialized.contains("image-secret"));
        assert_eq!(response.secret_states["providers.0.api_key"], true);
        assert_eq!(response.secret_states["plugins.web.tavily_api_keys"], true);
        assert!(response.config.get("memory").is_some());
    }

    #[test]
    fn omitted_provider_secret_does_not_follow_array_position_after_rename() {
        let mut current = AppConfig::default();
        current.providers[0].id = "first".to_string();
        current.providers[0].api_key = Some("first-secret".to_string());
        let mut candidate = current.clone();
        candidate.providers[0].id = "renamed".to_string();
        candidate.providers[0].api_key = None;
        restore_config_secrets(&mut candidate, &current, &HashMap::new()).unwrap();
        assert_eq!(candidate.providers[0].api_key, None);
    }

    #[test]
    fn explicit_secret_clear_removes_a_provider_key() {
        let mut current = AppConfig::default();
        current.providers[0].api_key = Some("secret".to_string());
        let mut candidate = current.clone();
        candidate.providers[0].api_key = None;
        let mutations = HashMap::from([("providers.0.api_key".to_string(), SecretMutation::Clear)]);
        restore_config_secrets(&mut candidate, &current, &mutations).unwrap();
        assert_eq!(candidate.providers[0].api_key, None);
    }

    #[test]
    fn stale_event_cursor_receives_resync_marker() {
        let events = EventHub::new();
        for index in 0..=EVENT_CAPACITY {
            events.publish("test", json!({ "index": index }));
        }
        let replay = events.replay_after(0);
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].kind, "resync_required");
        assert_eq!(replay[0].id, events.latest_id());
        let next = events.publish("after-resync", json!({}));
        assert!(next > replay[0].id);
    }

    #[test]
    fn replay_after_cursor_is_ordered_and_exclusive() {
        let events = EventHub::new();
        events.publish("one", json!({}));
        events.publish("two", json!({}));
        events.publish("three", json!({}));
        let replay = events.replay_after(1);
        assert_eq!(
            replay.iter().map(|record| record.id).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn future_event_cursor_requests_resync_after_server_restart() {
        let events = EventHub::new();
        let replay = events.replay_after(42);
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].kind, "resync_required");
    }

    #[test]
    fn answer_validation_trims_values_and_rejects_control_characters() {
        let request = sample_question();
        assert_eq!(
            normalize_answers(&request, vec![vec!["  All  ".to_string()]]).unwrap(),
            vec![vec!["All".to_string()]]
        );
        assert!(normalize_answers(&request, vec![vec!["bad\nanswer".to_string()]]).is_err());
    }

    #[test]
    fn invalid_answer_keeps_question_pending() {
        let broker = QuestionBroker::new();
        let (responder, mut response) = oneshot::channel();
        let question_id = broker.insert("run_test", sample_question(), responder);
        let invalid = broker.answer(&question_id, vec![Vec::new()], |_, _| {
            panic!("invalid answer must not be published")
        });
        assert!(matches!(invalid, Err(AnswerFailure::Invalid(_))));
        assert!(broker.pending.lock().unwrap().contains_key(&question_id));

        broker
            .answer(
                &question_id,
                vec![vec![" All ".to_string()]],
                |run_id, answers| {
                    assert_eq!(run_id, "run_test");
                    assert_eq!(answers, &vec![vec!["All".to_string()]]);
                },
            )
            .unwrap();
        assert!(matches!(
            response.try_recv().unwrap(),
            QuestionResponse::Answered(answers) if answers == vec![vec!["All".to_string()]]
        ));
    }

    #[test]
    fn closed_question_responder_does_not_publish_an_answer() {
        let broker = QuestionBroker::new();
        let (responder, response) = oneshot::channel();
        drop(response);
        let question_id = broker.insert("run_test", sample_question(), responder);
        let mut published = false;
        let result = broker.answer(&question_id, vec![vec!["All".to_string()]], |_, _| {
            published = true
        });
        assert!(matches!(result, Err(AnswerFailure::Gone)));
        assert!(!published);
    }

    fn sample_question() -> QuestionRequest {
        QuestionRequest {
            questions: vec![QuestionPrompt {
                header: "Scope".to_string(),
                question: "Which scope?".to_string(),
                options: vec![QuestionOption {
                    label: "All".to_string(),
                    description: String::new(),
                }],
                multiple: false,
                custom: true,
            }],
        }
    }

    #[test]
    fn content_limit_counts_characters() {
        assert!(validate_content("x".repeat(MAX_CONTENT_CHARS)).is_ok());
        let error = validate_content("界".repeat(MAX_CONTENT_CHARS + 1)).unwrap_err();
        assert_eq!(error.status, StatusCode::PAYLOAD_TOO_LARGE);
    }
}

// ─────────────────────────── 学分管理（面板页面 + API） ───────────────────────────
// 服务对象：大学辅导员（管理员）。页面 /credits 与面板共用会话认证。

use crate::state::CreditsDb as CreditsDbState;

async fn credits_asset() -> Response {
    text_asset(CREDITS_HTML, "text/html; charset=utf-8")
}

async fn credits_js_asset() -> Response {
    text_asset(CREDITS_JS, "application/javascript; charset=utf-8")
}

fn credits_db(state: &WebState) -> std::result::Result<CreditsDbState, ApiError> {
    CreditsDbState::open(&state.paths.data_dir).map_err(ApiError::internal)
}

fn credits_query<T: Into<String>>(value: Option<T>) -> Option<String> {
    value.map(Into::into).filter(|v| !v.trim().is_empty())
}

/// 概览：班级 / 类型 / 统计。
async fn credits_overview(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    let classes = db.list_classes().map_err(ApiError::internal)?;
    let types = db.list_credit_types().map_err(ApiError::internal)?;
    let total_classes = classes.len();
    let total_students = db.query_students(None, "").map_err(ApiError::internal)?.len();
    let records = db.query_credits(None, None, None, "", "").map_err(ApiError::internal)?;
    let total_records = records.len();
    let total_points: f64 = records.iter().map(|r| r.points).sum();
    Ok(Json(json!({
        "classes": classes,
        "types": types,
        "total_classes": total_classes,
        "total_students": total_students,
        "total_records": total_records,
        "total_points": total_points,
    })))
}

async fn credits_classes(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    Ok(Json(json!({ "classes": db.list_classes().map_err(ApiError::internal)? })))
}

async fn credits_class_add(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    let id = db
        .add_class(
            body.get("name").and_then(Value::as_str).unwrap_or(""),
            body.get("grade").and_then(Value::as_str).unwrap_or(""),
            body.get("major").and_then(Value::as_str).unwrap_or(""),
            body.get("note").and_then(Value::as_str).unwrap_or(""),
        )
        .map_err(ApiError::bad_request)?;
    Ok(Json(json!({ "id": id })))
}

async fn credits_class_update(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    db.update_class(
        id,
        body.get("name").and_then(Value::as_str),
        body.get("grade").and_then(Value::as_str),
        body.get("major").and_then(Value::as_str),
        body.get("note").and_then(Value::as_str),
    )
    .map_err(ApiError::bad_request)?;
    Ok(Json(json!({ "ok": true })))
}

async fn credits_class_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    let affected = db.delete_class(id).map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true, "unlinked": affected })))
}

async fn credits_types(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    Ok(Json(json!({ "types": db.list_credit_types().map_err(ApiError::internal)? })))
}

async fn credits_type_add(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    let id = db
        .add_credit_type(
            body.get("name").and_then(Value::as_str).unwrap_or(""),
            body.get("description").and_then(Value::as_str).unwrap_or(""),
            body.get("max_points").and_then(Value::as_f64).unwrap_or(0.0),
        )
        .map_err(ApiError::bad_request)?;
    Ok(Json(json!({ "id": id })))
}

async fn credits_students(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    let class_id = query
        .get("class_id")
        .and_then(|v| v.parse::<i64>().ok());
    let keyword = query.get("keyword").map(String::as_str).unwrap_or("");
    let students = db.query_students(class_id, keyword).map_err(ApiError::internal)?;
    // 附带每个学生学分汇总
    let mut with_totals = Vec::new();
    for student in &students {
        let summary = db.summary(Some(student.id), None).map_err(ApiError::internal)?;
        let mut value = serde_json::to_value(student).map_err(ApiError::internal)?;
        value["total_points"] = json!(summary.total);
        with_totals.push(value);
    }
    Ok(Json(json!({ "students": with_totals })))
}

async fn credits_student_add(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    let id = db
        .add_student(
            body.get("student_no").and_then(Value::as_str).unwrap_or(""),
            body.get("name").and_then(Value::as_str).unwrap_or(""),
            body.get("class_id").and_then(Value::as_i64),
            body.get("gender").and_then(Value::as_str).unwrap_or(""),
            body.get("phone").and_then(Value::as_str).unwrap_or(""),
            body.get("qq_id").and_then(Value::as_str).unwrap_or(""),
            body.get("wecom_id").and_then(Value::as_str).unwrap_or(""),
            body.get("feishu_id").and_then(Value::as_str).unwrap_or(""),
            body.get("note").and_then(Value::as_str).unwrap_or(""),
        )
        .map_err(ApiError::bad_request)?;
    Ok(Json(json!({ "id": id })))
}

async fn credits_student_update(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    db.update_student(
        id,
        body.get("student_no").and_then(Value::as_str),
        body.get("name").and_then(Value::as_str),
        body.get("class_id").and_then(Value::as_i64).map(Some),
        body.get("gender").and_then(Value::as_str),
        body.get("phone").and_then(Value::as_str),
        body.get("qq_id").and_then(Value::as_str),
        body.get("wecom_id").and_then(Value::as_str),
        body.get("feishu_id").and_then(Value::as_str),
        body.get("note").and_then(Value::as_str),
    )
    .map_err(ApiError::bad_request)?;
    Ok(Json(json!({ "ok": true })))
}

async fn credits_student_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    let deleted_records = db.delete_student(id).map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true, "deleted_records": deleted_records })))
}

async fn credits_records(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    let student_id = query.get("student_id").and_then(|v| v.parse::<i64>().ok());
    let class_id = query.get("class_id").and_then(|v| v.parse::<i64>().ok());
    let type_id = query.get("type_id").and_then(|v| v.parse::<i64>().ok());
    let semester = query.get("semester").map(String::as_str).unwrap_or("");
    let keyword = query.get("keyword").map(String::as_str).unwrap_or("");
    let records = db
        .query_credits(student_id, class_id, type_id, semester, keyword)
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "records": records })))
}

async fn credits_record_add(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    let id = db
        .add_credit(
            body.get("student_id").and_then(Value::as_i64).unwrap_or(0),
            body.get("type_id").and_then(Value::as_i64),
            body.get("points").and_then(Value::as_f64).unwrap_or(0.0),
            body.get("semester").and_then(Value::as_str).unwrap_or(""),
            body.get("happened_on").and_then(Value::as_str).unwrap_or(""),
            body.get("note").and_then(Value::as_str).unwrap_or(""),
            "面板",
        )
        .map_err(ApiError::bad_request)?;
    Ok(Json(json!({ "id": id })))
}

async fn credits_record_update(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    let type_id = body
        .get("type_id")
        .and_then(Value::as_i64)
        .map(Some);
    db.update_credit(
        id,
        body.get("points").and_then(Value::as_f64).filter(|p| *p != 0.0),
        type_id,
        body.get("semester").and_then(Value::as_str),
        body.get("happened_on").and_then(Value::as_str),
        body.get("note").and_then(Value::as_str),
    )
    .map_err(ApiError::bad_request)?;
    Ok(Json(json!({ "ok": true })))
}

async fn credits_record_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    db.delete_credit(id).map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true })))
}

async fn credits_import(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let db = credits_db(&state)?;
    let csv = body.get("csv").and_then(Value::as_str).unwrap_or("");
    let (imported, skipped) = db.import_students_csv(csv).map_err(ApiError::bad_request)?;
    Ok(Json(json!({
        "ok": true,
        "imported": imported,
        "skipped": skipped,
        "message": format!(
            "导入完成：成功 {imported} 人{}",
            if skipped.is_empty() { String::new() } else { format!("，跳过 {} 条（详见跳过列表）", skipped.len()) }
        ),
    })))
}

/// 按班级导出 xlsx 学分表（需辅导员激活码；未激活时引导联系开发者）。
async fn credits_export(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Response, ApiError> {
    require_mutation(&headers, &state)?;
    // 辅导员授权校验：config.license 已激活且 plan 为 admin/pro
    let config = AppConfig::load_or_default(&state.paths).map_err(ApiError::internal)?;
    let license = crate::license::load(&config);
    let authorized = license.is_activated()
        && (license.plan.contains("admin") || license.plan.contains("pro"));
    if !authorized {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "导出学分表需要辅导员私密钥匙（管理员激活码），请联系开发者 2101497063@qq.com 获取",
        ));
    }
    let class_id = body.get("class_id").and_then(Value::as_i64);
    let paths = state.paths.clone();
    let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let db = crate::state::CreditsDb::open(&paths.data_dir)?;
        build_credits_xlsx(&db, class_id)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)?;
    let filename = {
        let db = crate::state::CreditsDb::open(&state.paths.data_dir).map_err(ApiError::internal)?;
        let label = match class_id {
            Some(id) => db
                .list_classes()
                .map_err(ApiError::internal)?
                .into_iter()
                .find(|class| class.id == id)
                .map(|class| class.name)
                .unwrap_or_else(|| "全部班级".to_string()),
            None => "全部班级".to_string(),
        };
        format!("{}-学分表-{}.xlsx", label, chrono::Local::now().format("%Y%m%d"))
    };
    Ok((
        [
            (
                CONTENT_TYPE.to_string(),
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string(),
            ),
            (
                CONTENT_DISPOSITION.to_string(),
                format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// 生成 xlsx：Sheet1 学分明细，Sheet2 学生汇总（按班级可选）。
fn build_credits_xlsx(db: &crate::state::CreditsDb, class_id: Option<i64>) -> anyhow::Result<Vec<u8>> {
    use rust_xlsxwriter::{Format, FormatBorder, Workbook};
    let mut workbook = Workbook::new();
    let header_format = Format::new().set_bold().set_border(FormatBorder::Thin);
    let cell_format = Format::new().set_border(FormatBorder::Thin);

    // Sheet1：学分明细
    let records = db.query_credits(None, class_id, None, "", "")?;
    let sheet = workbook.add_worksheet().set_name("学分明细")?;
    for (column, title) in [
        "学号",
        "姓名",
        "班级",
        "学分类型",
        "分值",
        "备注",
        "操作人",
        "时间",
    ]
    .iter()
    .enumerate()
    {
        sheet.write_string_with_format(0, column as u16, *title, &header_format)?;
    }
    for (row, record) in records.iter().enumerate() {
        let row = (row + 1) as u32;
        sheet.write_string_with_format(row, 0, &record.student_no, &cell_format)?;
        sheet.write_string_with_format(row, 1, &record.student_name, &cell_format)?;
        sheet.write_string_with_format(
            row,
            2,
            record.class_name.as_deref().unwrap_or("未分班"),
            &cell_format,
        )?;
        sheet.write_string_with_format(
            row,
            3,
            record.type_name.as_deref().unwrap_or("未分类"),
            &cell_format,
        )?;
        sheet.write_number_with_format(row, 4, record.points, &cell_format)?;
        sheet.write_string_with_format(row, 5, &record.note, &cell_format)?;
        sheet.write_string_with_format(row, 6, &record.operator, &cell_format)?;
        sheet.write_string_with_format(row, 7, &record.created_at, &cell_format)?;
    }
    sheet.set_column_width(0, 14)?;
    sheet.set_column_width(1, 12)?;
    sheet.set_column_width(2, 16)?;
    sheet.set_column_width(3, 12)?;
    sheet.set_column_width(4, 8)?;
    sheet.set_column_width(5, 28)?;
    sheet.set_column_width(6, 12)?;
    sheet.set_column_width(7, 22)?;

    // Sheet2：学生汇总
    let students = db.query_students(class_id, "")?;
    let summary_sheet = workbook.add_worksheet().set_name("学生汇总")?;
    for (column, title) in ["学号", "姓名", "班级", "总学分", "类型明细"].iter().enumerate() {
        summary_sheet.write_string_with_format(0, column as u16, *title, &header_format)?;
    }
    for (row, student) in students.iter().enumerate() {
        let row = (row + 1) as u32;
        summary_sheet.write_string_with_format(row, 0, &student.student_no, &cell_format)?;
        summary_sheet.write_string_with_format(row, 1, &student.name, &cell_format)?;
        summary_sheet.write_string_with_format(
            row,
            2,
            student.class_name.as_deref().unwrap_or("未分班"),
            &cell_format,
        )?;
        let summary = db.summary(Some(student.id), None)?;
        summary_sheet.write_number_with_format(row, 3, summary.total, &cell_format)?;
        let detail = summary
            .by_type
            .iter()
            .map(|(name, points)| format!("{name}:{points}"))
            .collect::<Vec<_>>()
            .join("；");
        summary_sheet.write_string_with_format(row, 4, &detail, &cell_format)?;
    }
    summary_sheet.set_column_width(0, 14)?;
    summary_sheet.set_column_width(1, 12)?;
    summary_sheet.set_column_width(2, 16)?;
    summary_sheet.set_column_width(3, 10)?;
    summary_sheet.set_column_width(4, 48)?;

    Ok(workbook.save_to_buffer()?)
}

// ─────────────────────────── 更新检查 API ───────────────────────────

/// 检查更新（面板"版本与更新"区调用）。
async fn update_check_api(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let config = AppConfig::load_or_default(&state.paths).map_err(ApiError::internal)?;
    let paths = state.paths.clone();
    let check_config = config.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::update::check_update(&check_config, &paths)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "ok": true,
        "current": result.current_version,
        "latest": result.latest_version,
        "has_update": result.has_update,
        "forced": result.forced,
        "notes": result.notes,
        "upstream": config.update.upstream_url,
    })))
}

// ─────────────────────────── 设备配对 API（APK 扫码） ───────────────────────────

/// 无密码时限制面板 API 仅本机可达（/pair/ws 直连端点不受限）。
async fn lan_guard(
    State(state): State<WebState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if state.auth.password_digest.is_none() && !addr.ip().is_loopback() {
        return (
            StatusCode::FORBIDDEN,
            "面板仅本机可用；设置访问密码后可从局域网访问",
        )
            .into_response();
    }
    next.run(request).await
}

// ─────────────────────────── 局域网直连（APK 不经公网中继） ───────────────────────────

/// 直连配对中枢：配对码（APK 连上后等面板确认）、设备表、在线连接。
#[derive(Clone)]
struct DirectHub {
    /// 直连配对码 → 待确认配对（APK 已连 ws 等面板确认）
    pending: Arc<Mutex<HashMap<String, PendingPair>>>,
    /// 直连 token → 设备（配对成功后建立，重连用）
    devices: Arc<Mutex<HashMap<String, DirectDevice>>>,
    /// 在线 APK 设备 id → 消息通道（ws 断开即移除）
    online: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<WsMessage>>>>,
}

struct PendingPair {
    apk_label: String,
    expires_at: Instant,
    ws: Option<mpsc::UnboundedSender<WsMessage>>,
}

#[derive(Clone)]
struct DirectDevice {
    device_id: String,
    token: String,
}

impl DirectHub {
    fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            devices: Arc::new(Mutex::new(HashMap::new())),
            online: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 生成直连配对码（8 字符，5 分钟有效；APK 尚未连接，ws 留空）。
    fn create_pair_code(&self) -> String {
        loop {
            let code = direct_pair_code();
            let mut pending = self.pending.lock().unwrap();
            if !pending.contains_key(&code) {
                pending.insert(
                    code.clone(),
                    PendingPair {
                        apk_label: "等待扫码".to_string(),
                        expires_at: Instant::now() + Duration::from_secs(300),
                        ws: None,
                    },
                );
                return code;
            }
        }
    }

    /// APK 连上 /pair/ws 后用配对码登记 → 返回 APK 标签（SSE 推送确认用）。
    fn attach_pair_code(
        &self,
        code: &str,
        ws: mpsc::UnboundedSender<WsMessage>,
    ) -> std::result::Result<String, String> {
        let mut pending = self.pending.lock().unwrap();
        let Some(pair) = pending.get_mut(code) else {
            return Err("配对码无效".to_string());
        };
        if pair.expires_at < Instant::now() {
            pending.remove(code);
            return Err("配对码已过期".to_string());
        }
        pair.ws = Some(ws);
        Ok(pair.apk_label.clone())
    }

    /// 面板确认/拒绝直连配对：接受则签发 token 与设备 ID 并回 pair_result。
    fn confirm_pair(&self, code: &str, accept: bool) -> std::result::Result<(), String> {
        let mut pending = self.pending.lock().unwrap();
        let Some(pair) = pending.remove(code) else {
            return Err("配对码无效或已过期".to_string());
        };
        let Some(ws) = pair.ws else {
            return Err("设备尚未连接".to_string());
        };
        if accept {
            let token = random_token(16);
            let device_id = format!("apk-{}", &random_token(4)[..8]);
            self.devices.lock().unwrap().insert(
                token.clone(),
                DirectDevice {
                    device_id: device_id.clone(),
                    token: token.clone(),
                },
            );
            let _ = ws.send(ws_text(
                json!({
                    "type": "pair_result",
                    "accepted": true,
                    "token": token,
                    "device_id": device_id,
                    "desktop_id": "desktop",
                })
                .to_string(),
            ));
        } else {
            let _ = ws.send(ws_text(
                json!({"type": "pair_result", "accepted": false}).to_string(),
            ));
        }
        Ok(())
    }

    fn authenticate_token(&self, token: &str) -> Option<DirectDevice> {
        self.devices.lock().unwrap().get(token).cloned()
    }

    fn register_online(&self, device_id: &str, ws: mpsc::UnboundedSender<WsMessage>) {
        self.online.lock().unwrap().insert(device_id.to_string(), ws);
    }

    fn remove_online(&self, device_id: &str) {
        self.online.lock().unwrap().remove(device_id);
    }
}

/// 直连配对码：8 位字母数字，避开易混淆的 0/O/1/I。
fn direct_pair_code() -> String {
    const ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
    let mut bytes = [0u8; 8];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|byte| ALPHABET[(*byte as usize) % ALPHABET.len()] as char)
        .collect()
}

/// axum 0.8 的 ws 文本消息（Utf8Bytes 包装）。
fn ws_text(value: String) -> WsMessage {
    WsMessage::Text(axum::extract::ws::Utf8Bytes::from(value))
}

/// 局域网直连 WebSocket 端点：APK 扫码后优先直连（同 WiFi），失败自动切公网中继。
/// 协议为 relay-server 兼容子集：auth(code/token) → pair_pending → 面板确认 → pair_result → 消息。
async fn pair_ws(
    State(state): State<WebState>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| handle_pair_ws(state, socket))
}

async fn handle_pair_ws(state: WebState, socket: WebSocket) {
    let hub = state.direct.clone();
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<WsMessage>();
    let mut send_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if ws_sender.send(message).await.is_err() {
                break;
            }
        }
    });
    let mut device_id: Option<String> = None;
    while let Some(result) = ws_receiver.next().await {
        let Ok(message) = result else {
            break;
        };
        let Ok(text) = message.to_text() else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "auth" => {
                if let Some(code) = value.get("code").and_then(Value::as_str) {
                    // 配对码模式：登记并推送面板确认
                    match hub.attach_pair_code(code, tx.clone()) {
                        Ok(label) => {
                            state.events.publish(
                                "pairing.request",
                                json!({
                                    "code": code,
                                    "apk_label": label,
                                    "apk_device_id": "",
                                    "direct": true,
                                }),
                            );
                            let _ = tx.send(ws_text(
                                json!({"type": "pair_pending", "expires_in": 300}).to_string(),
                            ));
                        }
                        Err(reason) => {
                            let _ = tx.send(ws_text(
                                json!({"type": "auth_error", "code": 4003, "message": reason})
                                    .to_string(),
                            ));
                        }
                    }
                } else if let Some(token) = value.get("token").and_then(Value::as_str) {
                    // 已配对 token 重连
                    if let Some(device) = hub.authenticate_token(token) {
                        device_id = Some(device.device_id.clone());
                        hub.register_online(&device.device_id, tx.clone());
                        let _ = tx.send(ws_text(
                            json!({"type": "auth_ok", "device_id": device.device_id}).to_string(),
                        ));
                    } else {
                        let _ = tx.send(ws_text(
                            json!({"type": "auth_error", "code": 4004, "message": "token 无效"})
                                .to_string(),
                        ));
                    }
                }
            }
            "ping" => {
                let _ = tx.send(ws_text(json!({"type": "pong"}).to_string()));
            }
            "message" => {
                let Some(device_id) = device_id.as_deref() else {
                    let _ = tx.send(ws_text(
                        json!({"type": "auth_error", "code": 4001, "message": "未认证"})
                            .to_string(),
                    ));
                    continue;
                };
                let msg_id = value
                    .get("msg_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let body = value.get("body").cloned().unwrap_or(json!({}));
                let reply = handle_direct_message(&state, device_id, &body).await;
                for part in split_reply_chunks(&reply, 4000) {
                    let _ = tx.send(ws_text(
                        json!({
                            "type": "message",
                            "to": device_id,
                            "msg_id": msg_id,
                            "body": {"reply": part},
                        })
                        .to_string(),
                    ));
                }
            }
            _ => {}
        }
    }
    if let Some(device_id) = device_id {
        hub.remove_online(&device_id);
    }
    drop(tx);
    let _ = send_task.await;
}

/// 长回复分片（避免切断 emoji 代理对；与 bridge-common.cjs splitReply 同语义）。
fn split_reply_chunks(reply: &str, max_chars: usize) -> Vec<String> {
    if reply.chars().count() <= max_chars {
        return vec![reply.to_string()];
    }
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut current_count = 0usize;
    for character in reply.chars() {
        current.push(character);
        current_count += 1;
        if current_count >= max_chars {
            parts.push(std::mem::take(&mut current));
            current_count = 0;
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// 中继 REST 基地址（wss://host/ws → https://host）。
fn relay_http_base(relay_url: &str) -> std::result::Result<String, ApiError> {
    let url = relay_url.trim();
    if url.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "relay_url 未配置：先 `hilia relay config relay_url wss://你的中继/ws`",
        ));
    }
    let rest = url
        .strip_prefix("wss://")
        .or_else(|| url.strip_prefix("ws://"))
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "relay_url 必须以 wss:// 或 ws:// 开头"))?;
    let scheme = if url.starts_with("wss://") { "https" } else { "http" };
    Ok(format!("{scheme}://{}", rest.trim_end_matches('/').trim_end_matches("/ws")))
}

/// 生成配对二维码数据：调中继注册桌面配对码。
async fn pairing_create(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    // 1. 直连配对码（本地生成，无需中继；同 WiFi 下 APK 优先直连）
    let dcode = state.direct.create_pair_code();
    let direct_ws = local_ip_address::local_ip()
        .ok()
        .map(|ip| format!("ws://{ip}:{}/pair/ws", state.listen_port));
    let mut qr = format!(
        "hilia://pair?direct={}&dcode={dcode}",
        direct_ws.as_deref().unwrap_or("")
    );
    // 2. 公网中继配对码（可选：未配置中继时仅直连可用，异地无法连接）
    let mut relay_url = String::new();
    let mut code = String::new();
    let mut expires_in = 300i64;
    let bridges = crate::bridges::load(&state.paths).map_err(ApiError::internal)?;
    if let Some(relay) = bridges.relay.as_ref() {
        let base = relay_http_base(&relay.relay_url)?;
        relay_url = relay.relay_url.clone();
        let relay_result: std::result::Result<Value, String> = tokio::task::spawn_blocking({
            let base = base.clone();
            move || -> std::result::Result<Value, String> {
                let client = reqwest::blocking::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(10))
                    .timeout(std::time::Duration::from_secs(20))
                    .build()
                    .map_err(|error| error.to_string())?;
                let hostname =
                    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows 桌面".to_string());
                let response = client
                    .post(format!("{base}/pairing/register"))
                    .json(&json!({ "label": hostname }))
                    .send()
                    .map_err(|error| format!("中继连接失败：{error}"))?;
                if !response.status().is_success() {
                    return Err(format!("中继注册失败：HTTP {}", response.status()));
                }
                response.json().map_err(|error| error.to_string())
            }
        })
        .await
        .map_err(ApiError::internal)?;
        match relay_result {
            Ok(body) => {
                code = body
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                expires_in = body.get("expires_in").and_then(Value::as_i64).unwrap_or(300);
                if !code.is_empty() {
                    qr.push_str(&format!("&relay={relay_url}&code={code}"));
                }
            }
            Err(_message) => {
                // 中继不可达：二维码仍可直连使用
                relay_url.clear();
            }
        }
    }
    Ok(Json(json!({
        "ok": true,
        "code": code,
        "dcode": dcode,
        "relay_url": relay_url,
        "direct_ws": direct_ws,
        "expires_in": expires_in,
        // 二维码内容（APK 扫码解析；含 relay 段时异地可连接）
        "qr": qr,
    })))
}

/// 辅导员确认/拒绝直连配对（APK 直连本机 /pair/ws 时由面板弹窗触发）。
async fn pairing_confirm_direct(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let code = body
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if code.is_empty() {
        return Err(ApiError::bad_request("code required"));
    }
    let accept = body.get("accept").and_then(Value::as_bool).unwrap_or(false);
    state
        .direct
        .confirm_pair(&code, accept)
        .map_err(ApiError::bad_request)?;
    Ok(Json(json!({ "ok": true })))
}

/// 中继路径结构化消息统一入口：bridge.cjs 收到 APK 的 body.kind 消息时转发到这里，
/// 与直连路径（/pair/ws 内直接处理）共用同一套逻辑。
async fn mobile_dispatch(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let device_id = body
        .get("device_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if device_id.is_empty() {
        return Err(ApiError::bad_request("device_id required"));
    }
    let payload = body.get("body").cloned().unwrap_or(json!({}));
    let reply = handle_direct_message(&state, &device_id, &payload).await;
    Ok(Json(json!({ "ok": true, "reply": reply })))
}

/// APK 消息统一入口：结构化消息（kind=credit_apply 等）直接处理；
/// 文本消息走 hilia ask（与 bridge.cjs 同一会话隔离与身份注入）。
async fn handle_direct_message(state: &WebState, device_id: &str, body: &Value) -> String {
    let kind = body.get("kind").and_then(Value::as_str).unwrap_or("text");
    match kind {
        "text" | "question" => run_hilia_ask(state, device_id, body).await,
        "admin_auth" => handle_admin_auth(state, device_id, body).await,
        "credit_types" => handle_mobile_credit_types(state).await,
        "credit_my" => handle_mobile_credit_my(state, device_id).await,
        "credit_apply" => handle_mobile_credit_apply(state, device_id, body).await,
        "credit_submissions" => handle_mobile_submissions(state, device_id, body).await,
        "credit_approve" => handle_mobile_review(state, device_id, body, true).await,
        "credit_reject" => handle_mobile_review(state, device_id, body, false).await,
        "credit_summary" => handle_mobile_credit_summary(state, device_id, body).await,
        _ => format!(r#"{{"ok":false,"error":"未知消息类型：{kind}"}}"#),
    }
}

/// APK 文本消息 → hilia ask（与 bridge.cjs 相同的会话目录 / 身份 / 超时语义）。
async fn run_hilia_ask(state: &WebState, device_id: &str, body: &Value) -> String {
    let text = body
        .get("text")
        .or_else(|| body.get("question"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return r#"{"ok":false,"error":"消息为空"}"#.to_string();
    }
    let session_home = state
        .paths
        .data_dir
        .join("sessions")
        .join(format!("relay-apk-{device_id}"));
    let extra_env = ensure_apk_session_env(&state.paths, &session_home);
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("hilia"));
    let mut command = tokio::process::Command::new(exe);
    command
        .args([
            "--stdout",
            "--bridge-platform",
            "apk",
            "--bridge-user-id",
            device_id,
            "--bridge-chat-id",
            device_id,
            "ask",
            &text,
        ])
        .env("NO_COLOR", "1")
        .env("HILIA_HOME", &session_home)
        .envs(&extra_env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(Duration::from_secs(120), command.output()).await;
    match output {
        Ok(Ok(output)) => {
            if output.status.success() {
                let reply = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if reply.is_empty() {
                    "(我没想出该说啥)".to_string()
                } else {
                    reply
                }
            } else {
                let mut detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
                if detail.is_empty() {
                    detail = String::from_utf8_lossy(&output.stdout).trim().to_string();
                }
                let detail: String = detail.chars().take(200).collect();
                format!("出错了：{detail}")
            }
        }
        _ => "处理超时了（120s），换个问法试试？".to_string(),
    }
}

/// APK 会话目录初始化：首次使用时把主配置复制过去，敏感字段脱敏为 $env 引用
/// （与 bridge-common.cjs ensureSession 同语义，保证 APK 对话跟随导员配置）。
fn ensure_apk_session_env(paths: &GqyPaths, session_home: &FilePath) -> HashMap<String, String> {
    let mut env_map = HashMap::new();
    let cfg_dir = session_home.join("config");
    let cfg_path = cfg_dir.join("config.jsonc");
    if cfg_path.exists() {
        return env_map;
    }
    let Ok(main_text) = std::fs::read_to_string(&paths.config_file) else {
        return env_map;
    };
    let stripped = json_comments::StripComments::new(main_text.as_bytes());
    let mut value: Value = match serde_json::from_reader(stripped) {
        Ok(value) => value,
        Err(_) => return env_map,
    };
    let mut counter = 0usize;
    redact_config_secrets(&mut value, &mut env_map, &mut counter);
    let Ok(serialized) = serde_json::to_string_pretty(&value) else {
        return env_map;
    };
    if std::fs::create_dir_all(&cfg_dir).is_err() {
        return env_map;
    }
    let _ = std::fs::write(&cfg_path, serialized);
    env_map
}

fn redact_config_secrets(
    value: &mut Value,
    env_map: &mut HashMap<String, String>,
    counter: &mut usize,
) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if is_secret_key(key) && child.is_string() {
                    *counter += 1;
                    let env_name = format!("HILIA_CFG_{counter}");
                    if let Some(raw) = child.as_str() {
                        env_map.insert(env_name.clone(), raw.to_string());
                        *child = Value::String(format!("$env:{env_name}"));
                    }
                } else {
                    redact_config_secrets(child, env_map, counter);
                }
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                redact_config_secrets(item, env_map, counter);
            }
        }
        _ => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("token")
        || lower.contains("password")
        || lower.contains("secret")
        || lower.contains("credential")
}

/// 辅导员身份确认（APK 输管理员激活码 → 本地验签 → 登记为辅导员设备）。
async fn handle_admin_auth(state: &WebState, device_id: &str, body: &Value) -> String {
    let code = body
        .get("activation_code")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if code.is_empty() {
        return r#"{"ok":false,"error":"激活码不能为空"}"#.to_string();
    }
    // 与 hilia license activate 相同的验签逻辑（plan 需含 admin）
    let payload = match crate::license::verify_activation_code(&code) {
        Ok(payload) => payload,
        Err(_) => return r#"{"ok":false,"error":"激活码无效或已过期"}"#.to_string(),
    };
    let is_admin_plan = payload.plan.contains("admin") || payload.plan.contains("pro");
    if !is_admin_plan {
        return r#"{"ok":false,"error":"该激活码不是管理员（辅导员）授权"}"#.to_string();
    }
    let expires_at = if payload.expires_at <= 0 {
        String::new()
    } else {
        match chrono::DateTime::from_timestamp(payload.expires_at, 0) {
            Some(time) => time.to_rfc3339(),
            None => String::new(),
        }
    };
    let paths = state.paths.clone();
    let device_id = device_id.to_string();
    let plan = payload.plan.clone();
    let user = payload.user.clone();
    let expires_at_clone = expires_at.clone();
    let result: anyhow::Result<()> = tokio::task::spawn_blocking(move || {
        let db = crate::state::CreditsDb::open(&paths.data_dir)?;
        db.confirm_admin_device(&device_id, &plan, &user, &expires_at_clone)?;
        Ok(())
    })
    .await
    .map_err(|error| anyhow::anyhow!(error))
    .and_then(|result| result);
    match result {
        Ok(()) => json!({
            "ok": true,
            "admin": true,
            "plan": payload.plan,
            "user": payload.user,
            "expires_at": expires_at,
        })
        .to_string(),
        Err(error) => format!(r#"{{"ok":false,"error":"登记失败：{error}"}}"#),
    }
}

/// 学分类型列表（APK 申报表单下拉）。
async fn handle_mobile_credit_types(state: &WebState) -> String {
    let paths = state.paths.clone();
    let result: anyhow::Result<Vec<crate::state::CreditTypeRow>> =
        tokio::task::spawn_blocking(move || {
            let db = crate::state::CreditsDb::open(&paths.data_dir)?;
            db.list_credit_types()
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|result| result);
    match result {
        Ok(types) => json!({
            "ok": true,
            "types": types
                .iter()
                .map(|t| json!({
                    "id": t.id,
                    "name": t.name,
                    "description": t.description,
                    "max_points": t.max_points,
                }))
                .collect::<Vec<_>>(),
        })
        .to_string(),
        Err(error) => format!(r#"{{"ok":false,"error":"{error}"}}"#),
    }
}

/// 我的学分（APK 绑定学号后查询自己的汇总与明细）。
async fn handle_mobile_credit_my(state: &WebState, device_id: &str) -> String {
    let paths = state.paths.clone();
    let device_id = device_id.to_string();
    let result: anyhow::Result<Value> = tokio::task::spawn_blocking(move || {
        let db = crate::state::CreditsDb::open(&paths.data_dir)?;
        let Some(student) = db.find_student_by_apk(&device_id)? else {
            return Ok(json!({"ok": false, "error": "未绑定学号", "unbound": true}));
        };
        let summary = db.summary(Some(student.id), None)?;
        let records = db.query_credits(Some(student.id), None, None, "", "")?;
        Ok(json!({
            "ok": true,
            "student": json!({"student_no": student.student_no, "name": student.name}),
            "total": summary.total,
            "by_type": summary
                .by_type
                .iter()
                .map(|(name, points)| json!({"type": name, "points": points}))
                .collect::<Vec<_>>(),
            "records": records
                .iter()
                .map(|r| json!({
                    "type_name": r.type_name,
                    "points": r.points,
                    "note": r.note,
                    "created_at": r.created_at,
                }))
                .collect::<Vec<_>>(),
        }))
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|result| result);
    match result {
        Ok(value) => value.to_string(),
        Err(error) => format!(r#"{{"ok":false,"error":"{error}"}}"#),
    }
}

/// 问卷申报（职位人员）：提交学分 + 证据照片（base64 ≤ 3 张）。
async fn handle_mobile_credit_apply(state: &WebState, device_id: &str, body: &Value) -> String {
    let paths = state.paths.clone();
    let device_id = device_id.to_string();
    let type_id = body.get("type_id").and_then(Value::as_i64);
    let points = body.get("points").and_then(Value::as_f64).unwrap_or(0.0);
    let description = body
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let evidence: Vec<Value> = body
        .get("evidence")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let db = crate::state::CreditsDb::open(&paths.data_dir)?;
        let Some(student) = db.find_student_by_apk(&device_id)? else {
            return Ok(r#"{"ok":false,"error":"未绑定学号","unbound":true}"#.to_string());
        };
        // 申报权限：必须有班级职位（班长/学委等）
        if db.find_role_by_student(student.id)?.is_none() {
            return Ok(
                r#"{"ok":false,"error":"只有班级职位人员（如班长）才能填写学分申报"}"#.to_string(),
            );
        }
        let submission_id =
            db.add_submission(student.id, type_id, points, &description, &device_id)?;
        // 证据照片落盘：data_dir/evidence/<submission_id>_<n>.<ext>
        let mut saved = Vec::new();
        if !evidence.is_empty() {
            let evidence_dir = paths.data_dir.join("evidence");
            std::fs::create_dir_all(&evidence_dir)?;
            for (index, item) in evidence.iter().enumerate().take(3) {
                let name = item.get("name").and_then(Value::as_str).unwrap_or("photo");
                let data = item.get("data").and_then(Value::as_str).unwrap_or("");
                if data.is_empty() {
                    continue;
                }
                let bytes = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    data,
                )
                .map_err(|error| anyhow::anyhow!("图片解码失败：{error}"))?;
                let extension = std::path::Path::new(name)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("jpg")
                    .to_ascii_lowercase();
                let file_name = format!("{submission_id}_{}.{extension}", index + 1);
                let file_path = evidence_dir.join(&file_name);
                std::fs::write(&file_path, bytes)?;
                saved.push(json!({"name": name, "file": file_name}));
            }
            db.set_submission_evidence(submission_id, &serde_json::to_string(&saved)?)?;
        }
        Ok(json!({
            "ok": true,
            "submission_id": submission_id,
            "student_no": student.student_no,
            "student_name": student.name,
            "evidence_count": saved.len(),
            "status": "pending",
        })
        .to_string())
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|result| result);
    match result {
        Ok(reply) => reply,
        Err(error) => format!(r#"{{"ok":false,"error":"{error}"}}"#),
    }
}

/// 申报列表（辅导员：按状态/班级过滤）。
async fn handle_mobile_submissions(state: &WebState, device_id: &str, body: &Value) -> String {
    let paths = state.paths.clone();
    let device_id = device_id.to_string();
    let status = body.get("status").and_then(Value::as_str).map(str::to_string);
    let class_id = body.get("class_id").and_then(Value::as_i64);
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let db = crate::state::CreditsDb::open(&paths.data_dir)?;
        if !db.is_admin_device(&device_id)? {
            return Ok(r#"{"ok":false,"error":"仅辅导员可查看申报列表"}"#.to_string());
        }
        let submissions = db.list_submissions(status.as_deref(), class_id, None, None, None)?;
        Ok(serde_json::to_string(&json!({
            "ok": true,
            "submissions": submissions,
        }))?)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|result| result);
    match result {
        Ok(reply) => reply,
        Err(error) => format!(r#"{{"ok":false,"error":"{error}"}}"#),
    }
}

/// 审批/驳回申报（辅导员）。
async fn handle_mobile_review(
    state: &WebState,
    device_id: &str,
    body: &Value,
    approve: bool,
) -> String {
    let paths = state.paths.clone();
    let device_id = device_id.to_string();
    let submission_id = body.get("submission_id").and_then(Value::as_i64);
    let note = body
        .get("review_note")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let db = crate::state::CreditsDb::open(&paths.data_dir)?;
        if !db.is_admin_device(&device_id)? {
            return Ok(r#"{"ok":false,"error":"仅辅导员可审批申报"}"#.to_string());
        }
        let Some(submission_id) = submission_id else {
            return Ok(r#"{"ok":false,"error":"缺少 submission_id"}"#.to_string());
        };
        if approve {
            db.approve_submission(submission_id, &note, &format!("APK 辅导员 {device_id}"))?;
        } else {
            db.reject_submission(submission_id, &note)?;
        }
        Ok(json!({"ok": true}).to_string())
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|result| result);
    match result {
        Ok(reply) => reply,
        Err(error) => format!(r#"{{"ok":false,"error":"{error}"}}"#),
    }
}

/// 申报统计（辅导员：当日/区间汇总，供 AI 总结与面板展示）。
async fn handle_mobile_credit_summary(state: &WebState, device_id: &str, body: &Value) -> String {
    let paths = state.paths.clone();
    let device_id = device_id.to_string();
    let date_from = body
        .get("date_from")
        .and_then(Value::as_str)
        .map(str::to_string);
    let date_to = body.get("date_to").and_then(Value::as_str).map(str::to_string);
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
        let db = crate::state::CreditsDb::open(&paths.data_dir)?;
        if !db.is_admin_device(&device_id)? {
            return Ok(r#"{"ok":false,"error":"仅辅导员可查看申报统计"}"#.to_string());
        }
        let summary = db.submissions_summary(date_from.as_deref(), date_to.as_deref())?;
        let totals: Vec<Value> = summary
            .iter()
            .map(|row| {
                json!({
                    "class_name": row.class_name,
                    "type_name": row.type_name,
                    "pending": row.pending,
                    "approved": row.approved,
                    "rejected": row.rejected,
                    "approved_points": row.total_points,
                })
            })
            .collect();
        Ok(serde_json::to_string(&json!({"ok": true, "summary": totals}))?)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|result| result);
    match result {
        Ok(reply) => reply,
        Err(error) => format!(r#"{{"ok":false,"error":"{error}"}}"#),
    }
}

/// 配对请求通知（由 relay bridge 本地调用）：SSE 推给面板弹确认。
async fn pairing_request(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let code = body.get("code").and_then(Value::as_str).unwrap_or_default().to_string();
    if code.is_empty() {
        return Err(ApiError::bad_request("code required"));
    }
    let apk_label = body.get("apk_label").and_then(Value::as_str).unwrap_or("Android");
    let apk_device_id = body.get("apk_device_id").and_then(Value::as_str).unwrap_or("");
    state.events.publish(
        "pairing.request",
        json!({
            "code": code,
            "apk_label": apk_label,
            "apk_device_id": apk_device_id,
        }),
    );
    Ok(Json(json!({ "ok": true })))
}

/// 辅导员确认/拒绝配对：调中继 confirm。
async fn pairing_confirm(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, ApiError> {
    require_auth(&headers, &state)?;
    let code = body.get("code").and_then(Value::as_str).unwrap_or_default().to_string();
    if code.is_empty() {
        return Err(ApiError::bad_request("code required"));
    }
    let accept = body.get("accept").and_then(Value::as_bool).unwrap_or(false);
    let bridges = crate::bridges::load(&state.paths).map_err(ApiError::internal)?;
    let relay = bridges
        .relay
        .as_ref()
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "中继未配置"))?;
    let base = relay_http_base(&relay.relay_url)?;
    let ok = tokio::task::spawn_blocking(move || -> std::result::Result<bool, String> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|error| error.to_string())?;
        let response = client
            .post(format!("{base}/pairing/confirm"))
            .json(&json!({ "code": code, "accept": accept }))
            .send()
            .map_err(|error| format!("中继连接失败：{error}"))?;
        if !response.status().is_success() {
            return Err(format!("中继确认失败：HTTP {}", response.status()));
        }
        Ok(true)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(|message| ApiError::new(StatusCode::BAD_GATEWAY, message))?;
    Ok(Json(json!({ "ok": ok, "accepted": accept })))
}
