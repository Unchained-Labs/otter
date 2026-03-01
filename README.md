# Otter

Otter is a Rust orchestration service and reusable library for running Mistral Vibe programmatically at scale.

It accepts prompts through HTTP endpoints, queues and schedules work, executes `vibe --prompt` in isolated trusted workspaces, and stores full execution history in PostgreSQL.

## Features

- Rust workspace architecture:
  - `otter-core`: orchestration domain library.
  - `otter-server`: HTTP API.
  - `otter-worker`: async queue consumer.
- PostgreSQL persistence for projects, workspaces, jobs, outputs, and events.
- Redis-backed queueing and worker retry lifecycle.
- Isolated per-workspace `VIBE_HOME` trust model.
- Streaming execution chunks (`output_chunk`) via SSE.
- Workspace filesystem APIs (`tree` / `file`) for frontend explorer UX.
- Request tracing + lifecycle logs for operations visibility.
- Dockerized runtime with Compose stack for local and NUC deployment.

## Quick Start (Docker Compose)

Prepare local secrets:

```bash
cp .env.example .env
# edit .env and set MISTRAL_API_KEY
```

Install Mistral Vibe on host and bootstrap host `~/.vibe`:

```bash
./scripts/install_mistral_vibe.sh
./scripts/bootstrap_host_vibe_home.sh
```

Use the local endpoint CLI for quick API tests:

```bash
./scripts/otter-cli.sh smoke
./scripts/otter-cli.sh projects list
./scripts/otter-cli.sh queue list 20 0
```

```bash
docker compose up --build
```

Check health:

```bash
curl http://localhost:8080/healthz
```

## Environment Variables

- `OTTER_DATABASE_URL` (required), example: `postgres://otter:otter@postgres:5432/otter`
- `OTTER_REDIS_URL` (required), example: `redis://redis:6379`
- `OTTER_LISTEN_ADDR` (default `0.0.0.0:8080`)
- `OTTER_CORS_ALLOWED_ORIGINS` (default `http://localhost:5173`, comma-separated)
- `OTTER_VIBE_BIN` (default `vibe`)
- `OTTER_VIBE_HOME_BASE` (default `/var/lib/otter/vibe`)
- `OTTER_VIBE_MODEL` (optional, example `mistral-large-3`)
- `OTTER_VIBE_PROVIDER` (optional, example `mistral`)
- `OTTER_VIBE_EXTRA_ENV` (optional, comma-separated `KEY=VALUE` pairs forwarded to vibe process)
- `OTTER_DEFAULT_WORKSPACE_PATH` (optional fallback workspace root when enqueue omits `workspace_id`)
- `OTTER_MAX_ATTEMPTS` (default `5`)
- `OTTER_WORKER_CONCURRENCY` (default `1`)
- `OTTER_ALLOWED_ROOTS` (optional `:`-separated allowlist)
- `MISTRAL_API_KEY` (read from `.env`, passed to server/worker/vibe process)

### Selecting a Vibe model

Otter now supports explicit model/provider passthrough to the Vibe subprocess via environment:

```bash
OTTER_VIBE_MODEL=mistral-large-3
OTTER_VIBE_PROVIDER=mistral
```

These are forwarded as `VIBE_MODEL` / `MISTRAL_MODEL` and `VIBE_PROVIDER` for compatibility.

## API Endpoints (MVP)

- `POST /v1/projects`, `GET /v1/projects`
- `POST /v1/workspaces`, `GET /v1/workspaces`
- `GET /v1/workspaces/{id}/tree`, `GET /v1/workspaces/{id}/file`
- `POST /v1/prompts` (`workspace_id` optional when `OTTER_DEFAULT_WORKSPACE_PATH` is configured)
- `GET /v1/jobs/{id}`
- `GET /v1/jobs/{id}/events`
- `GET /v1/events/stream` (includes `output_chunk` events)
- `POST /v1/jobs/{id}/cancel`
- `GET /v1/queue`
- `PATCH /v1/queue/{id}` (update queue position via priority)
- `GET /v1/history`

## Repository Structure

```text
otter/
├── otter-core/
├── otter-server/
├── otter-worker/
├── docs/
├── docker-compose.yml
└── Dockerfile
```

## Documentation

- `docs/architecture.md`
- `docs/api.md`
- `docs/workspace-trust-model.md`
- `docs/prompt-to-result-flow.md`
- `docs/operations-nuc.md`
- `docs/runbook.md`

## Local Development

The current repository targets modern Rust toolchains. Build with stable Rust:

```bash
cargo check
```

Run services:

```bash
cargo run -p otter-server
cargo run -p otter-worker
```

Install and run pre-commit hooks:

```bash
pip install pre-commit
pre-commit install
pre-commit run --all-files
```

## Notes

- Worker execution requires the `vibe` binary to be available in the runtime environment.
- Docker image installs `mistral-vibe` and exposes `vibe` in `PATH`.
- `scripts/bootstrap_host_vibe_home.sh` mirrors `/home/wardn/.vibe` into `${HOME}/.vibe` and writes `MISTRAL_API_KEY` from `otter/.env`.
- In production, put the API behind authentication and TLS termination.
