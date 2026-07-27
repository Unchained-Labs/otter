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
  - Returns job metadata (including `is_paused` and optional `preview_url`), latest output payload if available, and `queue_rank` while queued.
- `POST /v1/jobs/{id}/cancel`
  - Cancels queued/running jobs.
- `POST /v1/jobs/{id}/pause`
  - Pauses a queued job so it remains queued but not runnable by workers.
- `POST /v1/jobs/{id}/resume`
  - Resumes a paused queued job and re-enqueues it for execution.
- `POST /v1/jobs/{id}/preview-url`
  - Sets a demo URL for job browser preview.
  - Body:
    ```json
    { "preview_url": "http://host:port" }
    ```
  - URL must be absolute and use `http` or `https`.
- `GET /v1/jobs/{id}/events`
  - Returns ordered lifecycle events.
- `GET /v1/events/stream`
  - Server-Sent Events stream of job lifecycle events for live UI updates.
  - Includes incremental `output_chunk` events (`stdout` / `stderr`) during Vibe execution.
  - Optional `job_id` query parameter restricts the stream to a single job.
  - Events carry a monotonic `seq`; the stream pages on it, so no event is skipped
    when several land in the same microsecond.
  - A new connection starts at the current tail rather than replaying the backlog.

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
  - Workers claim the runnable job with the lowest `priority`, so repositioning
    changes execution order, not just display order. Ties are broken by aged
    intensity — see [Task Complexity And Scheduling](#task-complexity-and-scheduling).

## Operational Visibility

- HTTP request tracing is enabled server-side (status + latency).
- Job lifecycle logs are emitted by worker/service layers for enqueue/claim/retry/fail/complete states.
- Runtime shell commands auto-recover when workspace container is missing/stopped before falling back to host workspace shell execution.

## Task Complexity And Scheduling

Every job is scored at enqueue by [`otter-complexity`](../otter-complexity/README.md):
`complexity` (1–10), `task_size` (1–10) and `intensity` (0–100), plus a band, an
estimate and the signals behind the score. Scoring is local, deterministic and
adds no measurable latency to enqueue.

- `POST /v1/complexity/score`
  - Body: `{ "prompt": "...", "dependency_count": 0, "scoped_to_project_path": false }`
  - Returns a full assessment without enqueuing anything. Use it to show what a
    task will cost before committing to it, or to rank a backlog externally.
- `POST /v1/jobs/{id}/assessment`
  - Body: `{ "complexity": 7, "size": 4, "confidence": 0.9 }`
  - Overrides the heuristic score. The queue re-orders on the next claim, so a
    correction takes effect immediately for anything still waiting.

### Scheduling

`OTTER_SCHEDULING_STRATEGY` selects how the worker picks the next job:

| Strategy | Order |
|---|---|
| `smart` (default) | `priority`, then aged intensity, then `created_at` |
| `priority` | `priority`, then `created_at` |
| `fifo` | `created_at` only |

Under `smart`, cheap work clears ahead of expensive work, which shortens the
average wait across the queue. Explicit `priority` remains the outermost key:
it is the one signal a human set deliberately, and a heuristic must not override
an explicit decision.

To prevent starvation, a waiting job sheds `OTTER_SCHEDULING_AGING_STEP`
intensity every `OTTER_SCHEDULING_AGING_SECONDS`. The defaults shed the full
0–100 range over roughly three hours, so a large job falls behind newcomers only
for a bounded time. Ageing much faster makes the queue effectively FIFO and
throws away the throughput gain scoring exists to provide.

`GET /v1/queue` returns `intensity`, `complexity_band`, `estimated_minutes` and
`effective_intensity` (intensity after ageing), and ranks rows with the same
ordering the scheduler uses — the board and the worker never disagree.

Jobs enqueued before scoring existed have `NULL` intensity and are treated as
mid-range, so they neither jump the queue nor sink to the bottom.

## Operability

- `GET /healthz` — liveness. Answers whenever the process is up.
- `GET /readyz` — readiness. Exercises the database and reports the queue;
  returns 503 with a per-dependency breakdown when the database is unreachable.
  A queue outage is reported but does not fail readiness: the worker claims from
  the database regardless, so an outage costs latency, not correctness.
- `GET /metrics` — Prometheus text format: job counts by status, paused count,
  mean queued intensity, and total estimated minutes of queued work.
