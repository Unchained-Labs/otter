use anyhow::{bail, Result};
use serde_json::Value;
use sqlx::postgres::{PgPoolOptions, PgQueryResult};
use sqlx::PgPool;
use sqlx::Row;
use std::path::PathBuf;
use uuid::Uuid;

use crate::domain::{
    CreateProjectRequest, CreateWorkspaceRequest, HistoryItem, Job, JobEvent, JobOutput,
    JobRuntimeAppRegistryEntry, JobStatus, Project, QueueItem, RuntimePortBinding, Workspace,
    WorkspaceRuntimeRegistryEntry,
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

    pub async fn find_project_by_name(&self, name: &str) -> Result<Option<Project>> {
        let project = sqlx::query_as::<_, Project>(
            "SELECT id, name, description, created_at FROM projects WHERE name = $1 LIMIT 1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(project)
    }

    pub async fn create_workspace(
        &self,
        workspace_id: Uuid,
        request: CreateWorkspaceRequest,
        canonical_root_path: String,
        isolated_vibe_home: String,
    ) -> Result<Workspace> {
        let workspace = sqlx::query_as::<_, Workspace>(
            r#"
            INSERT INTO workspaces (id, project_id, name, root_path, isolated_vibe_home)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, project_id, name, root_path, isolated_vibe_home, created_at
            "#,
        )
        .bind(workspace_id)
        .bind(request.project_id)
        .bind(request.name)
        .bind(canonical_root_path)
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

    pub async fn find_workspace_by_root_path(&self, root_path: &str) -> Result<Option<Workspace>> {
        let workspace = sqlx::query_as::<_, Workspace>(
            r#"
            SELECT id, project_id, name, root_path, isolated_vibe_home, created_at
            FROM workspaces
            WHERE root_path = $1
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(root_path)
        .fetch_optional(&self.pool)
        .await?;
        Ok(workspace)
    }

    pub async fn create_job(
        &self,
        workspace_id: Uuid,
        prompt: &str,
        priority: Option<i32>,
        schedule_at: Option<chrono::DateTime<chrono::Utc>>,
        project_path: Option<&str>,
        max_attempts: i32,
    ) -> Result<Job> {
        let job = sqlx::query_as::<_, Job>(
            r#"
            INSERT INTO jobs (workspace_id, prompt, status, priority, schedule_at, project_path, attempts, max_attempts)
            VALUES ($1, $2, 'queued', $3, $4, $5, 0, $6)
            RETURNING id, workspace_id, prompt, preview_url, project_path, runtime_start_command, runtime_stop_command, runtime_command_cwd, is_paused, status, priority, schedule_at, attempts, max_attempts, error, created_at, updated_at
            "#,
        )
        .bind(workspace_id)
        .bind(prompt)
        .bind(priority.unwrap_or(100))
        .bind(schedule_at)
        .bind(project_path)
        .bind(max_attempts)
        .fetch_one(&self.pool)
        .await?;
        Ok(job)
    }

    pub async fn fetch_job(&self, job_id: Uuid) -> Result<Option<Job>> {
        let job = sqlx::query_as::<_, Job>(
            r#"
            SELECT id, workspace_id, prompt, preview_url, project_path, runtime_start_command, runtime_stop_command, runtime_command_cwd, is_paused, status, priority, schedule_at, attempts, max_attempts, error, created_at, updated_at
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
              j.status,
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

    pub async fn list_job_events_since(
        &self,
        since: chrono::DateTime<chrono::Utc>,
        limit: i64,
    ) -> Result<Vec<JobEvent>> {
        let events = sqlx::query_as::<_, JobEvent>(
            r#"
            SELECT id, job_id, event_type, payload, created_at
            FROM job_events
            WHERE created_at > $1
            ORDER BY created_at ASC
            LIMIT $2
            "#,
        )
        .bind(since)
        .bind(limit)
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

    pub async fn claim_queued_job_by_id(&self, job_id: Uuid) -> Result<Option<Job>> {
        let job = sqlx::query_as::<_, Job>(
            r#"
            UPDATE jobs j
            SET status = 'running',
                updated_at = now()
            WHERE j.id = $1
              AND j.status = 'queued'
              AND j.is_paused = false
              AND (j.schedule_at IS NULL OR j.schedule_at <= now())
              AND NOT EXISTS (
                SELECT 1
                FROM job_dependencies dep
                JOIN jobs parent ON parent.id = dep.depends_on_job_id
                WHERE dep.job_id = j.id
                  AND parent.status <> 'succeeded'
              )
            RETURNING j.id, j.workspace_id, j.prompt, j.preview_url, j.project_path, j.runtime_start_command, j.runtime_stop_command, j.runtime_command_cwd, j.is_paused, j.status, j.priority, j.schedule_at, j.attempts, j.max_attempts, j.error, j.created_at, j.updated_at
            "#,
        )
        .bind(job_id)
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
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let result =
            sqlx::query("UPDATE jobs SET status = 'succeeded', updated_at = now() WHERE id = $1 AND status = 'running'")
            .bind(job_id)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO job_outputs (job_id, assistant_output, raw_json) VALUES ($1, $2, $3)",
        )
        .bind(job_id)
        .bind(assistant_output)
        .bind(raw_json)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn fail_job(&self, job_id: Uuid, error: String) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
            SET status = 'failed', attempts = attempts + 1, error = $2, updated_at = now()
            WHERE id = $1
              AND status = 'running'
            "#,
        )
        .bind(job_id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn set_job_back_to_queue(&self, job_id: Uuid, error: String) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
            SET status = 'queued', attempts = attempts + 1, error = $2, updated_at = now()
            WHERE id = $1
              AND status = 'running'
            "#,
        )
        .bind(job_id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
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

    pub async fn has_unresolved_dependencies(&self, job_id: Uuid) -> Result<bool> {
        let blocked = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS (
                SELECT 1
                FROM job_dependencies dep
                JOIN jobs parent ON parent.id = dep.depends_on_job_id
                WHERE dep.job_id = $1
                  AND parent.status <> 'succeeded'
            )
            "#,
        )
        .bind(job_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(blocked)
    }

    pub async fn list_queue(&self, limit: i64, offset: i64) -> Result<Vec<QueueItem>> {
        let rows = sqlx::query_as::<_, QueueItem>(
            r#"
            SELECT
                id AS job_id,
                workspace_id,
                prompt,
                is_paused,
                blocked_by_dependencies,
                dependency_count,
                unresolved_dependency_count,
                priority,
                schedule_at,
                queue_rank,
                created_at
            FROM (
                SELECT
                    id,
                    workspace_id,
                    prompt,
                    is_paused,
                    EXISTS (
                        SELECT 1
                        FROM job_dependencies dep
                        JOIN jobs parent ON parent.id = dep.depends_on_job_id
                        WHERE dep.job_id = jobs.id
                          AND parent.status <> 'succeeded'
                    ) AS blocked_by_dependencies,
                    (
                        SELECT COUNT(*)
                        FROM job_dependencies dep
                        WHERE dep.job_id = jobs.id
                    )::bigint AS dependency_count,
                    (
                        SELECT COUNT(*)
                        FROM job_dependencies dep
                        JOIN jobs parent ON parent.id = dep.depends_on_job_id
                        WHERE dep.job_id = jobs.id
                          AND parent.status <> 'succeeded'
                    )::bigint AS unresolved_dependency_count,
                    priority,
                    schedule_at,
                    created_at,
                    row_number() OVER (
                        ORDER BY priority ASC, created_at ASC
                    ) AS queue_rank
                FROM jobs
                WHERE status = 'queued'
                  AND (schedule_at IS NULL OR schedule_at <= now() OR is_paused = true)
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

    pub async fn pause_queued_job(&self, job_id: Uuid) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
            SET is_paused = true, updated_at = now()
            WHERE id = $1
              AND status = 'queued'
            "#,
        )
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn resume_queued_job(&self, job_id: Uuid) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
            SET is_paused = false, updated_at = now()
            WHERE id = $1
              AND status = 'queued'
            "#,
        )
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn hold_job(&self, job_id: Uuid) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
            SET status = CASE WHEN status = 'running' THEN 'queued' ELSE status END,
                is_paused = true,
                updated_at = now()
            WHERE id = $1
              AND status IN ('queued', 'running')
            "#,
        )
        .bind(job_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn add_job_dependencies(&self, job_id: Uuid, dependency_ids: &[Uuid]) -> Result<()> {
        if dependency_ids.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for dependency_id in dependency_ids {
            if dependency_id == &job_id {
                bail!("job cannot depend on itself");
            }
            let exists =
                sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM jobs WHERE id = $1)")
                    .bind(dependency_id)
                    .fetch_one(&mut *tx)
                    .await?;
            if !exists {
                bail!("dependency job not found: {dependency_id}");
            }
            sqlx::query(
                r#"
                INSERT INTO job_dependencies (job_id, depends_on_job_id)
                VALUES ($1, $2)
                ON CONFLICT DO NOTHING
                "#,
            )
            .bind(job_id)
            .bind(dependency_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn list_job_dependency_ids(&self, job_id: Uuid) -> Result<Vec<Uuid>> {
        let rows = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT depends_on_job_id
            FROM job_dependencies
            WHERE job_id = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn set_job_project_path(&self, job_id: Uuid, project_path: &str) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
            SET project_path = $2, updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .bind(project_path)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn set_job_runtime_launch_config(
        &self,
        job_id: Uuid,
        start_command: &str,
        stop_command: Option<&str>,
        working_directory: Option<&str>,
    ) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
            SET runtime_start_command = $2,
                runtime_stop_command = $3,
                runtime_command_cwd = $4,
                updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .bind(start_command)
        .bind(stop_command)
        .bind(working_directory)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn set_job_preview_url(&self, job_id: Uuid, preview_url: &str) -> Result<bool> {
        let result = sqlx::query(
            r#"
            UPDATE jobs
            SET preview_url = $2, updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .bind(preview_url)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    fn ports_to_json(ports: &[RuntimePortBinding]) -> Result<Value> {
        serde_json::to_value(ports).map_err(|error| anyhow::anyhow!(error))
    }

    fn ports_from_json(ports: Value) -> Result<Vec<RuntimePortBinding>> {
        serde_json::from_value(ports).map_err(|error| anyhow::anyhow!(error))
    }

    pub async fn upsert_workspace_runtime_registry(
        &self,
        workspace_id: Uuid,
        container_name: &str,
        image_tag: &str,
        status: &str,
        preferred_url: Option<&str>,
        ports: &[RuntimePortBinding],
    ) -> Result<()> {
        let ports_json = Self::ports_to_json(ports)?;
        sqlx::query(
            r#"
            INSERT INTO workspace_runtime_registry (
                workspace_id,
                container_name,
                image_tag,
                status,
                preferred_url,
                ports,
                updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, now())
            ON CONFLICT (workspace_id) DO UPDATE SET
                container_name = EXCLUDED.container_name,
                image_tag = EXCLUDED.image_tag,
                status = EXCLUDED.status,
                preferred_url = EXCLUDED.preferred_url,
                ports = EXCLUDED.ports,
                updated_at = now()
            "#,
        )
        .bind(workspace_id)
        .bind(container_name)
        .bind(image_tag)
        .bind(status)
        .bind(preferred_url)
        .bind(ports_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_job_runtime_app_registry(
        &self,
        job_id: Uuid,
        workspace_id: Uuid,
        working_directory: Option<&str>,
        start_command: &str,
        stop_command: Option<&str>,
        status: &str,
        preferred_url: Option<&str>,
        ports: &[RuntimePortBinding],
    ) -> Result<()> {
        let ports_json = Self::ports_to_json(ports)?;
        sqlx::query(
            r#"
            INSERT INTO job_runtime_app_registry (
                job_id,
                workspace_id,
                working_directory,
                start_command,
                stop_command,
                status,
                preferred_url,
                ports,
                updated_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
            ON CONFLICT (job_id) DO UPDATE SET
                workspace_id = EXCLUDED.workspace_id,
                working_directory = EXCLUDED.working_directory,
                start_command = EXCLUDED.start_command,
                stop_command = EXCLUDED.stop_command,
                status = EXCLUDED.status,
                preferred_url = EXCLUDED.preferred_url,
                ports = EXCLUDED.ports,
                updated_at = now()
            "#,
        )
        .bind(job_id)
        .bind(workspace_id)
        .bind(working_directory)
        .bind(start_command)
        .bind(stop_command)
        .bind(status)
        .bind(preferred_url)
        .bind(ports_json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_workspace_runtime_registries(
        &self,
    ) -> Result<Vec<WorkspaceRuntimeRegistryEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT
                workspace_id,
                container_name,
                image_tag,
                status,
                preferred_url,
                ports
            FROM workspace_runtime_registry
            ORDER BY updated_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let workspace_id: Uuid = row.try_get("workspace_id")?;
            let container_name: String = row.try_get("container_name")?;
            let image_tag: String = row.try_get("image_tag")?;
            let status: String = row.try_get("status")?;
            let preferred_url: Option<String> = row.try_get("preferred_url")?;
            let ports_json: Value = row.try_get("ports")?;
            let ports = Self::ports_from_json(ports_json)?;

            out.push(WorkspaceRuntimeRegistryEntry {
                workspace_id,
                container_name,
                image_tag,
                status,
                preferred_url,
                ports,
            });
        }

        Ok(out)
    }

    pub async fn list_job_runtime_app_registries(&self) -> Result<Vec<JobRuntimeAppRegistryEntry>> {
        let rows = sqlx::query(
            r#"
            SELECT
                job_id,
                workspace_id,
                working_directory,
                start_command,
                stop_command,
                status,
                preferred_url,
                ports
            FROM job_runtime_app_registry
            ORDER BY updated_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let job_id: Uuid = row.try_get("job_id")?;
            let workspace_id: Uuid = row.try_get("workspace_id")?;
            let working_directory: Option<String> = row.try_get("working_directory")?;
            let start_command: String = row.try_get("start_command")?;
            let stop_command: Option<String> = row.try_get("stop_command")?;
            let status: String = row.try_get("status")?;
            let preferred_url: Option<String> = row.try_get("preferred_url")?;
            let ports_json: Value = row.try_get("ports")?;
            let ports = Self::ports_from_json(ports_json)?;

            out.push(JobRuntimeAppRegistryEntry {
                job_id,
                workspace_id,
                working_directory,
                start_command,
                stop_command,
                status,
                preferred_url,
                ports,
            });
        }

        Ok(out)
    }
}

#[cfg(test)]
mod registry_tests {
    use super::Database;
    use crate::domain::RuntimePortBinding;

    #[test]
    fn ports_roundtrip_serializes_and_parses() {
        let ports = vec![RuntimePortBinding {
            container_port: 3000,
            host_ip: "0.0.0.0".to_string(),
            host_port: 49162,
        }];

        let json = Database::ports_to_json(&ports).unwrap();
        let parsed = Database::ports_from_json(json).unwrap();

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].container_port, 3000);
        assert_eq!(parsed[0].host_ip, "0.0.0.0");
        assert_eq!(parsed[0].host_port, 49162);
    }
}
