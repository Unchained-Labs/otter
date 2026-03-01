# Intel NUC 24/7 Operations

## Host Baseline

- Ubuntu LTS with unattended security updates.
- Docker Engine + Compose plugin.
- Dedicated non-root service user.
- Filesystem layout:
  - `/opt/otter` for compose stack.
  - `/opt/otter/workspaces` for mounted repositories.
  - `/opt/otter/backups` for DB snapshots.

## Deployment Pattern

1. Clone repository to `/opt/otter/repo`.
2. Copy and adjust environment values.
3. Start stack with `docker compose up -d`.
4. Verify:
   - `curl http://localhost:8080/healthz`
   - queue worker logs show startup.

## Recommended Hardening

- Restrict API access with reverse proxy + auth (e.g., Caddy/Nginx + mTLS/OIDC).
- Firewall only required ports.
- Use Docker secrets for credentials (not plain env in production).
- Pin image tags and roll updates with health checks.

## Monitoring

- Container logs to journald or centralized sink.
- Alert on:
  - worker crash loops
  - queue backlog growth
  - DB unavailable
  - repeated job failures

## Backups

- PostgreSQL daily dump + WAL-aware strategy if strict RPO required.
- Backup `vibe_state` volume when workspace trust/audit continuity matters.
- Test restore monthly.

## Upgrade Playbook

1. Pull latest code/images.
2. Run migrations in staging.
3. Deploy during maintenance window.
4. Validate health + basic prompt workflow.
5. Roll back if job success rate regresses.
