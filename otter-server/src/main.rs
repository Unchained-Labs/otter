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
use futures_util::StreamExt;
use otter_core::config::AppConfig;
use otter_core::db::Database;
use otter_core::domain::{
    CreateProjectRequest, CreateWorkspaceRequest, EnqueuePromptRequest, Job, JobEvent, JobOutput,
    QueueItem, RuntimeAppRegistryResponse, UpdateQueuePositionRequest, Workspace,
    WORKSPACE_ROOT_MARKER_FILE,
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
    dependency_job_ids: Vec<Uuid>,
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

#[derive(serde::Deserialize)]
struct SetJobPreviewUrlRequest {
    preview_url: String,
}

#[derive(serde::Deserialize)]
struct SetJobProjectPathRequest {
    project_path: String,
}

#[derive(serde::Deserialize)]
struct SetJobDependenciesRequest {
    dependency_job_ids: Vec<Uuid>,
}

fn validate_dependency_job_ids(ids: &[Uuid]) -> Result<(), (StatusCode, String)> {
    if ids.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "dependency_job_ids must not be empty".to_string(),
        ));
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct SetJobRuntimeLaunchRequest {
    start_command: String,
    stop_command: Option<String>,
    working_directory: Option<String>,
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
        .route("/v1/runtime/app-registry", get(get_runtime_app_registry))
        .route(
            "/v1/runtime/apps/shutdown-all",
            post(shutdown_all_runtime_apps),
        )
        .route(
            "/v1/runtime/workspaces/{id}/shell/ws",
            get(runtime_workspace_shell_ws),
        )
        .route("/v1/prompts", post(enqueue_prompt))
        .route("/v1/voice/prompts", post(enqueue_voice_prompt))
        .route("/v1/jobs/{id}", get(get_job))
        .route("/v1/jobs/{id}/events", get(get_job_events))
        .route("/v1/jobs/{id}/preview-url", post(set_job_preview_url))
        .route("/v1/jobs/{id}/project-path", post(set_job_project_path))
        .route("/v1/jobs/{id}/dependencies", post(set_job_dependencies))
        .route("/v1/jobs/{id}/runtime-launch", post(set_job_runtime_launch))
        .route(
            "/v1/jobs/{id}/runtime-launch/start",
            post(start_job_runtime_launch),
        )
        .route(
            "/v1/jobs/{id}/runtime-launch/stop",
            post(stop_job_runtime_launch),
        )
        .route("/v1/jobs/{id}/pause", post(pause_job))
        .route("/v1/jobs/{id}/hold", post(hold_job))
        .route("/v1/jobs/{id}/resume", post(resume_job))
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
                                .unwrap_or_else(|_| "{\"event\":\"error\"}".to_string())
                                .into(),
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
                            .unwrap_or_else(|_| "{\"event\":\"error\"}".to_string())
                            .into(),
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
        match state
            .service
            .runtime_exec_for_workspace(resolved_workspace_id, session_id, &body.command)
            .await
        {
            Ok(result) => {
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
            Err(error) => {
                warn!(
                    workspace_id = %resolved_workspace_id,
                    command = %body.command,
                    error = %error,
                    "runtime command failed; falling back to host workspace shell"
                );
            }
        }
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
    let mut dependency_job_ids: Vec<Uuid> = Vec::new();
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
            "dependency_job_id" => {
                let value = field.text().await.map_err(internal_error)?;
                if !value.trim().is_empty() {
                    dependency_job_ids.push(Uuid::parse_str(value.trim()).map_err(|e| {
                        (
                            StatusCode::BAD_REQUEST,
                            format!("invalid dependency_job_id: {e}"),
                        )
                    })?);
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
            project_path: None,
            dependency_job_ids: if dependency_job_ids.is_empty() {
                None
            } else {
                Some(dependency_job_ids)
            },
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
        .timeout(std::time::Duration::from_secs(45))
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
    let dependency_job_ids = state
        .service
        .db
        .list_job_dependency_ids(id)
        .await
        .map_err(internal_error)?;
    Ok(Json(JobResponse {
        job,
        output,
        queue_rank,
        dependency_job_ids,
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

async fn pause_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let paused = state.service.pause_job(id).await.map_err(internal_error)?;
    if !paused {
        return Err((StatusCode::NOT_FOUND, "queued job not found".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn hold_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let updated = state.service.hold_job(id).await.map_err(internal_error)?;
    if !updated {
        return Err((StatusCode::NOT_FOUND, "queued/running job not found".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn resume_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let resumed = state.service.resume_job(id).await.map_err(internal_error)?;
    if !resumed {
        return Err((StatusCode::NOT_FOUND, "paused queued job not found".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn set_job_project_path(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetJobProjectPathRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let project_path = normalize_relative_workspace_path(body.project_path.as_str())?;

    if project_path.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "project_path must point to a self-contained project folder".to_string(),
        ));
    }

    let job = state
        .service
        .db
        .fetch_job(id)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "job not found".to_string()))?;
    let workspace = state
        .service
        .db
        .fetch_workspace(job.workspace_id)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "workspace not found".to_string()))?;

    validate_self_contained_compose_dir(&workspace, &project_path)?;

    let updated = state
        .service
        .db
        .set_job_project_path(id, &project_path)
        .await
        .map_err(internal_error)?;
    if !updated {
        return Err((StatusCode::NOT_FOUND, "job not found".to_string()));
    }
    state
        .service
        .db
        .insert_job_event(
            id,
            "project_path_set",
            serde_json::json!({ "project_path": project_path }),
        )
        .await
        .map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_job_dependencies(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetJobDependenciesRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    validate_dependency_job_ids(&body.dependency_job_ids)?;

    state
        .service
        .db
        .add_job_dependencies(id, &body.dependency_job_ids)
        .await
        .map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid job dependencies: {error}"),
            )
        })?;

    state
        .service
        .db
        .insert_job_event(
            id,
            "dependencies_set",
            serde_json::json!({ "dependency_job_ids": body.dependency_job_ids }),
        )
        .await
        .map_err(internal_error)?;

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod dependency_tests {
    use super::*;

    #[test]
    fn validate_dependency_job_ids_rejects_empty() {
        let err = validate_dependency_job_ids(&[]).unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_dependency_job_ids_accepts_non_empty() {
        let id = Uuid::new_v4();
        validate_dependency_job_ids(&[id]).unwrap();
    }
}

async fn set_job_runtime_launch(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetJobRuntimeLaunchRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let start_command = body.start_command.trim();
    let working_directory_was_provided = body.working_directory.is_some();
    if start_command.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "start_command must not be empty".to_string(),
        ));
    }
    let stop_command = body
        .stop_command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let working_directory = body
        .working_directory
        .as_deref()
        .map(normalize_relative_workspace_path)
        .transpose()?;

    if working_directory_was_provided {
        let working_directory_value = working_directory.as_deref().unwrap_or("");
        if working_directory_value.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "working_directory must point to a self-contained project folder".to_string(),
            ));
        }

        let job = state
            .service
            .db
            .fetch_job(id)
            .await
            .map_err(internal_error)?
            .ok_or((StatusCode::NOT_FOUND, "job not found".to_string()))?;
        let workspace = state
            .service
            .db
            .fetch_workspace(job.workspace_id)
            .await
            .map_err(internal_error)?
            .ok_or((StatusCode::NOT_FOUND, "workspace not found".to_string()))?;

        validate_self_contained_compose_dir(&workspace, working_directory_value)?;
    }

    let updated = state
        .service
        .db
        .set_job_runtime_launch_config(
            id,
            start_command,
            stop_command,
            working_directory.as_deref(),
        )
        .await
        .map_err(internal_error)?;
    if !updated {
        return Err((StatusCode::NOT_FOUND, "job not found".to_string()));
    }
    state
        .service
        .db
        .insert_job_event(
            id,
            "runtime_launch_config_set",
            serde_json::json!({
                "start_command": start_command,
                "stop_command": stop_command,
                "working_directory": working_directory
            }),
        )
        .await
        .map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn start_job_runtime_launch(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    execute_job_runtime_launch(state, id, true).await.map(Json)
}

async fn stop_job_runtime_launch(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    execute_job_runtime_launch(state, id, false).await.map(Json)
}

#[derive(serde::Serialize)]
struct RuntimeAppRegistryShutdownSummary {
    stopped_jobs: i64,
    stopped_workspaces: i64,
}

async fn get_runtime_app_registry(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let workspace_registries = state
        .service
        .db
        .list_workspace_runtime_registries()
        .await
        .map_err(internal_error)?;
    let job_registries = state
        .service
        .db
        .list_job_runtime_app_registries()
        .await
        .map_err(internal_error)?;

    let running_workspaces = workspace_registries
        .into_iter()
        .filter(|entry| entry.status == "running")
        .collect::<Vec<_>>();
    let running_jobs = job_registries
        .into_iter()
        .filter(|entry| entry.status == "running")
        .collect::<Vec<_>>();

    Ok(Json(RuntimeAppRegistryResponse {
        workspaces: running_workspaces,
        jobs: running_jobs,
    }))
}

async fn shutdown_all_runtime_apps(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    if !state.service.runtime_enabled() {
        return Err((
            StatusCode::BAD_REQUEST,
            "runtime is not enabled".to_string(),
        ));
    }

    let job_registries = state
        .service
        .db
        .list_job_runtime_app_registries()
        .await
        .map_err(internal_error)?;
    let running_jobs = job_registries
        .into_iter()
        .filter(|entry| entry.status == "running")
        .collect::<Vec<_>>();

    // Stop job-level app instances first so they can release ports cleanly.
    let mut stopped_jobs: i64 = 0;
    let mut workspaces_to_stop: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for entry in running_jobs.iter() {
        workspaces_to_stop.insert(entry.workspace_id);
        match execute_job_runtime_launch(state.clone(), entry.job_id, false).await {
            Ok(_) => stopped_jobs += 1,
            Err(error) => {
                warn!(
                    job_id = %entry.job_id,
                    error = %error.1,
                    "failed to stop job runtime app instance during shutdown-all"
                );
            }
        }
    }

    // Then stop the workspace runtime containers themselves.
    let mut stopped_workspaces: i64 = 0;
    for workspace_id in workspaces_to_stop {
        match state.service.runtime_stop_for_workspace(workspace_id).await {
            Ok(runtime_info) => {
                let runtime_status = match runtime_info.status {
                    otter_core::domain::RuntimeContainerStatus::Running => "running",
                    otter_core::domain::RuntimeContainerStatus::Stopped => "stopped",
                    otter_core::domain::RuntimeContainerStatus::Missing => "missing",
                };
                if let Err(error) = state
                    .service
                    .db
                    .upsert_workspace_runtime_registry(
                        workspace_id,
                        &runtime_info.container_name,
                        &runtime_info.image_tag,
                        runtime_status,
                        runtime_info.preferred_url.as_deref(),
                        &runtime_info.ports,
                    )
                    .await
                {
                    warn!(
                        workspace_id = %workspace_id,
                        error = %error,
                        "failed to upsert workspace runtime registry during shutdown-all"
                    );
                }
                stopped_workspaces += 1;
            }
            Err(error) => {
                warn!(
                    workspace_id = %workspace_id,
                    error = %error,
                    "failed to stop runtime workspace container during shutdown-all"
                );
            }
        }
    }

    Ok(Json(RuntimeAppRegistryShutdownSummary {
        stopped_jobs,
        stopped_workspaces,
    }))
}

async fn set_job_preview_url(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetJobPreviewUrlRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let preview_url = body.preview_url.trim();
    if preview_url.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "preview_url must not be empty".to_string(),
        ));
    }
    let parsed = reqwest::Url::parse(preview_url).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            format!("preview_url must be a valid absolute URL: {error}"),
        )
    })?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err((
            StatusCode::BAD_REQUEST,
            "preview_url must use http or https".to_string(),
        ));
    }

    let updated = state
        .service
        .db
        .set_job_preview_url(id, preview_url)
        .await
        .map_err(internal_error)?;
    if !updated {
        return Err((StatusCode::NOT_FOUND, "job not found".to_string()));
    }
    state
        .service
        .db
        .insert_job_event(
            id,
            "preview_url_set",
            serde_json::json!({ "preview_url": preview_url }),
        )
        .await
        .map_err(internal_error)?;
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

async fn execute_job_runtime_launch(
    state: AppState,
    job_id: Uuid,
    start: bool,
) -> Result<WorkspaceCommandResponse, (StatusCode, String)> {
    let job = state
        .service
        .db
        .fetch_job(job_id)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "job not found".to_string()))?;
    let command = if start {
        job.runtime_start_command.clone()
    } else {
        job.runtime_stop_command.clone()
    }
    .ok_or((
        StatusCode::BAD_REQUEST,
        if start {
            "runtime start command is not configured"
        } else {
            "runtime stop command is not configured"
        }
        .to_string(),
    ))?;
    let result = execute_workspace_command(
        state.clone(),
        Some(job.workspace_id),
        WorkspaceCommandRequest {
            command,
            working_directory: job.runtime_command_cwd.clone(),
            shell_session_id: Some(format!("job-runtime:{job_id}")),
            timeout_seconds: Some(120),
        },
    )
    .await?;
    let event_type = if start {
        "runtime_launch_started"
    } else {
        "runtime_launch_stopped"
    };
    state
        .service
        .db
        .insert_job_event(
            job_id,
            event_type,
            serde_json::json!({
                "exit_code": result.exit_code,
                "working_directory": result.working_directory,
                "timed_out": result.timed_out
            }),
        )
        .await
        .map_err(internal_error)?;

    // Keep the job-level app registry in sync with runtime start/stop.
    let job_registry_status = if start { "running" } else { "stopped" };
    let runtime_ports;
    let runtime_preferred_url;
    if state.service.runtime_enabled() {
        match state
            .service
            .runtime_status_for_workspace(job.workspace_id)
            .await
        {
            Ok(runtime_info) => {
                runtime_ports = runtime_info.ports;
                runtime_preferred_url = runtime_info.preferred_url;
            }
            Err(error) => {
                warn!(
                    workspace_id = %job.workspace_id,
                    job_id = %job_id,
                    error = %error,
                    "failed to resolve runtime status for job app registry upsert"
                );
                runtime_ports = Vec::new();
                runtime_preferred_url = None;
            }
        }
    } else {
        runtime_ports = Vec::new();
        runtime_preferred_url = None;
    }

    let start_command = job.runtime_start_command.clone().unwrap_or_default();
    if let Err(error) = state
        .service
        .db
        .upsert_job_runtime_app_registry(
            job_id,
            job.workspace_id,
            job.runtime_command_cwd.as_deref(),
            &start_command,
            job.runtime_stop_command.as_deref(),
            job_registry_status,
            runtime_preferred_url.as_deref(),
            &runtime_ports,
        )
        .await
    {
        warn!(
            workspace_id = %job.workspace_id,
            job_id = %job_id,
            error = %error,
            "failed to upsert job runtime app registry"
        );
    }
    Ok(result)
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

fn normalize_relative_workspace_path(path: &str) -> Result<String, (StatusCode, String)> {
    let value = path.trim().replace('\\', "/");
    if value.is_empty() {
        return Ok(String::new());
    }
    if value.starts_with('/') {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must be relative to workspace root".to_string(),
        ));
    }
    let clean_segments = value
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if clean_segments.iter().any(|segment| *segment == "..") {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must not escape workspace root".to_string(),
        ));
    }
    Ok(clean_segments.join("/"))
}

fn workspace_root_marker_exists(workspace: &Workspace) -> bool {
    FsPath::new(&workspace.root_path)
        .join(WORKSPACE_ROOT_MARKER_FILE)
        .exists()
}

fn validate_self_contained_compose_dir(
    workspace: &Workspace,
    relative_dir: &str,
) -> Result<(), (StatusCode, String)> {
    if relative_dir.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must point to a self-contained project folder, not workspace root".to_string(),
        ));
    }

    if !workspace_root_marker_exists(workspace) {
        // Non-fatal: we still enforce compose-file presence to avoid breaking older workspaces.
        warn!(
            workspace_root = %workspace.root_path,
            "workspace root marker missing; consider recreating the workspace to enable safe cleanup defaults"
        );
    }

    let resolved_dir = resolve_workspace_subpath(workspace, relative_dir)?;
    if !resolved_dir.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must resolve to a directory".to_string(),
        ));
    }

    let has_docker_compose_yml = resolved_dir.join("docker-compose.yml").exists();
    let has_compose_yaml = resolved_dir.join("compose.yaml").exists();
    if !has_docker_compose_yml && !has_compose_yaml {
        return Err((
            StatusCode::BAD_REQUEST,
            "project directory must contain docker-compose.yml or compose.yaml".to_string(),
        ));
    }

    Ok(())
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
    use super::Workspace;
    use super::WORKSPACE_ROOT_MARKER_FILE;
    use super::{
        normalize_relative_workspace_path, split_stdout_and_cwd,
        validate_self_contained_compose_dir,
    };
    use axum::http::StatusCode;
    use chrono::Utc;
    use std::fs;
    use uuid::Uuid;

    #[test]
    fn strips_cwd_marker_from_stdout() {
        let stdout = "hello\n__OTTER_CWD__:/tmp/demo\n";
        let (cleaned, cwd) = split_stdout_and_cwd(stdout, "/fallback");
        assert_eq!(cleaned, "hello\n");
        assert_eq!(cwd, "/tmp/demo");
    }

    fn temp_workspace_root() -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("otter-guardrails-{}", Uuid::new_v4().as_simple()));
        fs::create_dir_all(&dir).expect("create temp workspace root");
        dir
    }

    fn workspace_from_root(root_path: &std::path::Path) -> Workspace {
        Workspace {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            name: "temp-workspace".to_string(),
            root_path: root_path.to_string_lossy().to_string(),
            isolated_vibe_home: root_path.join(".vibe").to_string_lossy().to_string(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn normalize_relative_workspace_path_rejects_absolute_paths() {
        let err = normalize_relative_workspace_path("/etc/passwd").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn normalize_relative_workspace_path_rejects_parent_segments() {
        let err = normalize_relative_workspace_path("a/../b").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_self_contained_compose_dir_rejects_missing_compose_file() {
        let root = temp_workspace_root();
        fs::write(root.join(WORKSPACE_ROOT_MARKER_FILE), "managed=true\n").unwrap();

        fs::create_dir_all(root.join("proj")).unwrap();

        let workspace = workspace_from_root(&root);
        let err = validate_self_contained_compose_dir(&workspace, "proj").unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("docker-compose.yml") || err.1.contains("compose.yaml"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn validate_self_contained_compose_dir_accepts_docker_compose_yml() {
        let root = temp_workspace_root();
        fs::write(root.join(WORKSPACE_ROOT_MARKER_FILE), "managed=true\n").unwrap();

        let proj = root.join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("docker-compose.yml"), "version: '3'\n").unwrap();

        let workspace = workspace_from_root(&root);
        validate_self_contained_compose_dir(&workspace, "proj").unwrap();

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn validate_self_contained_compose_dir_allows_when_workspace_marker_missing() {
        let root = temp_workspace_root();

        let proj = root.join("proj");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("docker-compose.yml"), "version: '3'\n").unwrap();

        let workspace = workspace_from_root(&root);
        validate_self_contained_compose_dir(&workspace, "proj").unwrap();

        let _ = fs::remove_dir_all(&root);
    }
}
