#!/usr/bin/env python3
"""Unit tests for the eval harness scoring logic.

These cover the pure functions only, so they run in CI without a live stack.
Standard library `unittest` — no pytest dependency in the Rust repo.

    python3 -m unittest discover -s evals -p 'test_*.py'
"""

from __future__ import annotations

import json
import unittest
from pathlib import Path

from run_evals import (
    ScenarioResult,
    score_scenario,
    select_scenarios,
    summarise,
)


def result(**kwargs) -> ScenarioResult:
    defaults = {"id": "s", "tier": "smoke", "status": "succeeded"}
    defaults.update(kwargs)
    return ScenarioResult(**defaults)


class DeliveredTests(unittest.TestCase):
    def test_succeeded_with_preview_url_is_delivered(self):
        self.assertTrue(result(preview_url="http://localhost:3000").delivered)

    def test_succeeded_without_preview_url_is_not_delivered(self):
        self.assertFalse(result(preview_url=None).delivered)

    def test_blank_preview_url_is_not_delivered(self):
        # A whitespace-only URL is not a running app.
        self.assertFalse(result(preview_url="   ").delivered)

    def test_failed_job_is_never_delivered(self):
        self.assertFalse(
            result(status="failed", preview_url="http://localhost:3000").delivered
        )


class ScoreScenarioTests(unittest.TestCase):
    def test_delivered_scenario_passes(self):
        scenario = {"id": "s", "expect": {"delivered": True}}
        scored = score_scenario(scenario, result(preview_url="http://x"))
        self.assertTrue(scored.passed)
        self.assertEqual(scored.failures, [])

    def test_missing_preview_url_is_reported_distinctly(self):
        scenario = {"id": "s", "expect": {"delivered": True}}
        scored = score_scenario(scenario, result(preview_url=None))
        self.assertFalse(scored.passed)
        self.assertIn("no preview URL", scored.failures[0])

    def test_failed_status_reports_the_status(self):
        scenario = {"id": "s", "expect": {"delivered": True}}
        scored = score_scenario(scenario, result(status="failed"))
        self.assertIn("'failed'", scored.failures[0])

    def test_duration_budget_is_enforced(self):
        scenario = {"id": "s", "expect": {"max_duration_seconds": 60}}
        scored = score_scenario(scenario, result(preview_url="http://x", duration_seconds=90))
        self.assertFalse(scored.passed)
        self.assertIn("budget was 60s", scored.failures[0])

    def test_duration_within_budget_passes(self):
        scenario = {"id": "s", "expect": {"max_duration_seconds": 60}}
        scored = score_scenario(scenario, result(preview_url="http://x", duration_seconds=30))
        self.assertTrue(scored.passed)

    def test_cost_budget_is_enforced(self):
        scenario = {"id": "s", "expect": {"max_cost_usd": 0.10}}
        scored = score_scenario(scenario, result(preview_url="http://x", estimated_cost_usd=0.25))
        self.assertFalse(scored.passed)
        self.assertIn("budget was $0.1", scored.failures[0])

    def test_all_violations_are_collected_not_just_the_first(self):
        scenario = {
            "id": "s",
            "expect": {"delivered": True, "max_duration_seconds": 10, "max_cost_usd": 0.01},
        }
        scored = score_scenario(
            scenario,
            result(status="failed", duration_seconds=100, estimated_cost_usd=1.0),
        )
        self.assertEqual(len(scored.failures), 3)

    def test_unknown_cost_does_not_trip_the_cost_budget(self):
        # No pricing configured means unknown cost, which must not read as a breach.
        scenario = {"id": "s", "expect": {"max_cost_usd": 0.01}}
        scored = score_scenario(scenario, result(preview_url="http://x", estimated_cost_usd=None))
        self.assertTrue(scored.passed)


class SummariseTests(unittest.TestCase):
    def test_summarises_mixed_run(self):
        results = [
            score_scenario(
                {"expect": {"delivered": True}},
                result(id="a", preview_url="http://x", duration_seconds=10, total_tokens=100,
                       estimated_cost_usd=0.1),
            ),
            score_scenario(
                {"expect": {"delivered": True}},
                result(id="b", status="failed", duration_seconds=20, total_tokens=50,
                       estimated_cost_usd=0.05),
            ),
        ]
        summary = summarise(results)
        self.assertEqual(summary["scenarios"], 2)
        self.assertEqual(summary["delivered"], 1)
        self.assertEqual(summary["passed"], 1)
        self.assertAlmostEqual(summary["delivery_rate"], 0.5)
        self.assertAlmostEqual(summary["total_estimated_cost_usd"], 0.15)
        self.assertEqual(summary["total_tokens"], 150)
        self.assertAlmostEqual(summary["mean_duration_seconds"], 15.0)

    def test_empty_run_reports_undefined_rates_not_zero(self):
        summary = summarise([])
        self.assertEqual(summary["scenarios"], 0)
        self.assertIsNone(summary["delivery_rate"])
        self.assertIsNone(summary["pass_rate"])


class SelectScenariosTests(unittest.TestCase):
    SCENARIOS = [
        {"id": "a", "tier": "smoke"},
        {"id": "b", "tier": "core"},
        {"id": "c", "tier": "smoke"},
    ]

    def test_no_filter_returns_everything(self):
        self.assertEqual(len(select_scenarios(self.SCENARIOS, None, None)), 3)

    def test_filters_by_tier(self):
        selected = select_scenarios(self.SCENARIOS, "smoke", None)
        self.assertEqual([s["id"] for s in selected], ["a", "c"])

    def test_filters_by_id(self):
        selected = select_scenarios(self.SCENARIOS, None, ["b"])
        self.assertEqual([s["id"] for s in selected], ["b"])

    def test_tier_and_id_filters_intersect(self):
        self.assertEqual(select_scenarios(self.SCENARIOS, "core", ["a"]), [])


class ScenarioSuiteTests(unittest.TestCase):
    """Guards the shipped suite against malformed edits."""

    def setUp(self):
        path = Path(__file__).parent / "scenarios.json"
        self.suite = json.loads(path.read_text())

    def test_suite_parses_and_is_not_empty(self):
        self.assertTrue(self.suite["scenarios"])

    def test_every_scenario_has_required_fields(self):
        for scenario in self.suite["scenarios"]:
            self.assertIn("id", scenario)
            self.assertIn("prompt", scenario)
            self.assertIn("tier", scenario)
            self.assertIn("expect", scenario)
            self.assertGreater(scenario.get("timeout_seconds", 0), 0)

    def test_scenario_ids_are_unique(self):
        ids = [s["id"] for s in self.suite["scenarios"]]
        self.assertEqual(len(ids), len(set(ids)))

    def test_duration_budget_fits_inside_the_timeout(self):
        # A budget above the timeout could never fail for the right reason.
        for scenario in self.suite["scenarios"]:
            budget = scenario["expect"].get("max_duration_seconds")
            if budget is not None:
                self.assertLessEqual(
                    budget, scenario["timeout_seconds"], f"{scenario['id']} budget exceeds timeout"
                )


if __name__ == "__main__":
    unittest.main()
