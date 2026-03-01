# Otter Architecture

## Overview

Otter is a Rust-first orchestration layer for Mistral Vibe programmatic execution. It exposes a REST API for prompt intake, stores execution metadata and history in PostgreSQL, queues work in Redis, and executes jobs through `vibe --prompt` from an isolated workspace context.

## Components

- `otter-server`: HTTP API (`axum`) and orchestration entrypoint.
- `otter-worker`: asynchronous queue consumer and job executor.
- `otter-core`: shared domain, persistence, queue, trust isolation, and Vibe execution logic.
- PostgreSQL: source of truth for projects, workspaces, jobs, outputs, and events.
- Redis: queue transport for asynchronous job dispatch.
- Parameterizable parallel workers via `OTTER_WORKER_CONCURRENCY`.
- SSE broadcaster from persisted job events, consumed by Seal for live updates.

## Execution Flow

1. Client submits prompt via `POST /v1/prompts`.
2. Server validates input and persists a `queued` job.
3. Server pushes job id to Redis queue.
4. Worker pops queue item, claims the corresponding queued DB row atomically, and marks job `running`.
5. Worker runs Vibe with isolated trust context (`VIBE_HOME`) and streams stdout/stderr chunks into `job_events` (`output_chunk`).
6. Worker stores final output and marks `succeeded` / `failed` / retry queued.
7. Client retrieves state with `GET /v1/jobs/{id}`, `GET /v1/jobs/{id}/events`, `GET /v1/history`, or `GET /v1/events/stream`.
8. Queue-aware clients consume `GET /v1/queue` + `PATCH /v1/queue/{id}` for rank-based orchestration.

## Reliability Pattern

- DB-backed status transitions for idempotent state progression.
- Retries with attempt counter and capped total attempts.
- Event stream persisted for auditability and operational triage.
- Cancel endpoint for queued/running jobs.
- Guarded state transitions to prevent cancelled jobs being overwritten by stale worker completions.

## Security Model

- Workspace path canonicalization and optional allowlist roots.
- Per-workspace isolated `VIBE_HOME` path.
- Explicit trusted folder materialization per isolated Vibe home.
- No direct modification of global `~/.vibe` state by default.

## Workspace Discovery Behavior

- `OTTER_DEFAULT_WORKSPACE_PATH` enables "auto workspace" enqueue flow.
- `GET /v1/workspaces` now syncs top-level directories under the default root into workspace records (hidden/internal dirs are skipped).
- Workspace filesystem browsing is exposed through:
  - `GET /v1/workspaces/{id}/tree`
  - `GET /v1/workspaces/{id}/file`

## Observability

- Structured lifecycle logs from `otter-core` (accepted, queued, claimed, retry, failed, completed).
- HTTP request tracing from `otter-server` using `tower-http` trace middleware (status + latency).
