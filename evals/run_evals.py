#!/usr/bin/env python3
"""Prompt-to-build eval harness for Otter.

Runs a suite of real prompts against a live Otter deployment and scores each on
the only outcome that matters: did it produce a reachable application?

A job exiting zero is not success. The agent is instructed to start its build
and register a preview URL, so `delivered` (succeeded *and* carrying a preview
URL) is the headline metric, and the gap between the success rate and the
delivery rate is where regressions hide.

Standard library only, so it runs anywhere the stack runs without a virtualenv.

Usage:
    ./run_evals.py --base-url http://localhost:8080
    ./run_evals.py --tier smoke --min-delivery-rate 0.8
    ./run_evals.py --dry-run          # validate scenarios + connectivity only
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Any, Iterable

TERMINAL_STATUSES = {"succeeded", "failed", "cancelled"}
DEFAULT_SCENARIOS = Path(__file__).parent / "scenarios.json"


# --------------------------------------------------------------------------
# Scoring — pure functions, unit tested without a live stack.
# --------------------------------------------------------------------------


@dataclass
class ScenarioResult:
    """Outcome of a single eval scenario."""

    id: str
    tier: str
    status: str
    job_id: str | None = None
    duration_seconds: float | None = None
    preview_url: str | None = None
    attempts: int | None = None
    total_tokens: int | None = None
    estimated_cost_usd: float | None = None
    error: str | None = None
    failures: list[str] = field(default_factory=list)

    @property
    def delivered(self) -> bool:
        """Succeeded *and* published a usable preview URL."""
        return self.status == "succeeded" and bool(
            self.preview_url and self.preview_url.strip()
        )

    @property
    def passed(self) -> bool:
        return not self.failures


def score_scenario(scenario: dict[str, Any], result: ScenarioResult) -> ScenarioResult:
    """Applies a scenario's expectations, recording every violation.

    All expectations are checked rather than short-circuiting on the first
    failure — a run that both failed to deliver *and* blew its time budget is
    more informative than one that only reports the first problem.
    """
    expect = scenario.get("expect", {})
    failures: list[str] = []

    if expect.get("delivered") and not result.delivered:
        if result.status != "succeeded":
            failures.append(f"expected delivery, job status was {result.status!r}")
        else:
            failures.append("job succeeded but published no preview URL")

    max_duration = expect.get("max_duration_seconds")
    if (
        max_duration is not None
        and result.duration_seconds is not None
        and result.duration_seconds > max_duration
    ):
        failures.append(
            f"took {result.duration_seconds:.0f}s, budget was {max_duration}s"
        )

    max_cost = expect.get("max_cost_usd")
    if (
        max_cost is not None
        and result.estimated_cost_usd is not None
        and result.estimated_cost_usd > max_cost
    ):
        failures.append(
            f"cost ${result.estimated_cost_usd:.4f}, budget was ${max_cost}"
        )

    result.failures = failures
    return result


def summarise(results: list[ScenarioResult]) -> dict[str, Any]:
    """Aggregates scenario results into report-level metrics."""
    total = len(results)
    delivered = sum(1 for r in results if r.delivered)
    passed = sum(1 for r in results if r.passed)
    durations = [r.duration_seconds for r in results if r.duration_seconds is not None]
    costs = [r.estimated_cost_usd for r in results if r.estimated_cost_usd is not None]
    tokens = [r.total_tokens for r in results if r.total_tokens is not None]

    return {
        "scenarios": total,
        "delivered": delivered,
        "passed": passed,
        # Rates are None for an empty run rather than a misleading 0.0.
        "delivery_rate": (delivered / total) if total else None,
        "pass_rate": (passed / total) if total else None,
        "total_duration_seconds": sum(durations) if durations else None,
        "mean_duration_seconds": (sum(durations) / len(durations)) if durations else None,
        "total_tokens": sum(tokens) if tokens else None,
        "total_estimated_cost_usd": sum(costs) if costs else None,
    }


def select_scenarios(
    scenarios: Iterable[dict[str, Any]], tier: str | None, only: list[str] | None
) -> list[dict[str, Any]]:
    selected = list(scenarios)
    if tier:
        selected = [s for s in selected if s.get("tier") == tier]
    if only:
        wanted = set(only)
        selected = [s for s in selected if s.get("id") in wanted]
    return selected


# --------------------------------------------------------------------------
# HTTP plumbing
# --------------------------------------------------------------------------


class OtterClient:
    def __init__(self, base_url: str, timeout: int = 30) -> None:
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    def _request(
        self, method: str, path: str, body: dict[str, Any] | None = None
    ) -> Any:
        url = f"{self.base_url}{path}"
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(url, data=data, method=method)
        if data is not None:
            request.add_header("Content-Type", "application/json")
        with urllib.request.urlopen(request, timeout=self.timeout) as response:
            payload = response.read()
        return json.loads(payload) if payload else None

    def healthz(self) -> bool:
        try:
            urllib.request.urlopen(
                f"{self.base_url}/healthz", timeout=self.timeout
            ).read()
            return True
        except (urllib.error.URLError, OSError):
            return False

    def enqueue(self, prompt: str) -> dict[str, Any]:
        return self._request("POST", "/v1/prompts", {"prompt": prompt})

    def job(self, job_id: str) -> dict[str, Any]:
        return self._request("GET", f"/v1/jobs/{job_id}")

    def usage(self, job_id: str) -> dict[str, Any] | None:
        try:
            return self._request("GET", f"/v1/jobs/{job_id}/usage")
        except urllib.error.HTTPError as error:
            # 404 simply means no usage was recorded for this job.
            if error.code == 404:
                return None
            raise


def run_scenario(
    client: OtterClient, scenario: dict[str, Any], poll_seconds: float
) -> ScenarioResult:
    result = ScenarioResult(
        id=scenario["id"], tier=scenario.get("tier", "default"), status="not_started"
    )
    started = time.monotonic()

    try:
        job = client.enqueue(scenario["prompt"])
    except (urllib.error.URLError, OSError, ValueError) as error:
        result.status = "enqueue_failed"
        result.error = str(error)
        return score_scenario(scenario, result)

    result.job_id = job.get("id")
    timeout = scenario.get("timeout_seconds", 1800)

    while True:
        elapsed = time.monotonic() - started
        if elapsed > timeout:
            result.status = "timeout"
            result.duration_seconds = elapsed
            result.error = f"job did not reach a terminal state within {timeout}s"
            return score_scenario(scenario, result)

        try:
            current = client.job(result.job_id)
        except (urllib.error.URLError, OSError) as error:
            # Transient control-plane blips should not fail an eval run.
            result.error = f"poll error (retrying): {error}"
            time.sleep(poll_seconds)
            continue

        status = current.get("status", "unknown")
        if status in TERMINAL_STATUSES:
            result.status = status
            result.duration_seconds = time.monotonic() - started
            result.preview_url = current.get("preview_url")
            result.attempts = current.get("attempts")
            if status == "failed":
                result.error = current.get("error")
            break

        time.sleep(poll_seconds)

    usage = client.usage(result.job_id)
    if usage:
        result.total_tokens = usage.get("total_tokens")
        result.estimated_cost_usd = usage.get("estimated_cost_usd")

    return score_scenario(scenario, result)


# --------------------------------------------------------------------------
# Reporting
# --------------------------------------------------------------------------


def format_report(results: list[ScenarioResult], summary: dict[str, Any]) -> str:
    lines = ["", "Otter prompt-to-build evals", "=" * 72]
    header = f"{'scenario':<24}{'tier':<8}{'status':<12}{'dur':>7}{'cost':>10}  result"
    lines.append(header)
    lines.append("-" * 72)

    for result in results:
        duration = (
            f"{result.duration_seconds:.0f}s"
            if result.duration_seconds is not None
            else "-"
        )
        cost = (
            f"${result.estimated_cost_usd:.4f}"
            if result.estimated_cost_usd is not None
            else "-"
        )
        verdict = "PASS" if result.passed else "FAIL"
        lines.append(
            f"{result.id:<24}{result.tier:<8}{result.status:<12}"
            f"{duration:>7}{cost:>10}  {verdict}"
        )
        for failure in result.failures:
            lines.append(f"{'':<24}  ! {failure}")

    lines.append("-" * 72)
    delivery_rate = summary["delivery_rate"]
    pass_rate = summary["pass_rate"]
    lines.append(
        f"delivered {summary['delivered']}/{summary['scenarios']}"
        + (f" ({delivery_rate:.0%})" if delivery_rate is not None else "")
    )
    lines.append(
        f"passed    {summary['passed']}/{summary['scenarios']}"
        + (f" ({pass_rate:.0%})" if pass_rate is not None else "")
    )
    if summary["total_estimated_cost_usd"] is not None:
        lines.append(f"est. cost ${summary['total_estimated_cost_usd']:.4f}")
    if summary["mean_duration_seconds"] is not None:
        lines.append(f"mean dur  {summary['mean_duration_seconds']:.0f}s")
    lines.append("")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://localhost:8080")
    parser.add_argument("--scenarios", type=Path, default=DEFAULT_SCENARIOS)
    parser.add_argument("--tier", help="only run scenarios in this tier")
    parser.add_argument("--only", nargs="*", help="only run these scenario ids")
    parser.add_argument("--poll-seconds", type=float, default=5.0)
    parser.add_argument("--report", type=Path, help="write the JSON report here")
    parser.add_argument(
        "--min-delivery-rate",
        type=float,
        help="exit non-zero if the delivery rate falls below this (0.0-1.0)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="validate scenarios and connectivity without enqueueing anything",
    )
    args = parser.parse_args(argv)

    suite = json.loads(args.scenarios.read_text())
    scenarios = select_scenarios(suite["scenarios"], args.tier, args.only)
    if not scenarios:
        print("no scenarios selected", file=sys.stderr)
        return 2

    client = OtterClient(args.base_url)
    reachable = client.healthz()

    if args.dry_run:
        print(f"scenarios: {len(scenarios)}")
        for scenario in scenarios:
            print(f"  - [{scenario.get('tier', 'default')}] {scenario['id']}")
        print(f"otter at {args.base_url}: {'reachable' if reachable else 'UNREACHABLE'}")
        return 0 if reachable else 1

    if not reachable:
        print(f"otter is not reachable at {args.base_url}", file=sys.stderr)
        return 1

    results = [run_scenario(client, s, args.poll_seconds) for s in scenarios]
    summary = summarise(results)
    print(format_report(results, summary))

    if args.report:
        args.report.write_text(
            json.dumps(
                {"summary": summary, "results": [asdict(r) for r in results]}, indent=2
            )
        )
        print(f"report written to {args.report}")

    if args.min_delivery_rate is not None:
        rate = summary["delivery_rate"] or 0.0
        if rate < args.min_delivery_rate:
            print(
                f"delivery rate {rate:.0%} is below the "
                f"{args.min_delivery_rate:.0%} threshold",
                file=sys.stderr,
            )
            return 1

    return 0 if all(r.passed for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
