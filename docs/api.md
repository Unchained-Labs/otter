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

## Jobs

- `GET /v1/jobs/{id}`
  - Returns job metadata, latest output payload if available, and `queue_rank` while queued.
- `POST /v1/jobs/{id}/cancel`
  - Cancels queued/running jobs.
- `GET /v1/jobs/{id}/events`
  - Returns ordered lifecycle events.
- `GET /v1/events/stream`
  - Server-Sent Events stream of job lifecycle events for live UI updates.

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
