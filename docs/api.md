# Otter API

Base URL: `http://<host>:8080`

## Health

- `GET /healthz`
  - Returns `200 OK` with `ok`.

## Projects

- `POST /v1/projects`
  - Body:
    ```json
    { "name": "my-project", "description": "optional" }
    ```
- `GET /v1/projects`
  - Lists all projects.

## Workspaces

- `POST /v1/workspaces`
  - Body:
    ```json
    {
      "project_id": "uuid",
      "name": "backend-repo",
      "root_path": "/workspaces/backend-repo"
    }
    ```
  - Creates workspace and initializes isolated Vibe trust context.
- `GET /v1/workspaces`
  - Lists all workspaces.
  - When `OTTER_DEFAULT_WORKSPACE_PATH` is set, top-level directories under that root are synchronized into workspace records.
- `GET /v1/workspaces/{id}/tree?path=&depth=2`
  - Lists directory/file entries under a workspace root (safe canonicalized relative traversal only).
- `GET /v1/workspaces/{id}/file?path=<relative_path>`
  - Returns file content for a workspace-relative file path.
- `POST /v1/workspaces/{id}/command`
  - Runs a shell command in a specific workspace.
- `POST /v1/workspaces/command`
  - Runs a shell command in selected workspace, or auto workspace when `workspace_id` is omitted.
  - Body:
    ```json
    {
      "workspace_id": "uuid-optional",
      "command": "npm run dev",
      "working_directory": "relative/path/optional",
      "timeout_seconds": 120
    }
    ```

## Prompt Queueing

- `POST /v1/prompts`
  - Body:
    ```json
    {
      "workspace_id": "uuid",
      "prompt": "Refactor src/main.rs for modularity",
      "priority": 100,
      "schedule_at": null
    }
    ```
  - Returns accepted job payload.
- `POST /v1/voice/prompts`
  - Multipart form upload of voice audio and optional `workspace_id`.
  - Otter forwards audio to Lavoix STT, then enqueues the transcribed text as a normal prompt.
  - Fields:
    - `file` (required)
    - `workspace_id` (optional)
    - `language` (optional)
    - `provider` (optional)

## Jobs

- `GET /v1/jobs/{id}`
  - Returns job metadata, latest output payload if available, and `queue_rank` while queued.
- `POST /v1/jobs/{id}/cancel`
  - Cancels queued/running jobs.
- `GET /v1/jobs/{id}/events`
  - Returns ordered lifecycle events.
- `GET /v1/events/stream`
  - Server-Sent Events stream of job lifecycle events for live UI updates.
  - Includes incremental `output_chunk` events (`stdout` / `stderr`) during Vibe execution.

## History

- `GET /v1/history?limit=100`
  - Returns recent prompt/output history.

## Queue Management

- `GET /v1/queue?limit=100&offset=0`
  - Returns queued jobs with stable rank order (`queue_rank`) for UI consumption.
- `PATCH /v1/queue/{job_id}`
  - Body:
    ```json
    { "priority": 10 }
    ```
  - Repositions queued jobs by updating priority.

## Operational Visibility

- HTTP request tracing is enabled server-side (status + latency).
- Job lifecycle logs are emitted by worker/service layers for enqueue/claim/retry/fail/complete states.
