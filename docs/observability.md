# Observability

Otter records what each agent run cost and whether it actually shipped
something, and exposes both for scraping.

## Delivery, not just success

`JobStatus::Succeeded` means the agent process exited zero. It does not mean an
application is running.

The agent is instructed to containerise its build, start it, verify the port
responds, and register the result with `POST /v1/jobs/{id}/preview-url`. That
gives a stricter signal: **delivered** — succeeded *and* carrying a non-empty
preview URL.

Both are exposed. The gap between them is the population of runs that finished
cleanly and shipped nothing, which is where regressions actually show up.

## `GET /metrics`

Prometheus text exposition on the conventional unversioned path:

```bash
curl http://localhost:8080/metrics
```

| Metric | Type | Meaning |
|---|---|---|
| `otter_jobs{status}` | gauge | Jobs per lifecycle status |
| `otter_jobs_total` | gauge | All jobs recorded |
| `otter_jobs_delivered_total` | gauge | Succeeded *and* published a preview URL |
| `otter_tokens_total{kind}` | gauge | Prompt and completion tokens consumed |
| `otter_estimated_cost_usd_total` | gauge | Spend across jobs with a configured price |
| `otter_jobs_with_cost_total` | gauge | Jobs contributing to that total |
| `otter_success_rate` | gauge | Succeeded share of terminal jobs |
| `otter_delivery_rate` | gauge | Delivered share of terminal jobs |
| `otter_job_duration_ms_avg` | gauge | Mean duration of succeeded jobs |

Rate and duration series are omitted entirely until at least one job has reached
a terminal state — an absent series is honest, `0` would not be.

### Why database-derived

`otter-server` and `otter-worker` are separate processes. In-process counters in
the server would miss everything the worker does and would reset on every
deploy. Every metric here is a Postgres aggregate computed per scrape, so any
replica reports the same numbers. The cost is one query per scrape, which is
comfortably cheap at normal scrape intervals.

## `GET /v1/metrics/summary`

The same aggregate as JSON, with the derived rates included, for dashboards and
the eval harness:

```json
{
  "summary": { "jobs_total": 8, "jobs_delivered": 2, "tokens_total": 4210, "…": "…" },
  "success_rate": 0.5,
  "delivery_rate": 0.3333333333333333
}
```

## `GET /v1/jobs/{id}/usage`

Per-job accounting. Returns `404` when no usage was recorded for the job.

```json
{
  "job_id": "…",
  "model": "mistral-large-3",
  "prompt_tokens": 1000,
  "completion_tokens": 200,
  "total_tokens": 1200,
  "estimated_cost_usd": 0.1,
  "duration_ms": 10000
}
```

`duration_ms` covers agent execution only — the post-run `setup.sh` hook and
runtime container provisioning are excluded.

## How usage is captured

Token counts are extracted from the streamed transcript persisted in
`job_outputs.raw_json`. Providers do not agree on a shape, so
`otter_core::usage` normalises the common variants:

- top-level `usage`, or nested under `message`, `response`, or `delta`
- `prompt_tokens`/`completion_tokens` and the `input_tokens`/`output_tokens` aliases
- a missing `total_tokens` is derived from the other two

At most one usage object is read **per transcript entry** — providers often
expose the same counts at several nesting levels in one line, and counting each
would inflate the total. Across entries the counts *are* summed, because an
agentic run makes several model calls and each reports its own usage.

Usage is recorded immediately after the agent run, before the setup hook and
runtime provisioning, so a failure in those later steps cannot lose accounting
for work already paid for. Recording is best-effort: a telemetry write failure
is logged and never fails the job.

::: warning Failed runs are not accounted
When the agent process exits non-zero the executor returns an error without a
transcript, so no usage row is written. Token spend on failed runs is therefore
not currently captured.
:::

## Pricing

Cost is derived, never provider-reported, so prices are operator-supplied via
`OTTER_MODEL_PRICING` as `model=input:output` in USD per million tokens:

```bash
OTTER_MODEL_PRICING=mistral-large-3=2.0:6.0,mistral-small-latest=0.2:0.6
```

With no price configured for a model, `estimated_cost_usd` is `null` — never
`0`. Those jobs still contribute token counts and are excluded from
`otter_jobs_with_cost_total`, so the spend total is never quietly understated.

Malformed entries are skipped rather than failing startup: a typo in a price
string should cost you cost reporting, not your control plane.

## Evals

Metrics describe real traffic. The [eval suite](https://github.com/Unchained-Labs/otter/blob/main/evals/README.md)
answers whether a change helped, on demand, by running a fixed set of prompts
and scoring delivery, duration, and cost.

```bash
cd evals
./run_evals.py --dry-run
./run_evals.py --tier smoke
./run_evals.py --report report.json --min-delivery-rate 0.8
```
