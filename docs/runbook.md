# Otter Runbook

## Incident: Jobs Stuck in Queued

- Check Redis health and connectivity from worker.
- Check worker logs for dequeue errors.
- Verify worker is running and not crash-looping.
- Requeue manually by pushing job IDs if needed.

## Incident: Jobs Failing Repeatedly

- Inspect `/v1/jobs/{id}/events` for error payloads.
- Validate `vibe` binary availability in worker runtime.
- Validate workspace path still exists and is accessible.
- Confirm Vibe API key/config available in isolated `VIBE_HOME`.

## Incident: PostgreSQL Unavailable

- Check container/service status and disk pressure.
- Restore from latest backup if corruption occurred.
- Restart server/worker after DB recovery.

## Incident: High Queue Backlog

- Scale worker replicas.
- Reduce prompt concurrency load at client ingress.
- Prioritize urgent jobs with `priority` policy extensions.

## Routine Checks

- Health endpoint returns `200`.
- Successful prompt-to-output cycle in under SLO target.
- Backup jobs completed and verified.
- Disk usage under alert thresholds.
