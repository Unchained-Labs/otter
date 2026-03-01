use std::convert::Infallible;
use std::fs;
use std::path::{Path as FsPath, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Multipart, Path, Query, State};
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use futures_util::{SinkExt, StreamExt};
use otter_core::config::AppConfig;
use otter_core::db::Database;
use otter_core::domain::{
    CreateProjectRequest, CreateWorkspaceRequest, EnqueuePromptRequest, Job, JobEvent, JobOutput,
    QueueItem, UpdateQueuePositionRequest, Workspace,
};
use otter_core::queue::RedisQueue;
use otter_core::runtime::docker_manager::split_stdout_and_cwd;
use otter_core::service::OtterService;
use tokio::process::Command;
use tokio::time::Duration as TokioDuration;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::{info, warn, Level};
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    service: Arc<OtterService<RedisQueue>>,
    lavoix_url: String,
    shell_sessions: Arc<tokio::sync::Mutex<std::collections::HashMap<String, String>>>,
}

#[derive(serde::Deserialize)]
struct HistoryQuery {
    limit: Option<i64>,
}

#[derive(serde::Deserialize)]
struct QueueQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(serde::Deserialize)]
struct WorkspaceTreeQuery {
    path: Option<String>,
    depth: Option<usize>,
}

#[derive(serde::Deserialize)]
struct WorkspaceFileQuery {
    path: String,
}

#[derive(serde::Deserialize)]
struct WorkspaceCommandRequest {
    command: String,
    working_directory: Option<String>,
    shell_session_id: Option<String>,
    timeout_seconds: Option<u64>,
}

#[derive(serde::Deserialize)]
struct WorkspaceCommandDispatchRequest {
    workspace_id: Option<Uuid>,
    command: String,
    working_directory: Option<String>,
    shell_session_id: Option<String>,
    timeout_seconds: Option<u64>,
}

#[derive(serde::Serialize)]
struct WorkspaceTreeEntryResponse {
    name: String,
    relative_path: String,
    kind: String,
    size_bytes: Option<u64>,
}

#[derive(serde::Serialize)]
struct WorkspaceTreeResponse {
    workspace_id: Uuid,
    root_path: String,
    base_path: String,
    entries: Vec<WorkspaceTreeEntryResponse>,
}

#[derive(serde::Serialize)]
struct WorkspaceFileResponse {
    workspace_id: Uuid,
    relative_path: String,
    content: String,
    truncated: bool,
}

#[derive(serde::Serialize)]
struct WorkspaceCommandResponse {
    workspace_id: Uuid,
    command: String,
    working_directory: String,
    shell_session_id: Option<String>,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

#[derive(serde::Deserialize)]
struct RuntimeLogsQuery {
    tail: Option<usize>,
}

#[derive(serde::Serialize)]
struct RuntimeLogsResponse {
    workspace_id: Uuid,
    logs: String,
}

#[derive(serde::Deserialize)]
struct ShellWsClientMessage {
    command: String,
    shell_session_id: Option<String>,
}

#[derive(serde::Serialize)]
struct ShellWsServerMessage {
    event: String,
    command: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
    exit_code: Option<i64>,
    working_directory: Option<String>,
    error: Option<String>,
}

#[derive(serde::Serialize)]
struct JobResponse {
    job: Job,
    output: Option<JobOutput>,
    queue_rank: Option<i64>,
}

#[derive(serde::Deserialize)]
struct LavoixTranscriptionResponse {
    text: String,
}

#[derive(serde::Serialize)]
struct VoiceEnqueueResponse {
    transcript: String,
    job: Job,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = AppConfig::from_env()?;
    let database = connect_database_with_retry(&config.database_url).await?;
    migrate_with_retry(database.clone()).await?;
    let queue = connect_redis_with_retry(&config.redis_url).await?;
    let service = Arc::new(OtterService::new(&config, database, queue));
    let state = AppState {
        service,
        lavoix_url: config.lavoix_url.clone(),
        shell_sessions: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
    };

    let allow_any_origin = config
        .cors_allowed_origins
        .iter()
        .any(|origin| origin == "*");
    let cors = if allow_any_origin {
        CorsLayer::new().allow_origin(Any)
    } else {
        let allowed_origins = config
            .cors_allowed_origins
            .iter()
            .map(|origin| origin.parse::<HeaderValue>())
            .collect::<std::result::Result<Vec<_>, _>>()?;
        CorsLayer::new().allow_origin(AllowOrigin::list(allowed_origins))
    }
    .allow_methods([
        axum::http::Method::GET,
        axum::http::Method::POST,
        axum::http::Method::PATCH,
        axum::http::Method::OPTIONS,
    ])
    .allow_headers(tower_http::cors::Any);

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/projects", post(create_project).get(list_projects))
        .route(
            "/v1/workspaces",
            post(create_workspace).get(list_workspaces),
        )
        .route("/v1/workspaces/{id}/tree", get(get_workspace_tree))
        .route("/v1/workspaces/{id}/file", get(get_workspace_file))
        .route("/v1/workspaces/{id}/command", post(run_workspace_command))
        .route("/v1/workspaces/command", post(run_workspace_command_any))
        .route(
            "/v1/runtime/workspaces/{id}",
            get(get_runtime_workspace_status),
        )
        .route(
            "/v1/runtime/workspaces/{id}/start",
            post(start_runtime_workspace_container),
        )
        .route(
            "/v1/runtime/workspaces/{id}/stop",
            post(stop_runtime_workspace_container),
        )
        .route(
            "/v1/runtime/workspaces/{id}/restart",
            post(restart_runtime_workspace_container),
        )
        .route(
            "/v1/runtime/workspaces/{id}/logs",
            get(get_runtime_workspace_logs),
        )
        .route(
            "/v1/runtime/workspaces/{id}/shell/ws",
            get(runtime_workspace_shell_ws),
        )
        .route("/v1/prompts", post(enqueue_prompt))
        .route("/v1/voice/prompts", post(enqueue_voice_prompt))
        .route("/v1/jobs/{id}", get(get_job))
        .route("/v1/jobs/{id}/events", get(get_job_events))
        .route("/v1/events/stream", get(stream_job_events))
        .route("/v1/jobs/{id}/cancel", post(cancel_job))
        .route("/v1/queue", get(get_queue))
        .route(
            "/v1/queue/{id}",
            axum::routing::patch(update_queue_position),
        )
        .route("/v1/history", get(get_history))
        .with_state(state)
        .layer(TraceLayer::new_for_http().on_response(DefaultOnResponse::new().level(Level::INFO)))
        .layer(cors);

    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    info!("otter-server listening on {}", config.listen_addr);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn connect_database_with_retry(database_url: &str) -> Result<Arc<Database>> {
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        match Database::connect(database_url).await {
            Ok(database) => {
                info!(attempts, "connected to postgres");
                return Ok(Arc::new(database));
            }
            Err(error) if attempts < 30 => {
                warn!(attempts, error = %error, "postgres connect failed; retrying");
                tokio::time::sleep(TokioDuration::from_secs(2)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn migrate_with_retry(database: Arc<Database>) -> Result<()> {
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        match database.migrate().await {
            Ok(()) => {
                info!(attempts, "database migrations applied");
                return Ok(());
            }
            Err(error) if attempts < 20 => {
                warn!(attempts, error = %error, "database migration failed; retrying");
                tokio::time::sleep(TokioDuration::from_secs(2)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn connect_redis_with_retry(redis_url: &str) -> Result<Arc<RedisQueue>> {
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        match RedisQueue::connect(redis_url).await {
            Ok(queue) => {
                info!(attempts, "connected to redis");
                return Ok(Arc::new(queue));
            }
            Err(error) if attempts < 30 => {
                warn!(attempts, error = %error, "redis connect failed; retrying");
                tokio::time::sleep(TokioDuration::from_secs(2)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn create_project(
    State(state): State<AppState>,
    Json(body): Json<CreateProjectRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .service
        .create_project(body)
        .await
        .map(|item| (StatusCode::CREATED, Json(item)))
        .map_err(internal_error)
}

async fn list_projects(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .service
        .list_projects()
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn create_workspace(
    State(state): State<AppState>,
    Json(body): Json<CreateWorkspaceRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .service
        .create_workspace(body)
        .await
        .map(|item| (StatusCode::CREATED, Json(item)))
        .map_err(internal_error)
}

async fn list_workspaces(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .service
        .list_workspaces()
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn get_workspace_tree(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<WorkspaceTreeQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let workspace = state
        .service
        .db
        .fetch_workspace(id)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "workspace not found".to_string()))?;
    let depth = query.depth.unwrap_or(2).min(5);
    let base = resolve_workspace_subpath(&workspace, query.path.as_deref().unwrap_or(""))?;
    let root = canonicalize_workspace_root(&workspace)?;
    let mut entries = Vec::new();
    list_workspace_entries(&root, &base, depth, &mut entries).map_err(internal_error)?;
    Ok(Json(WorkspaceTreeResponse {
        workspace_id: id,
        root_path: workspace.root_path,
        base_path: query.path.unwrap_or_default(),
        entries,
    }))
}

async fn get_workspace_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<WorkspaceFileQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let workspace = state
        .service
        .db
        .fetch_workspace(id)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "workspace not found".to_string()))?;
    let file_path = resolve_workspace_subpath(&workspace, &query.path)?;
    let metadata = fs::metadata(&file_path).map_err(internal_error)?;
    if metadata.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            "path points to a directory".to_string(),
        ));
    }
    let bytes = fs::read(&file_path).map_err(internal_error)?;
    let max_bytes = 200_000usize;
    let truncated = bytes.len() > max_bytes;
    let content = String::from_utf8_lossy(&bytes[..bytes.len().min(max_bytes)]).to_string();
    Ok(Json(WorkspaceFileResponse {
        workspace_id: id,
        relative_path: query.path,
        content,
        truncated,
    }))
}

async fn run_workspace_command(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<WorkspaceCommandRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    execute_workspace_command(state, Some(id), body)
        .await
        .map(Json)
}

async fn run_workspace_command_any(
    State(state): State<AppState>,
    Json(body): Json<WorkspaceCommandDispatchRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    execute_workspace_command(
        state,
        body.workspace_id,
        WorkspaceCommandRequest {
            command: body.command,
            working_directory: body.working_directory,
            shell_session_id: body.shell_session_id,
            timeout_seconds: body.timeout_seconds,
        },
    )
    .await
    .map(Json)
}

async fn get_runtime_workspace_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .service
        .runtime_status_for_workspace(id)
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn start_runtime_workspace_container(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .service
        .runtime_start_for_workspace(id)
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn stop_runtime_workspace_container(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .service
        .runtime_stop_for_workspace(id)
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn restart_runtime_workspace_container(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .service
        .runtime_restart_for_workspace(id)
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn get_runtime_workspace_logs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<RuntimeLogsQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let tail = query.tail.unwrap_or(500).clamp(1, 10_000);
    let logs = state
        .service
        .runtime_logs_for_workspace(id, tail)
        .await
        .map_err(internal_error)?;
    Ok(Json(RuntimeLogsResponse {
        workspace_id: id,
        logs,
    }))
}

async fn runtime_workspace_shell_ws(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    if !state.service.runtime_enabled() {
        return Err((
            StatusCode::BAD_REQUEST,
            "runtime is not enabled".to_string(),
        ));
    }
    Ok(ws.on_upgrade(move |socket| handle_runtime_shell_ws(socket, state, id)))
}

async fn handle_runtime_shell_ws(mut socket: WebSocket, state: AppState, workspace_id: Uuid) {
    while let Some(message) = socket.next().await {
        let Ok(message) = message else {
            break;
        };
        match message {
            Message::Text(text) => {
                let payload = match serde_json::from_str::<ShellWsClientMessage>(&text) {
                    Ok(payload) => payload,
                    Err(error) => {
                        let _ = socket
                            .send(Message::Text(
                                serde_json::to_string(&ShellWsServerMessage {
                                    event: "error".to_string(),
                                    command: None,
                                    stdout: None,
                                    stderr: None,
                                    exit_code: None,
                                    working_directory: None,
                                    error: Some(format!("invalid payload: {error}")),
                                })
                                .unwrap_or_else(|_| "{\"event\":\"error\"}".to_string()),
                            ))
                            .await;
                        continue;
                    }
                };
                let session_id = payload
                    .shell_session_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("ws");
                let result = state
                    .service
                    .runtime_exec_for_workspace(workspace_id, session_id, &payload.command)
                    .await;
                let response = match result {
                    Ok(out) => ShellWsServerMessage {
                        event: "result".to_string(),
                        command: Some(payload.command),
                        stdout: Some(out.stdout),
                        stderr: Some(out.stderr),
                        exit_code: Some(out.exit_code),
                        working_directory: Some(out.cwd),
                        error: None,
                    },
                    Err(error) => ShellWsServerMessage {
                        event: "error".to_string(),
                        command: Some(payload.command),
                        stdout: None,
                        stderr: None,
                        exit_code: None,
                        working_directory: None,
                        error: Some(error.to_string()),
                    },
                };
                if socket
                    .send(Message::Text(
                        serde_json::to_string(&response)
                            .unwrap_or_else(|_| "{\"event\":\"error\"}".to_string()),
                    ))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Message::Close(_) => break,
            Message::Ping(payload) => {
                if socket.send(Message::Pong(payload)).await.is_err() {
                    break;
                }
            }
            Message::Pong(_) | Message::Binary(_) => {}
        }
    }
}

async fn execute_workspace_command(
    state: AppState,
    workspace_id: Option<Uuid>,
    body: WorkspaceCommandRequest,
) -> Result<WorkspaceCommandResponse, (StatusCode, String)> {
    if body.command.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "command must not be empty".to_string(),
        ));
    }

    let resolved_workspace_id = state
        .service
        .resolve_workspace_id_or_default(workspace_id)
        .await
        .map_err(internal_error)?;

    let workspace = state
        .service
        .db
        .fetch_workspace(resolved_workspace_id)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "workspace not found".to_string()))?;

    let shell_session_id = body
        .shell_session_id
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(120).collect::<String>());
    if state.service.runtime_enabled() {
        let session_id = shell_session_id.as_deref().unwrap_or("workspace-shell");
        let result = state
            .service
            .runtime_exec_for_workspace(resolved_workspace_id, session_id, &body.command)
            .await
            .map_err(internal_error)?;
        return Ok(WorkspaceCommandResponse {
            workspace_id: resolved_workspace_id,
            command: body.command,
            working_directory: result.cwd,
            shell_session_id,
            exit_code: Some(result.exit_code as i32),
            stdout: result.stdout,
            stderr: result.stderr,
            timed_out: false,
        });
    }

    let session_key = shell_session_id
        .as_ref()
        .map(|session_id| format!("{resolved_workspace_id}:{session_id}"));
    let persisted_cwd = if let Some(key) = &session_key {
        state.shell_sessions.lock().await.get(key).cloned()
    } else {
        None
    };
    let requested_working_directory = body
        .working_directory
        .clone()
        .or(persisted_cwd)
        .unwrap_or_default();
    let cwd = resolve_workspace_subpath(&workspace, &requested_working_directory)?;
    let timeout_seconds = body.timeout_seconds.unwrap_or(30).clamp(1, 300);
    let wrapped_command = format!(
        "({command}); __otter_exit=$?; printf '\\n__OTTER_CWD__:%s\\n' \"$PWD\"; exit $__otter_exit",
        command = body.command
    );

    info!(
        workspace_id = %resolved_workspace_id,
        cwd = %cwd.display(),
        timeout_seconds,
        command = %body.command,
        "running workspace shell command"
    );

    let output = Command::new("timeout")
        .arg(format!("{timeout_seconds}s"))
        .arg("bash")
        .arg("-lc")
        .arg(&wrapped_command)
        .current_dir(&cwd)
        .env("VIBE_HOME", &workspace.isolated_vibe_home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(internal_error)?;

    let timed_out = output.status.code() == Some(124);
    if timed_out {
        warn!(
            workspace_id = %resolved_workspace_id,
            cwd = %cwd.display(),
            timeout_seconds,
            command = %body.command,
            "workspace shell command timed out"
        );
    }

    let stdout_with_marker = String::from_utf8_lossy(&output.stdout).to_string();
    let (stdout, resolved_cwd) =
        split_stdout_and_cwd(&stdout_with_marker, &cwd.display().to_string());
    if let Some(key) = session_key.as_ref() {
        state
            .shell_sessions
            .lock()
            .await
            .insert(key.clone(), resolved_cwd.clone());
    }

    Ok(WorkspaceCommandResponse {
        workspace_id: resolved_workspace_id,
        command: body.command,
        working_directory: resolved_cwd,
        shell_session_id,
        exit_code: output.status.code(),
        stdout,
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        timed_out,
    })
}

async fn enqueue_prompt(
    State(state): State<AppState>,
    Json(body): Json<EnqueuePromptRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .service
        .enqueue_prompt(body)
        .await
        .map(|job| (StatusCode::ACCEPTED, Json(job)))
        .map_err(internal_error)
}

async fn enqueue_voice_prompt(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let mut workspace_id: Option<Uuid> = None;
    let mut language: Option<String> = None;
    let mut provider: Option<String> = None;
    let mut file_name = "audio.webm".to_string();
    let mut content_type = "audio/webm".to_string();
    let mut audio_bytes: Option<Vec<u8>> = None;

    while let Some(field) = multipart.next_field().await.map_err(internal_error)? {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "file" | "audio" => {
                if let Some(filename) = field.file_name() {
                    file_name = filename.to_string();
                }
                if let Some(ct) = field.content_type() {
                    content_type = ct.to_string();
                }
                let bytes = field.bytes().await.map_err(internal_error)?;
                audio_bytes = Some(bytes.to_vec());
            }
            "workspace_id" => {
                let value = field.text().await.map_err(internal_error)?;
                if !value.trim().is_empty() {
                    workspace_id = Some(Uuid::parse_str(value.trim()).map_err(|e| {
                        (
                            StatusCode::BAD_REQUEST,
                            format!("invalid workspace_id: {e}"),
                        )
                    })?);
                }
            }
            "language" => {
                let value = field.text().await.map_err(internal_error)?;
                if !value.trim().is_empty() {
                    language = Some(value);
                }
            }
            "provider" => {
                let value = field.text().await.map_err(internal_error)?;
                if !value.trim().is_empty() {
                    provider = Some(value);
                }
            }
            _ => {}
        }
    }

    let audio = audio_bytes.ok_or((
        StatusCode::BAD_REQUEST,
        "multipart payload must include file".to_string(),
    ))?;
    if audio.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "audio payload is empty".to_string(),
        ));
    }

    let transcript = transcribe_with_lavoix(
        &state.lavoix_url,
        audio,
        file_name,
        content_type,
        language,
        provider,
    )
    .await?;
    let prompt = transcript.trim().to_string();
    if prompt.is_empty() {
        return Err((
            StatusCode::BAD_GATEWAY,
            "lavoix returned an empty transcript".to_string(),
        ));
    }

    let job = state
        .service
        .enqueue_prompt(EnqueuePromptRequest {
            workspace_id,
            prompt: prompt.clone(),
            priority: None,
            schedule_at: None,
        })
        .await
        .map_err(internal_error)?;

    Ok((
        StatusCode::ACCEPTED,
        Json(VoiceEnqueueResponse {
            transcript: prompt,
            job,
        }),
    ))
}

async fn transcribe_with_lavoix(
    lavoix_url: &str,
    audio_bytes: Vec<u8>,
    file_name: String,
    content_type: String,
    language: Option<String>,
    provider: Option<String>,
) -> Result<String, (StatusCode, String)> {
    let part = reqwest::multipart::Part::bytes(audio_bytes)
        .file_name(file_name)
        .mime_str(&content_type)
        .map_err(internal_error)?;
    let mut form = reqwest::multipart::Form::new().part("file", part);
    if let Some(language) = language {
        form = form.text("language", language);
    }
    if let Some(provider) = provider {
        form = form.text("provider", provider);
    }

    let url = format!("{}/v1/stt/transcribe", lavoix_url.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .post(url)
        .multipart(form)
        .send()
        .await
        .map_err(|error| {
            (
                StatusCode::BAD_GATEWAY,
                format!("lavoix request failed: {error}"),
            )
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("lavoix returned {status}: {body}"),
        ));
    }

    let payload = response
        .json::<LavoixTranscriptionResponse>()
        .await
        .map_err(|error| {
            (
                StatusCode::BAD_GATEWAY,
                format!("invalid lavoix response: {error}"),
            )
        })?;
    Ok(payload.text)
}

async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let Some(job) = state.service.fetch_job(id).await.map_err(internal_error)? else {
        return Err((StatusCode::NOT_FOUND, "job not found".into()));
    };
    let output = state
        .service
        .fetch_job_output(id)
        .await
        .map_err(internal_error)?;
    let queue_rank = state
        .service
        .queue_rank_for_job(id)
        .await
        .map_err(internal_error)?;
    Ok(Json(JobResponse {
        job,
        output,
        queue_rank,
    }))
}

async fn get_job_events(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let events: Vec<JobEvent> = state
        .service
        .fetch_job_events(id)
        .await
        .map_err(internal_error)?;
    Ok(Json(events))
}

async fn cancel_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let cancelled = state.service.cancel_job(id).await.map_err(internal_error)?;
    if !cancelled {
        return Err((StatusCode::NOT_FOUND, "job not cancellable".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn get_history(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    let history = state
        .service
        .db
        .list_history(limit)
        .await
        .map_err(internal_error)?;
    Ok(Json(history))
}

async fn get_queue(
    State(state): State<AppState>,
    Query(query): Query<QueueQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    let offset = query.offset.unwrap_or(0).max(0);
    let items: Vec<QueueItem> = state
        .service
        .list_queue(limit, offset)
        .await
        .map_err(internal_error)?;
    Ok(Json(items))
}

async fn stream_job_events(
    State(state): State<AppState>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        let mut since = Utc::now() - Duration::seconds(5);
        loop {
            let events = state
                .service
                .fetch_job_events_since(since, 200)
                .await
                .unwrap_or_default();

            for event in events {
                since = event.created_at;
                let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
                yield Ok(Event::default().event(event.event_type.clone()).data(payload));
            }

            tokio::time::sleep(TokioDuration::from_secs(1)).await;
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::new().interval(TokioDuration::from_secs(10)))
}

async fn update_queue_position(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateQueuePositionRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let updated = state
        .service
        .update_queue_position(id, body)
        .await
        .map_err(internal_error)?;
    if !updated {
        return Err((StatusCode::NOT_FOUND, "queued job not found".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

fn canonicalize_workspace_root(workspace: &Workspace) -> Result<PathBuf, (StatusCode, String)> {
    let root = fs::canonicalize(&workspace.root_path)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(root)
}

fn resolve_workspace_subpath(
    workspace: &Workspace,
    relative_path: &str,
) -> Result<PathBuf, (StatusCode, String)> {
    let root = canonicalize_workspace_root(workspace)?;
    if relative_path.is_empty() {
        return Ok(root);
    }
    let candidate = root.join(relative_path);
    let canonical = fs::canonicalize(&candidate)
        .map_err(|_| (StatusCode::NOT_FOUND, "path not found".to_string()))?;
    if !canonical.starts_with(&root) {
        return Err((
            StatusCode::BAD_REQUEST,
            "path escapes workspace root".to_string(),
        ));
    }
    Ok(canonical)
}

fn list_workspace_entries(
    root: &FsPath,
    current: &FsPath,
    depth: usize,
    entries: &mut Vec<WorkspaceTreeEntryResponse>,
) -> Result<()> {
    if depth == 0 {
        return Ok(());
    }
    let dir = fs::read_dir(current)?;
    let mut children = dir.filter_map(|entry| entry.ok()).collect::<Vec<_>>();
    children.sort_by_key(|entry| entry.path());
    for child in children {
        let path = child.path();
        let metadata = child.metadata()?;
        let kind = if metadata.is_dir() {
            "directory"
        } else {
            "file"
        };
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path.as_path())
            .to_string_lossy()
            .to_string();
        entries.push(WorkspaceTreeEntryResponse {
            name: child.file_name().to_string_lossy().to_string(),
            relative_path: relative.clone(),
            kind: kind.to_string(),
            size_bytes: if metadata.is_file() {
                Some(metadata.len())
            } else {
                None
            },
        });
        if metadata.is_dir() {
            list_workspace_entries(root, &path, depth - 1, entries)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::split_stdout_and_cwd;

    #[test]
    fn strips_cwd_marker_from_stdout() {
        let stdout = "hello\n__OTTER_CWD__:/tmp/demo\n";
        let (cleaned, cwd) = split_stdout_and_cwd(stdout, "/fallback");
        assert_eq!(cleaned, "hello\n");
        assert_eq!(cwd, "/tmp/demo");
    }
}
