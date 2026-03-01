use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::Utc;
use tokio::time::sleep;
use tracing::{info, warn};
use uuid::Uuid;
use validator::Validate;

use crate::config::AppConfig;
use crate::db::Database;
use crate::domain::{
    CreateProjectRequest, CreateWorkspaceRequest, EnqueuePromptRequest, Job, JobEvent, JobStatus,
    Project, QueueItem, UpdateQueuePositionRequest, Workspace,
};
use crate::queue::{Queue, QueueMessage};
use crate::vibe::{VibeExecutor, VibeOutputChunk};
use crate::workspace::WorkspaceManager;

const DEFAULT_QUEUE_NAME: &str = "otter:jobs";

#[derive(Clone)]
pub struct OtterService<Q: Queue> {
    pub db: Arc<Database>,
    pub queue: Arc<Q>,
    pub workspace_manager: Arc<WorkspaceManager>,
    pub vibe_executor: Arc<VibeExecutor>,
    pub max_attempts: i32,
    pub default_workspace_path: Option<PathBuf>,
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
            default_workspace_path: config.default_workspace_path.clone(),
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
            .create_workspace(
                workspace_id,
                request,
                canonical.display().to_string(),
                isolated_vibe_home.display().to_string(),
            )
            .await
    }

    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        self.db.list_workspaces().await
    }

    pub async fn enqueue_prompt(&self, request: EnqueuePromptRequest) -> Result<Job> {
        request.validate()?;
        let workspace_id = match request.workspace_id {
            Some(workspace_id) => workspace_id,
            None => self.resolve_default_workspace_id().await?,
        };
        let job = self
            .db
            .create_job(
                workspace_id,
                &request.prompt,
                request.priority,
                request.schedule_at,
                self.max_attempts,
            )
            .await?;
        self.db
            .insert_job_event(job.id, "accepted", serde_json::json!({}))
            .await?;
        self.queue
            .enqueue(DEFAULT_QUEUE_NAME, &QueueMessage { job_id: job.id })
            .await?;
        self.db
            .insert_job_event(job.id, "queued", serde_json::json!({}))
            .await?;
        info!(
            %workspace_id,
            job_id = %job.id,
            priority = job.priority,
            "job accepted and queued"
        );
        Ok(job)
    }

    async fn resolve_default_workspace_id(&self) -> Result<Uuid> {
        let fallback_path = self.default_workspace_path.as_ref().ok_or_else(|| {
            anyhow!("workspace_id is required when OTTER_DEFAULT_WORKSPACE_PATH is not configured")
        })?;
        let canonical = self
            .workspace_manager
            .validate_workspace_path(fallback_path.as_path())?;
        let root_path = canonical.to_string_lossy().to_string();

        if let Some(existing) = self.db.find_workspace_by_root_path(&root_path).await? {
            return Ok(existing.id);
        }

        const DEFAULT_PROJECT_NAME: &str = "default";
        const DEFAULT_WORKSPACE_NAME: &str = "default-workspace";

        let project_id =
            if let Some(project) = self.db.find_project_by_name(DEFAULT_PROJECT_NAME).await? {
                project.id
            } else {
                self.db
                    .create_project(CreateProjectRequest {
                        name: DEFAULT_PROJECT_NAME.to_string(),
                        description: Some(
                            "Auto-created project for OTTER_DEFAULT_WORKSPACE_PATH".to_string(),
                        ),
                    })
                    .await?
                    .id
            };

        let workspace_id = Uuid::new_v4();
        let isolated_vibe_home = self
            .workspace_manager
            .prepare_isolated_vibe_home(workspace_id, &canonical)
            .await?;

        let workspace = self
            .db
            .create_workspace(
                workspace_id,
                CreateWorkspaceRequest {
                    project_id,
                    name: DEFAULT_WORKSPACE_NAME.to_string(),
                    root_path,
                },
                canonical.display().to_string(),
                isolated_vibe_home.display().to_string(),
            )
            .await?;

        Ok(workspace.id)
    }

    pub async fn process_next_job(&self) -> Result<Option<Uuid>> {
        let Some(message) = self.queue.dequeue(DEFAULT_QUEUE_NAME).await? else {
            sleep(Duration::from_millis(250)).await;
            return Ok(None);
        };
        info!(job_id = %message.job_id, "dequeued job from redis queue");
        let Some(job) = self.db.claim_queued_job_by_id(message.job_id).await? else {
            if let Some(job_state) = self.db.fetch_job(message.job_id).await? {
                let should_requeue_for_schedule = matches!(job_state.status, JobStatus::Queued)
                    && job_state
                        .schedule_at
                        .map(|at| at > Utc::now())
                        .unwrap_or(false);
                if should_requeue_for_schedule {
                    self.queue
                        .enqueue(
                            DEFAULT_QUEUE_NAME,
                            &QueueMessage {
                                job_id: message.job_id,
                            },
                        )
                        .await?;
                    info!(
                        job_id = %message.job_id,
                        schedule_at = ?job_state.schedule_at,
                        "job is scheduled in the future; re-queued"
                    );
                }
            }
            sleep(Duration::from_millis(250)).await;
            return Ok(None);
        };
        info!(
            job_id = %job.id,
            attempts = job.attempts,
            max_attempts = job.max_attempts,
            "job claimed and starting execution"
        );

        self.db
            .insert_job_event(job.id, "started", serde_json::json!({}))
            .await?;

        if let Err(error) = self.execute_job(&job).await {
            let error_message = error.to_string();
            if job.attempts + 1 < job.max_attempts {
                let requeued = self
                    .db
                    .set_job_back_to_queue(job.id, error_message.clone())
                    .await?;
                if requeued {
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
                    warn!(
                        job_id = %job.id,
                        attempts = job.attempts + 1,
                        max_attempts = job.max_attempts,
                        error = %error_message,
                        "job failed and was re-queued for retry"
                    );
                }
            } else {
                let failed = self.db.fail_job(job.id, error_message.clone()).await?;
                if failed {
                    self.db
                        .insert_job_event(
                            job.id,
                            "failed",
                            serde_json::json!({ "error": error_message }),
                        )
                        .await?;
                    warn!(
                        job_id = %job.id,
                        attempts = job.attempts + 1,
                        max_attempts = job.max_attempts,
                        error = %error_message,
                        "job failed permanently"
                    );
                }
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
            .run_prompt_streaming(
                &job.prompt,
                &workspace_path,
                &isolated_vibe_home,
                |chunk: VibeOutputChunk| async move {
                    self.db
                        .insert_job_event(
                            job.id,
                            "output_chunk",
                            serde_json::json!({
                                "stream": chunk.stream,
                                "line": chunk.line
                            }),
                        )
                        .await?;
                    Ok(())
                },
            )
            .await?;

        let completed = self
            .db
            .complete_job(
                job.id,
                result.assistant_output.clone(),
                result.raw_json.clone(),
            )
            .await?;
        if !completed {
            // Job was cancelled or transitioned concurrently; do not emit completion event.
            warn!(
                job_id = %job.id,
                "job completion skipped because status changed concurrently"
            );
            return Ok(());
        }

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
        info!(
            job_id = %job.id,
            exit_code = result.exit_code,
            "job completed successfully"
        );
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
