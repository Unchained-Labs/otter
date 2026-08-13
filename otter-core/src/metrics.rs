//! Fleet-level metrics for the orchestration control plane.
//!
//! Metrics are derived from Postgres rather than from in-process counters. The
//! server and the worker are separate processes: in-process counters in the
//! server would silently miss everything the worker does, and would reset on
//! every deploy. Querying the database means `/metrics` reports the same
//! numbers no matter which replica answers.

use serde::{Deserialize, Serialize};

/// Aggregate view of job outcomes, spend, and throughput.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricsSummary {
    pub jobs_total: i64,
    pub jobs_queued: i64,
    pub jobs_running: i64,
    pub jobs_succeeded: i64,
    pub jobs_failed: i64,
    pub jobs_cancelled: i64,
    /// Jobs that both succeeded *and* published a reachable preview URL. This
    /// is the signal that matters: a zero exit code is not a running app.
    pub jobs_delivered: i64,
    pub prompt_tokens_total: i64,
    pub completion_tokens_total: i64,
    pub tokens_total: i64,
    /// Sum over jobs with a configured model price. Jobs on unpriced models
    /// contribute tokens but not cost.
    pub estimated_cost_usd_total: f64,
    pub jobs_with_cost: i64,
    pub avg_duration_ms_succeeded: Option<f64>,
}

impl MetricsSummary {
    /// Share of terminal jobs that succeeded. `None` when nothing has finished.
    pub fn success_rate(&self) -> Option<f64> {
        let terminal = self.jobs_succeeded + self.jobs_failed + self.jobs_cancelled;
        if terminal == 0 {
            return None;
        }
        Some(self.jobs_succeeded as f64 / terminal as f64)
    }

    /// Share of terminal jobs that produced a reachable app. This is always
    /// less than or equal to [`Self::success_rate`], and the gap between them
    /// is the interesting number: work that "succeeded" without shipping.
    pub fn delivery_rate(&self) -> Option<f64> {
        let terminal = self.jobs_succeeded + self.jobs_failed + self.jobs_cancelled;
        if terminal == 0 {
            return None;
        }
        Some(self.jobs_delivered as f64 / terminal as f64)
    }

    /// Renders the summary in Prometheus text exposition format (v0.0.4).
    pub fn render_prometheus(&self) -> String {
        let mut out = String::new();

        gauge(
            &mut out,
            "otter_jobs",
            "Jobs by lifecycle status.",
            &[
                ("queued", self.jobs_queued as f64),
                ("running", self.jobs_running as f64),
                ("succeeded", self.jobs_succeeded as f64),
                ("failed", self.jobs_failed as f64),
                ("cancelled", self.jobs_cancelled as f64),
            ],
            "status",
        );

        simple_gauge(
            &mut out,
            "otter_jobs_total",
            "Total jobs recorded.",
            self.jobs_total as f64,
        );
        simple_gauge(
            &mut out,
            "otter_jobs_delivered_total",
            "Jobs that succeeded and published a preview URL.",
            self.jobs_delivered as f64,
        );

        gauge(
            &mut out,
            "otter_tokens_total",
            "Model tokens consumed by agent executions.",
            &[
                ("prompt", self.prompt_tokens_total as f64),
                ("completion", self.completion_tokens_total as f64),
            ],
            "kind",
        );

        simple_gauge(
            &mut out,
            "otter_estimated_cost_usd_total",
            "Estimated spend in USD across jobs with a configured model price.",
            self.estimated_cost_usd_total,
        );
        simple_gauge(
            &mut out,
            "otter_jobs_with_cost_total",
            "Jobs contributing to the estimated cost total.",
            self.jobs_with_cost as f64,
        );

        if let Some(rate) = self.success_rate() {
            simple_gauge(
                &mut out,
                "otter_success_rate",
                "Succeeded share of terminal jobs.",
                rate,
            );
        }
        if let Some(rate) = self.delivery_rate() {
            simple_gauge(
                &mut out,
                "otter_delivery_rate",
                "Share of terminal jobs that published a reachable preview URL.",
                rate,
            );
        }
        if let Some(duration) = self.avg_duration_ms_succeeded {
            simple_gauge(
                &mut out,
                "otter_job_duration_ms_avg",
                "Mean wall-clock duration of succeeded jobs, in milliseconds.",
                duration,
            );
        }

        out
    }
}

fn simple_gauge(out: &mut String, name: &str, help: &str, value: f64) {
    out.push_str(&format!("# HELP {name} {help}\n"));
    out.push_str(&format!("# TYPE {name} gauge\n"));
    out.push_str(&format!("{name} {}\n", format_value(value)));
}

fn gauge(out: &mut String, name: &str, help: &str, series: &[(&str, f64)], label: &str) {
    out.push_str(&format!("# HELP {name} {help}\n"));
    out.push_str(&format!("# TYPE {name} gauge\n"));
    for (label_value, value) in series {
        out.push_str(&format!(
            "{name}{{{label}=\"{label_value}\"}} {}\n",
            format_value(*value)
        ));
    }
}

/// Prometheus rejects `NaN`/`inf` in most tooling paths and expects plain
/// decimal notation, so keep the formatting explicit rather than relying on
/// `f64`'s default `Display`.
fn format_value(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_string();
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    format!("{value:.6}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MetricsSummary {
        MetricsSummary {
            jobs_total: 10,
            jobs_queued: 2,
            jobs_running: 1,
            jobs_succeeded: 5,
            jobs_failed: 2,
            jobs_cancelled: 0,
            jobs_delivered: 4,
            prompt_tokens_total: 1000,
            completion_tokens_total: 250,
            tokens_total: 1250,
            estimated_cost_usd_total: 0.5,
            jobs_with_cost: 5,
            avg_duration_ms_succeeded: Some(42_000.0),
        }
    }

    #[test]
    fn success_rate_uses_terminal_jobs_only() {
        // 5 succeeded of 7 terminal — queued and running must not dilute it.
        let rate = sample().success_rate().unwrap();
        assert!((rate - 5.0 / 7.0).abs() < 1e-9, "unexpected rate {rate}");
    }

    #[test]
    fn delivery_rate_is_bounded_by_success_rate() {
        let summary = sample();
        assert!(summary.delivery_rate().unwrap() <= summary.success_rate().unwrap());
    }

    #[test]
    fn rates_are_undefined_before_anything_finishes() {
        let summary = MetricsSummary {
            jobs_queued: 3,
            jobs_total: 3,
            ..Default::default()
        };
        assert!(summary.success_rate().is_none());
        assert!(summary.delivery_rate().is_none());
    }

    #[test]
    fn renders_prometheus_exposition() {
        let rendered = sample().render_prometheus();
        assert!(rendered.contains("# TYPE otter_jobs gauge"));
        assert!(rendered.contains("otter_jobs{status=\"succeeded\"} 5"));
        assert!(rendered.contains("otter_tokens_total{kind=\"prompt\"} 1000"));
        assert!(rendered.contains("otter_jobs_delivered_total 4"));
        assert!(rendered.contains("otter_job_duration_ms_avg 42000"));
    }

    #[test]
    fn omits_undefined_rate_series() {
        let rendered = MetricsSummary::default().render_prometheus();
        assert!(!rendered.contains("otter_success_rate"));
        assert!(!rendered.contains("otter_job_duration_ms_avg"));
        // Counters that are meaningfully zero are still reported.
        assert!(rendered.contains("otter_jobs_total 0"));
    }

    #[test]
    fn formats_fractional_and_integral_values_distinctly() {
        assert_eq!(format_value(5.0), "5");
        assert_eq!(format_value(0.5), "0.500000");
        // Non-finite values must not emit `NaN` into the exposition.
        assert_eq!(format_value(f64::NAN), "0");
        assert_eq!(format_value(f64::INFINITY), "0");
    }
}
