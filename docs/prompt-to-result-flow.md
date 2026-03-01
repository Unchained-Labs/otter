# Prompt to Result Flow

## End-to-end lifecycle

1. Client sends `POST /v1/prompts` with:
   - `prompt` (required)
   - `priority` and `schedule_at` (optional)
   - `workspace_id` (optional; server can resolve default workspace)
2. Otter validates payload and resolves workspace context:
   - If `workspace_id` is provided, that workspace is used.
   - If missing, Otter uses `OTTER_DEFAULT_WORKSPACE_PATH` and auto-creates/reuses a default workspace record.
3. Otter inserts a `queued` job in PostgreSQL and records `accepted` + `queued` events.
4. Otter enqueues job id in Redis.
5. Worker dequeues job id from Redis, atomically claims that queued DB row, and marks it `running`.
6. Worker executes:
   - `vibe --prompt "<prompt>" --output json --workdir <workspace_root>`
   - with `VIBE_HOME=<isolated_workspace_home>`
7. While the process is running, stdout/stderr lines are emitted as `output_chunk` events and persisted in `job_events`.
8. Worker persists result and transitions status:
   - success -> `succeeded` + `job_outputs` row + `completed` event
   - failure with retries left -> back to `queued` + `retry_queued` event
   - terminal failure -> `failed` + `failed` event
9. Clients observe state via:
   - `GET /v1/jobs/{id}`
   - `GET /v1/jobs/{id}/events`
   - `GET /v1/queue`
   - `GET /v1/history`
   - `GET /v1/events/stream` (SSE)

## Data crossing boundaries

- Client -> API:
  - prompt payload (`prompt`, optional `workspace_id`, queue metadata)
- API -> PostgreSQL:
  - canonical job state (`status`, attempts, errors, timestamps)
  - immutable event trail (`job_events`)
  - final output snapshot (`job_outputs`)
- API -> Redis:
  - queue wake-up message containing job id
- Worker -> Vibe:
  - prompt text
  - execution directory (`--workdir`)
  - isolated trust context (`VIBE_HOME`)
- Vibe -> Worker:
  - JSON and assistant output
  - incremental stdout/stderr chunks
  - process exit information (stderr/exit code)
- API -> Client:
  - REST snapshots + live SSE events including incremental `output_chunk`

## Why workspaces still exist

Workspaces are an execution and trust boundary, not just UI metadata:

- they bind a job to a real filesystem root where Vibe can operate,
- they isolate `VIBE_HOME` trust state per workspace to avoid leakage across projects,
- they allow allowlist enforcement (`OTTER_ALLOWED_ROOTS`) and safer multi-project operation.

If you want a simpler model, use `OTTER_DEFAULT_WORKSPACE_PATH` and omit `workspace_id` from the frontend; Otter routes jobs through that default context and can sync workspace records from top-level directories under that root.
