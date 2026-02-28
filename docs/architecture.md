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

## Execution Flow

1. Client submits prompt via `POST /v1/prompts`.
2. Server validates input and persists a `queued` job.
3. Server pushes job id to Redis queue.
4. Worker pops queue item, marks job `running`, executes Vibe command.
5. Worker stores output/events and marks `succeeded` or `failed`.
6. Client retrieves state with `GET /v1/jobs/{id}`, `GET /v1/jobs/{id}/events`, or `GET /v1/history`.
7. Queue-aware clients consume `GET /v1/queue` + `PATCH /v1/queue/{id}` for rank-based orchestration.

## Reliability Pattern

- DB-backed status transitions for idempotent state progression.
- Retries with attempt counter and capped total attempts.
- Event stream persisted for auditability and operational triage.
- Cancel endpoint for queued/running jobs.

## Security Model

- Workspace path canonicalization and optional allowlist roots.
- Per-workspace isolated `VIBE_HOME` path.
- Explicit trusted folder materialization per isolated Vibe home.
- No direct modification of global `~/.vibe` state by default.
