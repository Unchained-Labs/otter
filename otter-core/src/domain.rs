use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Workspace {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub root_path: String,
    pub isolated_vibe_home: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "job_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Job {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub prompt: String,
    pub preview_url: Option<String>,
    pub is_paused: bool,
    pub status: JobStatus,
    pub priority: i32,
    pub schedule_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    pub max_attempts: i32,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct JobOutput {
    pub id: Uuid,
    pub job_id: Uuid,
    pub assistant_output: String,
    pub raw_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct JobEvent {
    pub id: Uuid,
    pub job_id: Uuid,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct CreateProjectRequest {
    #[validate(length(min = 2, max = 120))]
    pub name: String,
    #[validate(length(max = 400))]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct CreateWorkspaceRequest {
    pub project_id: Uuid,
    #[validate(length(min = 2, max = 120))]
    pub name: String,
    #[validate(length(min = 1, max = 4096))]
    pub root_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct EnqueuePromptRequest {
    pub workspace_id: Option<Uuid>,
    #[validate(length(min = 1, max = 100_000))]
    pub prompt: String,
    pub priority: Option<i32>,
    pub schedule_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
pub struct UpdateQueuePositionRequest {
    #[validate(range(min = 1, max = 100_000))]
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryItem {
    pub job_id: Uuid,
    pub workspace_id: Uuid,
    pub prompt: String,
    pub status: JobStatus,
    pub assistant_output: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct QueueItem {
    pub job_id: Uuid,
    pub workspace_id: Uuid,
    pub prompt: String,
    pub is_paused: bool,
    pub priority: i32,
    pub schedule_at: Option<DateTime<Utc>>,
    pub queue_rank: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeContainerStatus {
    Running,
    Stopped,
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePortBinding {
    pub container_port: u16,
    pub host_ip: String,
    pub host_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeContainerInfo {
    pub workspace_id: Uuid,
    pub container_name: String,
    pub image_tag: String,
    pub container_id: Option<String>,
    pub status: RuntimeContainerStatus,
    pub ports: Vec<RuntimePortBinding>,
    pub preferred_url: Option<String>,
}
