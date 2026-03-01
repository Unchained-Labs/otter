use std::convert::Infallible;
use std::fs;
use std::path::{Path as FsPath, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use otter_core::config::AppConfig;
use otter_core::db::Database;
use otter_core::domain::{
    CreateProjectRequest, CreateWorkspaceRequest, EnqueuePromptRequest, Job, JobEvent, JobOutput,
    QueueItem, UpdateQueuePositionRequest, Workspace,
};
use otter_core::queue::RedisQueue;
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
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

#[derive(serde::Serialize)]
struct JobResponse {
    job: Job,
    output: Option<JobOutput>,
    queue_rank: Option<i64>,
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
    let state = AppState { service };

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
        .route("/v1/prompts", post(enqueue_prompt))
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
    if body.command.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "command must not be empty".to_string(),
        ));
    }

    let workspace = state
        .service
        .db
        .fetch_workspace(id)
        .await
        .map_err(internal_error)?
        .ok_or((StatusCode::NOT_FOUND, "workspace not found".to_string()))?;

    let cwd =
        resolve_workspace_subpath(&workspace, body.working_directory.as_deref().unwrap_or(""))?;
    let timeout_seconds = body.timeout_seconds.unwrap_or(30).clamp(1, 300);

    info!(
        workspace_id = %id,
        cwd = %cwd.display(),
        timeout_seconds,
        command = %body.command,
        "running workspace shell command"
    );

    let output = Command::new("timeout")
        .arg(format!("{timeout_seconds}s"))
        .arg("bash")
        .arg("-lc")
        .arg(&body.command)
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
            workspace_id = %id,
            cwd = %cwd.display(),
            timeout_seconds,
            command = %body.command,
            "workspace shell command timed out"
        );
    }

    Ok(Json(WorkspaceCommandResponse {
        workspace_id: id,
        command: body.command,
        working_directory: cwd.display().to_string(),
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        timed_out,
    }))
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
