# Concepts

## Orchestration as Control Plane

otter behaves as a control plane over execution environments:

- APIs define intent and policy.
- Workers execute intent against concrete workspaces.
- Persistence enforces durable lifecycle semantics.

## Durable Lifecycle Model

Each job transitions through explicit states with guarded transitions. This prevents stale worker updates or duplicate queue signals from corrupting final state.

Key lifecycle principles:

- state transitions are persisted, not inferred.
- retries are bounded and observable.
- cancellation/pause/resume are first-class operations.

## Streaming Observability

`output_chunk` and lifecycle events are streamed via SSE and stored in persistence. This provides:

- real-time UX feedback,
- auditability,
- post-incident replay context.

## Workspace Trust Boundaries

Execution context is isolated per workspace:

- path canonicalization and optional root allowlists,
- isolated `VIBE_HOME`,
- explicit workspace APIs for filesystem and command execution.

## Queue Control Semantics

otter supports:

- rank-based queue ordering,
- priority updates,
- pausing queued jobs without marking them failed,
- resume with requeue semantics.
