# Tutorial: Runtime Operations

This guide covers runtime-level controls used by Seal and operators.

## Start, stop, restart runtime

Endpoints:

- `POST /v1/runtime/workspaces/{id}/start`
- `POST /v1/runtime/workspaces/{id}/stop`
- `POST /v1/runtime/workspaces/{id}/restart`

## Inspect runtime health

Use:

- `GET /v1/runtime/workspaces/{id}`
- `GET /v1/runtime/workspaces/{id}/logs?tail=200`

## Operate shell sessions

Interactive execution path:

- `GET /v1/runtime/workspaces/{id}/shell/ws`

HTTP fallback command path:

- `POST /v1/workspaces/{id}/command`

## Operational recommendations

- Keep runtime logs attached to job context for correlation.
- Preserve shell session IDs when running multi-command workflows.
- Use preview URL API when runtime app endpoint becomes available:
  - `POST /v1/jobs/{job_id}/preview-url`
