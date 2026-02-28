use anyhow::{bail, Result};
use sqlx::postgres::{PgPoolOptions, PgQueryResult};
use sqlx::PgPool;
use std::path::PathBuf;
use uuid::Uuid;

use crate::domain::{
    CreateProjectRequest, CreateWorkspaceRequest, EnqueuePromptRequest, HistoryItem, Job, JobEvent,
    JobOutput, JobStatus, Project, QueueItem, Workspace,
};

#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(20)
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn migrate(&self) -> Result<()> {
        let mut candidates = Vec::<PathBuf>::new();
        if let Ok(path) = std::env::var("OTTER_MIGRATIONS_DIR") {
            candidates.push(PathBuf::from(path));
        }
        candidates.push(PathBuf::from("/srv/otter/migrations"));
        candidates.push(std::env::current_dir()?.join("migrations"));
        candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations"));

        for path in candidates {
            if !path.exists() {
                continue;
            }
            let migrator = sqlx::migrate::Migrator::new(path.as_path()).await?;
            migrator.run(&self.pool).await?;
            return Ok(());
        }

        bail!("while resolving migrations: No such file or directory (os error 2)")
    }

    pub async fn create_project(&self, request: CreateProjectRequest) -> Result<Project> {
        let project = sqlx::query_as::<_, Project>(
            r#"
            INSERT INTO projects (name, description)
            VALUES ($1, $2)
            RETURNING id, name, description, created_at
            "#,
        )
        .bind(request.name)
        .bind(request.description)
        .fetch_one(&self.pool)
        .await?;
        Ok(project)
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        let records = sqlx::query_as::<_, Project>(
            "SELECT id, name, description, created_at FROM projects ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(records)
    }

    pub async fn create_workspace(
        &self,
        request: CreateWorkspaceRequest,
        isolated_vibe_home: String,
    ) -> Result<Workspace> {
        let workspace = sqlx::query_as::<_, Workspace>(
            r#"
            INSERT INTO workspaces (project_id, name, root_path, isolated_vibe_home)
            VALUES ($1, $2, $3, $4)
            RETURNING id, project_id, name, root_path, isolated_vibe_home, created_at
            "#,
        )
        .bind(request.project_id)
        .bind(request.name)
        .bind(request.root_path)
        .bind(isolated_vibe_home)
        .fetch_one(&self.pool)
        .await?;
        Ok(workspace)
    }

    pub async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let records = sqlx::query_as::<_, Workspace>(
            "SELECT id, project_id, name, root_path, isolated_vibe_home, created_at FROM workspaces ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(records)
    }

    pub async fn create_job(
        &self,
        request: EnqueuePromptRequest,
        max_attempts: i32,
    ) -> Result<Job> {
        let job = sqlx::query_as::<_, Job>(
            r#"
            INSERT INTO jobs (workspace_id, prompt, status, priority, schedule_at, attempts, max_attempts)
            VALUES ($1, $2, 'queued', $3, $4, 0, $5)
            RETURNING id, workspace_id, prompt, status AS "status: JobStatus", priority, schedule_at, attempts, max_attempts, error, created_at, updated_at
            "#,
        )
        .bind(request.workspace_id)
        .bind(request.prompt)
        .bind(request.priority.unwrap_or(100))
        .bind(request.schedule_at)
        .bind(max_attempts)
        .fetch_one(&self.pool)
        .await?;
        Ok(job)
    }

    pub async fn fetch_job(&self, job_id: Uuid) -> Result<Option<Job>> {
        let job = sqlx::query_as::<_, Job>(
            r#"
            SELECT id, workspace_id, prompt, status AS "status: JobStatus", priority, schedule_at, attempts, max_attempts, error, created_at, updated_at
            FROM jobs WHERE id = $1
            "#,
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(job)
    }

    pub async fn fetch_workspace(&self, workspace_id: Uuid) -> Result<Option<Workspace>> {
        let workspace = sqlx::query_as::<_, Workspace>(
            r#"
            SELECT id, project_id, name, root_path, isolated_vibe_home, created_at
            FROM workspaces WHERE id = $1
            "#,
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(workspace)
    }

    pub async fn list_history(&self, limit: i64) -> Result<Vec<HistoryItem>> {
        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                Uuid,
                String,
                JobStatus,
                Option<String>,
                chrono::DateTime<chrono::Utc>,
            ),
        >(
            r#"
            SELECT
              j.id,
              j.workspace_id,
              j.prompt,
              j.status AS "status: JobStatus",
              o.assistant_output,
              j.created_at
            FROM jobs j
            LEFT JOIN job_outputs o ON o.job_id = j.id
            ORDER BY j.created_at DESC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(
                |(job_id, workspace_id, prompt, status, assistant_output, created_at)| {
                    HistoryItem {
                        job_id,
                        workspace_id,
                        prompt,
                        status,
                        assistant_output,
                        created_at,
                    }
                },
            )
            .collect())
    }

    pub async fn list_job_events(&self, job_id: Uuid) -> Result<Vec<JobEvent>> {
        let events = sqlx::query_as::<_, JobEvent>(
            r#"
            SELECT id, job_id, event_type, payload, created_at
            FROM job_events
            WHERE job_id = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(events)
    }

    pub async fn insert_job_event(
        &self,
        job_id: Uuid,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<JobEvent> {
        let event = sqlx::query_as::<_, JobEvent>(
            r#"
            INSERT INTO job_events (job_id, event_type, payload)
            VALUES ($1, $2, $3)
            RETURNING id, job_id, event_type, payload, created_at
            "#,
        )
        .bind(job_id)
        .bind(event_type)
        .bind(payload)
        .fetch_one(&self.pool)
        .await?;
        Ok(event)
    }

    pub async fn mark_job_running(&self, job_id: Uuid) -> Result<PgQueryResult> {
        let result = sqlx::query(
            "UPDATE jobs SET status = 'running', updated_at = now() WHERE id = $1 AND status = 'queued'",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(result)
    }

    pub async fn claim_next_queued_job(&self) -> Result<Option<Job>> {
        let job = sqlx::query_as::<_, Job>(
            r#"
            WITH next_job AS (
                SELECT id
                FROM jobs
                WHERE status = 'queued'
                  AND (schedule_at IS NULL OR schedule_at <= now())
                ORDER BY priority ASC, created_at ASC
                FOR UPDATE SKIP LOCKED
                LIMIT 1
            )
            UPDATE jobs j
            SET status = 'running',
                updated_at = now()
            FROM next_job
            WHERE j.id = next_job.id
            RETURNING j.id, j.workspace_id, j.prompt, j.status AS "status: JobStatus", j.priority, j.schedule_at, j.attempts, j.max_attempts, j.error, j.created_at, j.updated_at
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(job)
    }

    pub async fn mark_job_cancelled(&self, job_id: Uuid) -> Result<PgQueryResult> {
        let result = sqlx::query(
            "UPDATE jobs SET status = 'cancelled', updated_at = now() WHERE id = $1 AND status IN ('queued', 'running')",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(result)
    }

    pub async fn complete_job(
        &self,
        job_id: Uuid,
        assistant_output: String,
        raw_json: serde_json::Value,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE jobs SET status = 'succeeded', updated_at = now() WHERE id = $1")
            .bind(job_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO job_outputs (job_id, assistant_output, raw_json) VALUES ($1, $2, $3)",
        )
        .bind(job_id)
        .bind(assistant_output)
        .bind(raw_json)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn fail_job(&self, job_id: Uuid, error: String) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE jobs
            SET status = 'failed', attempts = attempts + 1, error = $2, updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_job_back_to_queue(&self, job_id: Uuid, error: String) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE jobs
            SET status = 'queued', attempts = attempts + 1, error = $2, updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn fetch_job_output(&self, job_id: Uuid) -> Result<Option<JobOutput>> {
        let output = sqlx::query_as::<_, JobOutput>(
            r#"
            SELECT id, job_id, assistant_output, raw_json, created_at
            FROM job_outputs
            WHERE job_id = $1
            "#,
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(output)
    }

    pub async fn queue_rank_for_job(&self, job_id: Uuid) -> Result<Option<i64>> {
        let rank = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT queue_rank
            FROM (
                SELECT
                    id,
                    row_number() OVER (
                        ORDER BY priority ASC, created_at ASC
                    ) AS queue_rank
                FROM jobs
                WHERE status = 'queued'
                  AND (schedule_at IS NULL OR schedule_at <= now())
            ) ranked
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(rank)
    }

    pub async fn list_queue(&self, limit: i64, offset: i64) -> Result<Vec<QueueItem>> {
        let rows = sqlx::query_as::<_, QueueItem>(
            r#"
            SELECT
                id AS job_id,
                workspace_id,
                prompt,
                priority,
                schedule_at,
                queue_rank,
                created_at
            FROM (
                SELECT
                    id,
                    workspace_id,
                    prompt,
                    priority,
                    schedule_at,
                    created_at,
                    row_number() OVER (
                        ORDER BY priority ASC, created_at ASC
                    ) AS queue_rank
                FROM jobs
                WHERE status = 'queued'
                  AND (schedule_at IS NULL OR schedule_at <= now())
            ) queued
            ORDER BY queue_rank ASC
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn update_queue_priority(&self, job_id: Uuid, priority: i32) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
            SET priority = $2, updated_at = now()
            WHERE id = $1
              AND status = 'queued'
            "#,
        )
        .bind(job_id)
        .bind(priority)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}
