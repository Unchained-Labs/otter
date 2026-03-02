# API Guide

This page complements the raw REST reference in [api.md](/api).

## API Surface Categories

- **Workspace management**: create/list workspaces, browse files, run commands.
- **Prompt orchestration**: enqueue prompts, inspect jobs, stream events.
- **Queue control**: inspect queue rank, update priorities, pause/resume/cancel.
- **Runtime control**: container lifecycle, logs, shell channels.

## Request Patterns

### Synchronous management

Use direct endpoints for state inspection and control:

- `GET /v1/workspaces`
- `GET /v1/jobs/{id}`
- `PATCH /v1/queue/{id}`

### Asynchronous execution

Use prompt enqueue and event streaming:

- `POST /v1/prompts`
- `GET /v1/events/stream`

## Integration Guidance

- For dashboards and UIs, combine snapshot polling with SSE.
- For automation, persist job IDs and poll final state with timeout policy.
- For runtime shells, maintain session IDs for cwd continuity.
