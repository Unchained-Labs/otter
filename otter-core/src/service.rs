use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::{fs, path::Path};

use anyhow::{anyhow, Result};
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::{interval, sleep, Duration as TokioDuration, MissedTickBehavior};
use tracing::{info, warn};
use uuid::Uuid;
use validator::Validate;

use crate::config::AppConfig;
use crate::db::Database;
use crate::domain::{
    CreateProjectRequest, CreateWorkspaceRequest, EnqueuePromptRequest, Job, JobEvent, JobStatus,
    Project, QueueItem, RuntimeContainerInfo, RuntimeContainerStatus, UpdateQueuePositionRequest,
    Workspace,
};
use crate::queue::{Queue, QueueMessage};
use crate::runtime::docker_manager::{DockerRuntimeManager, RuntimeExecResult};
use crate::runtime::shell_session::build_shell_session_key;
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
    pub default_workspace_subdir: String,
    pub runtime_manager: Option<Arc<DockerRuntimeManager>>,
    pub runtime_shell_cwds: Arc<Mutex<std::collections::HashMap<String, String>>>,
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
            vibe_executor: Arc::new(VibeExecutor::new(
                config.vibe_bin.clone(),
                config.otter_api_base_url.clone(),
                config.vibe_model.clone(),
                config.vibe_provider.clone(),
                config.vibe_extra_env.clone(),
            )),
            max_attempts: config.max_attempts,
            default_workspace_path: config.default_workspace_path.clone(),
            default_workspace_subdir: config.default_workspace_subdir.clone(),
            runtime_manager: if config.runtime.enabled {
                match DockerRuntimeManager::new(config.runtime.clone()) {
                    Ok(manager) => Some(Arc::new(manager)),
                    Err(error) => {
                        warn!(error = %error, "runtime enabled but docker manager could not initialize");
                        None
                    }
                }
            } else {
                None
            },
            runtime_shell_cwds: Arc::new(Mutex::new(std::collections::HashMap::new())),
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
        self.sync_workspace_directories_from_default_root().await?;
        self.db.list_workspaces().await
    }

    pub async fn enqueue_prompt(&self, request: EnqueuePromptRequest) -> Result<Job> {
        request.validate()?;
        let workspace_id = self
            .resolve_workspace_id_or_default(request.workspace_id)
            .await?;
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
            job_id = %job.id,
            workspace_id = %workspace_id,
            priority = job.priority,
            "job accepted and queued"
        );
        Ok(job)
    }

    async fn resolve_default_workspace_id(&self) -> Result<Uuid> {
        let fallback_path = self.default_workspace_path.as_ref().ok_or_else(|| {
            anyhow!("workspace_id is required when OTTER_DEFAULT_WORKSPACE_PATH is not configured")
        })?;
        let canonical_root = self
            .workspace_manager
            .validate_workspace_path(fallback_path.as_path())?;
        let target_path = canonical_root.join(&self.default_workspace_subdir);
        fs::create_dir_all(&target_path)?;
        let canonical = self
            .workspace_manager
            .validate_workspace_path(&target_path)?;
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

    pub async fn resolve_workspace_id_or_default(
        &self,
        workspace_id: Option<Uuid>,
    ) -> Result<Uuid> {
        match workspace_id {
            Some(id) => Ok(id),
            None => self.resolve_default_workspace_id().await,
        }
    }

    async fn sync_workspace_directories_from_default_root(&self) -> Result<()> {
        let Some(default_root) = &self.default_workspace_path else {
            return Ok(());
        };
        let canonical_root = self
            .workspace_manager
            .validate_workspace_path(default_root.as_path())?;
        let project_id = self.ensure_default_project().await?;
        let entries = fs::read_dir(&canonical_root)?;
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let directory_name = entry.file_name().to_string_lossy().to_string();
            if directory_name.starts_with('.') || directory_name.starts_with("__") {
                continue;
            }
            let dir_path = entry.path();
            let Ok(canonical_dir) = self
                .workspace_manager
                .validate_workspace_path(Path::new(&dir_path))
            else {
                continue;
            };
            let root_path = canonical_dir.to_string_lossy().to_string();
            if self
                .db
                .find_workspace_by_root_path(&root_path)
                .await?
                .is_some()
            {
                continue;
            }

            let workspace_id = Uuid::new_v4();
            let isolated_vibe_home = self
                .workspace_manager
                .prepare_isolated_vibe_home(workspace_id, &canonical_dir)
                .await?;
            let name = directory_name;
            let _ = self
                .db
                .create_workspace(
                    workspace_id,
                    CreateWorkspaceRequest {
                        project_id,
                        name,
                        root_path: root_path.clone(),
                    },
                    root_path,
                    isolated_vibe_home.display().to_string(),
                )
                .await?;
        }
        Ok(())
    }

    async fn ensure_default_project(&self) -> Result<Uuid> {
        const DEFAULT_PROJECT_NAME: &str = "default";
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
        Ok(project_id)
    }

    pub async fn process_next_job(&self) -> Result<Option<Uuid>> {
        let Some(message) = self.queue.dequeue(DEFAULT_QUEUE_NAME).await? else {
            sleep(Duration::from_millis(250)).await;
            return Ok(None);
        };
        info!(job_id = %message.job_id, "dequeued job message from redis");
        let Some(job) = self.db.claim_queued_job_by_id(message.job_id).await? else {
            warn!(job_id = %message.job_id, "dequeued message could not claim queued job");
            if let Some(job_state) = self.db.fetch_job(message.job_id).await? {
                let should_requeue_for_schedule = matches!(job_state.status, JobStatus::Queued)
                    && job_state
                        .schedule_at
                        .map(|at| at > Utc::now())
                        .unwrap_or(false);
                let should_requeue_for_pause =
                    matches!(job_state.status, JobStatus::Queued) && job_state.is_paused;
                if should_requeue_for_schedule || should_requeue_for_pause {
                    info!(job_id = %message.job_id, "job not runnable yet; requeued message");
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
                        is_paused = job_state.is_paused,
                        "job is not runnable yet; re-queued"
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
        info!(
            job_id = %job.id,
            workspace_id = %job.workspace_id,
            attempt = job.attempts + 1,
            max_attempts = job.max_attempts,
            "job claimed and started"
        );

        if let Err(error) = self.execute_job(&job).await {
            let error_message = error.to_string();
            if job.attempts + 1 < job.max_attempts {
                let requeued = self
                    .db
                    .set_job_back_to_queue(job.id, error_message.clone())
                    .await?;
                if requeued {
                    warn!(
                        job_id = %job.id,
                        attempt = job.attempts + 1,
                        max_attempts = job.max_attempts,
                        error = %error_message,
                        "job failed and was requeued"
                    );
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
                    warn!(
                        job_id = %job.id,
                        attempts = job.attempts + 1,
                        error = %error_message,
                        "job failed terminally"
                    );
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
        let output_chunk_count = Arc::new(AtomicUsize::new(0));
        let chunk_counter = output_chunk_count.clone();

        info!(
            job_id = %job.id,
            workspace_id = %workspace.id,
            workspace_root = %workspace.root_path,
            isolated_vibe_home = %workspace.isolated_vibe_home,
            "executing vibe prompt"
        );

        let result = self
            .vibe_executor
            .run_prompt_streaming(
                &job.prompt,
                job.id,
                &workspace_path,
                &isolated_vibe_home,
                |chunk: VibeOutputChunk| {
                    let chunk_counter = chunk_counter.clone();
                    async move {
                        chunk_counter.fetch_add(1, Ordering::Relaxed);
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
                    }
                },
                || async {
                    let status = self.db.fetch_job(job.id).await?.map(|value| value.status);
                    Ok(matches!(status, Some(JobStatus::Cancelled)))
                },
            )
            .await?;
        info!(
            job_id = %job.id,
            exit_code = result.exit_code,
            output_chunks = output_chunk_count.load(Ordering::Relaxed),
            "vibe execution finished"
        );

        let setup_line_count = self
            .run_setup_script_with_streaming(job.id, &workspace_path)
            .await?;
        info!(
            job_id = %job.id,
            setup_lines = setup_line_count,
            "setup.sh execution finished"
        );

        if let Some(runtime) = &self.runtime_manager {
            let runtime_info = runtime
                .ensure_workspace_container(workspace.id, &workspace_path)
                .await?;
            self.db
                .insert_job_event(
                    job.id,
                    "runtime_container_ready",
                    serde_json::json!({
                        "container_name": runtime_info.container_name,
                        "container_id": runtime_info.container_id,
                        "status": runtime_info.status,
                        "preferred_url": runtime_info.preferred_url,
                        "ports": runtime_info.ports,
                    }),
                )
                .await?;
            if let Some(preview_url) = runtime_info.preferred_url {
                self.db
                    .insert_job_event(
                        job.id,
                        "output_chunk",
                        serde_json::json!({
                            "stream": "stdout",
                            "line": format!("[runtime] preview available at {preview_url}")
                        }),
                    )
                    .await?;
            }
        }

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

    async fn run_setup_script_with_streaming(
        &self,
        job_id: Uuid,
        workspace_path: &Path,
    ) -> Result<usize> {
        let setup_path = workspace_path.join("setup.sh");
        if !setup_path.exists() {
            return Err(anyhow!(
                "setup.sh was not generated in project root ({})",
                setup_path.display()
            ));
        }

        self.db
            .insert_job_event(
                job_id,
                "output_chunk",
                serde_json::json!({
                    "stream": "stdout",
                    "line": "[setup] starting ./setup.sh"
                }),
            )
            .await?;

        let mut child = Command::new("bash")
            .arg("-lc")
            .arg("chmod +x setup.sh && ./setup.sh")
            .current_dir(workspace_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("missing setup stdout pipe"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("missing setup stderr pipe"))?;

        let mut stdout_lines = BufReader::new(stdout).lines();
        let mut stderr_lines = BufReader::new(stderr).lines();
        let mut line_count: usize = 0;
        let mut cancel_poll = interval(TokioDuration::from_millis(400));
        cancel_poll.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                line = stdout_lines.next_line() => {
                    match line? {
                        Some(line) => {
                            line_count += 1;
                            self.db
                                .insert_job_event(
                                    job_id,
                                    "output_chunk",
                                    serde_json::json!({
                                        "stream": "stdout",
                                        "line": format!("[setup] {line}")
                                    }),
                                )
                                .await?;
                        }
                        None => break,
                    }
                }
                line = stderr_lines.next_line() => {
                    match line? {
                        Some(line) => {
                            line_count += 1;
                            self.db
                                .insert_job_event(
                                    job_id,
                                    "output_chunk",
                                    serde_json::json!({
                                        "stream": "stderr",
                                        "line": format!("[setup] {line}")
                                    }),
                                )
                                .await?;
                        }
                        None => break,
                    }
                }
                _ = cancel_poll.tick() => {
                    let status = self.db.fetch_job(job_id).await?.map(|job| job.status);
                    if matches!(status, Some(JobStatus::Cancelled)) {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        return Err(anyhow!("job cancelled while setup.sh was running"));
                    }
                }
            }
        }

        // Flush any remaining lines from either stream.
        while let Some(line) = stdout_lines.next_line().await? {
            line_count += 1;
            self.db
                .insert_job_event(
                    job_id,
                    "output_chunk",
                    serde_json::json!({
                        "stream": "stdout",
                        "line": format!("[setup] {line}")
                    }),
                )
                .await?;
        }
        while let Some(line) = stderr_lines.next_line().await? {
            line_count += 1;
            self.db
                .insert_job_event(
                    job_id,
                    "output_chunk",
                    serde_json::json!({
                        "stream": "stderr",
                        "line": format!("[setup] {line}")
                    }),
                )
                .await?;
        }

        let status = child.wait().await?;
        if !status.success() {
            return Err(anyhow!(
                "setup.sh exited with code {}",
                status.code().unwrap_or(-1)
            ));
        }

        self.db
            .insert_job_event(
                job_id,
                "output_chunk",
                serde_json::json!({
                    "stream": "stdout",
                    "line": "[setup] completed successfully"
                }),
            )
            .await?;

        Ok(line_count)
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

    pub fn runtime_enabled(&self) -> bool {
        self.runtime_manager.is_some()
    }

    pub async fn runtime_status_for_workspace(
        &self,
        workspace_id: Uuid,
    ) -> Result<RuntimeContainerInfo> {
        let runtime = self
            .runtime_manager
            .as_ref()
            .ok_or_else(|| anyhow!("runtime is not enabled"))?;
        runtime.runtime_status(workspace_id).await
    }

    pub async fn runtime_start_for_workspace(
        &self,
        workspace_id: Uuid,
    ) -> Result<RuntimeContainerInfo> {
        let runtime = self
            .runtime_manager
            .as_ref()
            .ok_or_else(|| anyhow!("runtime is not enabled"))?;
        runtime.start_workspace_container(workspace_id).await
    }

    pub async fn runtime_stop_for_workspace(
        &self,
        workspace_id: Uuid,
    ) -> Result<RuntimeContainerInfo> {
        let runtime = self
            .runtime_manager
            .as_ref()
            .ok_or_else(|| anyhow!("runtime is not enabled"))?;
        runtime.stop_workspace_container(workspace_id).await
    }

    pub async fn runtime_restart_for_workspace(
        &self,
        workspace_id: Uuid,
    ) -> Result<RuntimeContainerInfo> {
        let runtime = self
            .runtime_manager
            .as_ref()
            .ok_or_else(|| anyhow!("runtime is not enabled"))?;
        runtime.restart_workspace_container(workspace_id).await
    }

    pub async fn runtime_logs_for_workspace(
        &self,
        workspace_id: Uuid,
        tail: usize,
    ) -> Result<String> {
        let runtime = self
            .runtime_manager
            .as_ref()
            .ok_or_else(|| anyhow!("runtime is not enabled"))?;
        runtime.logs_for_workspace(workspace_id, tail).await
    }

    pub async fn runtime_exec_for_workspace(
        &self,
        workspace_id: Uuid,
        session_id: &str,
        command: &str,
    ) -> Result<RuntimeExecResult> {
        let runtime = self
            .runtime_manager
            .as_ref()
            .ok_or_else(|| anyhow!("runtime is not enabled"))?;
        let key = build_shell_session_key(workspace_id, session_id);
        let mut retry_cwd = {
            let sessions = self.runtime_shell_cwds.lock().await;
            sessions.get(&key).cloned()
        };
        let result = match runtime
            .exec_in_workspace(workspace_id, command, retry_cwd.as_deref())
            .await
        {
            Ok(result) => result,
            Err(initial_error) => {
                let status = runtime.runtime_status(workspace_id).await?;
                match status.status {
                    RuntimeContainerStatus::Running => return Err(initial_error),
                    RuntimeContainerStatus::Stopped => {
                        runtime.start_workspace_container(workspace_id).await?;
                    }
                    RuntimeContainerStatus::Missing => {
                        let workspace = self
                            .db
                            .fetch_workspace(workspace_id)
                            .await?
                            .ok_or_else(|| anyhow!("workspace not found: {workspace_id}"))?;
                        runtime
                            .ensure_workspace_container(workspace_id, Path::new(&workspace.root_path))
                            .await?;
                        // New container starts from default cwd; clear any stale cwd from prior session.
                        let mut sessions = self.runtime_shell_cwds.lock().await;
                        sessions.remove(&key);
                        retry_cwd = None;
                    }
                }
                runtime
                    .exec_in_workspace(workspace_id, command, retry_cwd.as_deref())
                    .await
                    .map_err(|retry_error| {
                        anyhow!(
                            "failed to execute command in runtime container after recovery (first error: {initial_error}; retry error: {retry_error})"
                        )
                    })?
            }
        };
        {
            let mut sessions = self.runtime_shell_cwds.lock().await;
            sessions.insert(key, result.cwd.clone());
        }
        Ok(result)
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

    pub async fn pause_job(&self, job_id: Uuid) -> Result<bool> {
        let updated = self.db.pause_queued_job(job_id).await?;
        if updated {
            self.db
                .insert_job_event(job_id, "paused", serde_json::json!({}))
                .await?;
        }
        Ok(updated)
    }

    pub async fn resume_job(&self, job_id: Uuid) -> Result<bool> {
        let updated = self.db.resume_queued_job(job_id).await?;
        if updated {
            self.db
                .insert_job_event(job_id, "resumed", serde_json::json!({}))
                .await?;
            self.queue
                .enqueue(DEFAULT_QUEUE_NAME, &QueueMessage { job_id })
                .await?;
        }
        Ok(updated)
    }
}
