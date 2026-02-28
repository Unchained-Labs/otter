use std::convert::Infallible;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::{Path, Query, State};
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
    QueueItem, UpdateQueuePositionRequest,
};
use otter_core::queue::RedisQueue;
use otter_core::service::OtterService;
use tokio::time::Duration as TokioDuration;
use tracing::info;
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
    let database = Arc::new(Database::connect(&config.database_url).await?);
    database.migrate().await?;
    let queue = Arc::new(RedisQueue::connect(&config.redis_url).await?);
    let service = Arc::new(OtterService::new(&config, database, queue));
    let state = AppState { service };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/projects", post(create_project).get(list_projects))
        .route(
            "/v1/workspaces",
            post(create_workspace).get(list_workspaces),
        )
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
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    info!("otter-server listening on {}", config.listen_addr);
    axum::serve(listener, app).await?;
    Ok(())
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
