use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::time::sleep;
use uuid::Uuid;
use validator::Validate;

use crate::config::AppConfig;
use crate::db::Database;
use crate::domain::{
    CreateProjectRequest, CreateWorkspaceRequest, EnqueuePromptRequest, Job, JobEvent, Project,
    QueueItem, UpdateQueuePositionRequest, Workspace,
};
use crate::queue::{Queue, QueueMessage};
use crate::vibe::VibeExecutor;
use crate::workspace::WorkspaceManager;

const DEFAULT_QUEUE_NAME: &str = "otter:jobs";

#[derive(Clone)]
pub struct OtterService<Q: Queue> {
    pub db: Arc<Database>,
    pub queue: Arc<Q>,
    pub workspace_manager: Arc<WorkspaceManager>,
    pub vibe_executor: Arc<VibeExecutor>,
    pub max_attempts: i32,
}

impl<Q: Queue> OtterService<Q> {
    pub fn new(config: &AppConfig, db: Arc<Database>, queue: Arc<Q>) -> Self {
        let allowed_roots = std::env::var("OTTER_ALLOWED_ROOTS")
            .unwrap_or_default()
            .split(':')
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .collect::<Vec<_>>();

        Self {
            db,
            queue,
            workspace_manager: Arc::new(WorkspaceManager::new(
                allowed_roots,
                config.vibe_base_home.clone(),
            )),
            vibe_executor: Arc::new(VibeExecutor::new(config.vibe_bin.clone())),
            max_attempts: config.max_attempts,
        }
    }

    pub async fn create_project(&self, request: CreateProjectRequest) -> Result<Project> {
        request.validate()?;
        self.db.create_project(request).await
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        self.db.list_projects().await
    }

    pub async fn create_workspace(&self, request: CreateWorkspaceRequest) -> Result<Workspace> {
        request.validate()?;
        let canonical = self
            .workspace_manager
            .validate_workspace_path(PathBuf::from(&request.root_path).as_path())?;
        let workspace_id = Uuid::new_v4();
        let isolated_vibe_home = self
            .workspace_manager
            .prepare_isolated_vibe_home(workspace_id, &canonical)
            .await?;

        self.db
            .create_workspace(request, isolated_vibe_home.display().to_string())
            .await
    }

    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        self.db.list_workspaces().await
    }

    pub async fn enqueue_prompt(&self, request: EnqueuePromptRequest) -> Result<Job> {
        request.validate()?;
        let job = self.db.create_job(request, self.max_attempts).await?;
        self.db
            .insert_job_event(job.id, "accepted", serde_json::json!({}))
            .await?;
        self.queue
            .enqueue(DEFAULT_QUEUE_NAME, &QueueMessage { job_id: job.id })
            .await?;
        self.db
            .insert_job_event(job.id, "queued", serde_json::json!({}))
            .await?;
        Ok(job)
    }

    pub async fn process_next_job(&self) -> Result<Option<Uuid>> {
        let _ = self.queue.dequeue(DEFAULT_QUEUE_NAME).await?;
        let Some(job) = self.db.claim_next_queued_job().await? else {
            sleep(Duration::from_millis(250)).await;
            return Ok(None);
        };

        self.db
            .insert_job_event(job.id, "started", serde_json::json!({}))
            .await?;

        if let Err(error) = self.execute_job(&job).await {
            let error_message = error.to_string();
            if job.attempts + 1 < job.max_attempts {
                self.db
                    .set_job_back_to_queue(job.id, error_message.clone())
                    .await?;
                self.queue
                    .enqueue(DEFAULT_QUEUE_NAME, &QueueMessage { job_id: job.id })
                    .await?;
                self.db
                    .insert_job_event(
                        job.id,
                        "retry_queued",
                        serde_json::json!({ "error": error_message }),
                    )
                    .await?;
            } else {
                self.db.fail_job(job.id, error_message.clone()).await?;
                self.db
                    .insert_job_event(
                        job.id,
                        "failed",
                        serde_json::json!({ "error": error_message }),
                    )
                    .await?;
            }
        }

        Ok(Some(job.id))
    }

    async fn execute_job(&self, job: &Job) -> Result<()> {
        let workspace = self
            .db
            .fetch_workspace(job.workspace_id)
            .await?
            .ok_or_else(|| anyhow!("workspace not found: {}", job.workspace_id))?;
        let workspace_path = PathBuf::from(&workspace.root_path);
        let isolated_vibe_home = PathBuf::from(&workspace.isolated_vibe_home);

        let result = self
            .vibe_executor
            .run_prompt(&job.prompt, &workspace_path, &isolated_vibe_home)
            .await?;

        self.db
            .complete_job(
                job.id,
                result.assistant_output.clone(),
                result.raw_json.clone(),
            )
            .await?;

        self.db
            .insert_job_event(
                job.id,
                "completed",
                serde_json::json!({
                    "exit_code": result.exit_code,
                    "stderr": result.stderr
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn fetch_job(&self, job_id: Uuid) -> Result<Option<Job>> {
        self.db.fetch_job(job_id).await
    }

    pub async fn fetch_job_output(&self, job_id: Uuid) -> Result<Option<crate::domain::JobOutput>> {
        self.db.fetch_job_output(job_id).await
    }

    pub async fn queue_rank_for_job(&self, job_id: Uuid) -> Result<Option<i64>> {
        self.db.queue_rank_for_job(job_id).await
    }

    pub async fn list_queue(&self, limit: i64, offset: i64) -> Result<Vec<QueueItem>> {
        self.db.list_queue(limit, offset).await
    }

    pub async fn update_queue_position(
        &self,
        job_id: Uuid,
        request: UpdateQueuePositionRequest,
    ) -> Result<bool> {
        request.validate()?;
        let updated = self
            .db
            .update_queue_priority(job_id, request.priority)
            .await?;
        if updated {
            self.db
                .insert_job_event(
                    job_id,
                    "queue_priority_updated",
                    serde_json::json!({ "priority": request.priority }),
                )
                .await?;
        }
        Ok(updated)
    }

    pub async fn fetch_job_events(&self, job_id: Uuid) -> Result<Vec<JobEvent>> {
        self.db.list_job_events(job_id).await
    }

    pub async fn fetch_job_events_since(
        &self,
        since: chrono::DateTime<chrono::Utc>,
        limit: i64,
    ) -> Result<Vec<JobEvent>> {
        self.db.list_job_events_since(since, limit).await
    }

    pub async fn cancel_job(&self, job_id: Uuid) -> Result<bool> {
        let result = self.db.mark_job_cancelled(job_id).await?;
        if result.rows_affected() > 0 {
            self.db
                .insert_job_event(job_id, "cancelled", serde_json::json!({}))
                .await?;
            return Ok(true);
        }
        Ok(false)
    }
}
