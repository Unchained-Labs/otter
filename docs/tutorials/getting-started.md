# Tutorial: Getting Started

This tutorial sets up otter locally and validates a full prompt-to-result cycle.

## 1. Configure environment

```bash
cp .env.example .env
# set MISTRAL_API_KEY and required URLs
```

## 2. Install and bootstrap host runtime

```bash
./scripts/install_mistral_vibe.sh
./scripts/bootstrap_host_vibe_home.sh
```

## 3. Start stack

```bash
docker compose up --build
```

## 4. Verify health

```bash
curl http://localhost:8080/healthz
```

## 5. Run smoke checks

```bash
./scripts/otter-cli.sh smoke
./scripts/otter-cli.sh projects list
./scripts/otter-cli.sh queue list 20 0
```

## 6. Submit first prompt

Use `POST /v1/prompts` with workspace and prompt payload, then observe:

- `GET /v1/jobs/{id}`
- `GET /v1/jobs/{id}/events`
- `GET /v1/events/stream`
