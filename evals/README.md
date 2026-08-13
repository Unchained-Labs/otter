# Prompt-to-build evals

A regression suite for the thing Otter is actually judged on: given a prompt,
does a working application come out the other end?

## Why delivery, not success

`status == "succeeded"` only means the agent process exited zero. It does not
mean anything is running. The agent is instructed to containerise its build,
start it, verify the port responds, and register the result via
`POST /v1/jobs/{id}/preview-url`.

So the headline metric here is **delivery rate** — jobs that succeeded *and*
published a preview URL — not success rate. The gap between the two is the
interesting signal: it is exactly the population of runs that looked fine and
shipped nothing. Both are exposed by the control plane at `/metrics` as
`otter_success_rate` and `otter_delivery_rate`.

## Running

Requires a live stack with a working `MISTRAL_API_KEY`. These are real builds:
the full suite costs real tokens and takes tens of minutes.

```bash
# See what would run, and check the control plane is reachable
./run_evals.py --dry-run

# Fastest useful signal: two small scenarios
./run_evals.py --tier smoke

# Everything, with a JSON report and a regression gate
./run_evals.py --report report.json --min-delivery-rate 0.8

# One scenario while iterating
./run_evals.py --only fastapi-health
```

Exit codes: `0` all scenarios passed, `1` a scenario failed or the delivery-rate
threshold was missed, `2` the selection matched no scenarios.

## Tiers

| Tier | Intent | Rough cost |
|---|---|---|
| `smoke` | Does the pipeline work at all? Small, unambiguous builds. | Minutes |
| `core` | Representative real work: a CRUD API, a built frontend. | ~15 min |
| `hard` | Multi-container and deliberately underspecified prompts. | ~30 min |

`ambiguous-prompt` is intentionally vague ("make me something that tracks
stuff"). It is not a trick: an agent that stalls on under-specification is
useless for voice input, where prompts are conversational by nature.

## Scenario format

```json
{
  "id": "fastapi-health",
  "tier": "smoke",
  "prompt": "Build a FastAPI service with ...",
  "timeout_seconds": 900,
  "expect": {
    "delivered": true,
    "max_duration_seconds": 600,
    "max_cost_usd": 0.50
  }
}
```

| Field | Meaning |
|---|---|
| `timeout_seconds` | Give up polling and record `timeout` |
| `expect.delivered` | Require success *and* a preview URL |
| `expect.max_duration_seconds` | Wall-clock budget |
| `expect.max_cost_usd` | Spend budget; skipped when the model has no configured price |

Every violated expectation is reported, not just the first — a run that both
failed to deliver and blew its budget tells you more than one that stops at the
first problem.

Cost checks need `OTTER_MODEL_PRICING` set on the server (see the observability
section of the Otter docs). Without it, token counts are still recorded and cost
expectations are skipped rather than treated as passing at zero.

## Tests

The scoring logic is unit tested and needs no live stack:

```bash
python3 -m unittest discover -s evals -p 'test_*.py'
```

This also validates the shipped `scenarios.json` — unique ids, required fields,
and duration budgets that actually fit inside their timeouts.

## Interpreting a run

- **Delivery rate dropped, success rate held** — the agent is finishing without
  shipping. Usually a preview-URL registration or port-binding regression.
- **Durations climbed, delivery held** — model or prompt regression; check
  `otter_job_duration_ms_avg` over the same window.
- **A single tier failed** — compare against the prompt wording before assuming
  the control plane changed.
