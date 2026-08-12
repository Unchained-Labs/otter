//! Token accounting and cost estimation for agent executions.
//!
//! The agent transcript we persist in `job_outputs.raw_json` is an array of the
//! JSON lines emitted by the agent in streaming mode. Providers report token
//! usage inside that stream, but not with a single agreed-upon shape, so this
//! module normalises the common variants into [`TokenUsage`].
//!
//! Cost is derived, never reported by the provider, so it is kept explicitly
//! separate: [`PricingTable`] is operator-configured and may legitimately be
//! empty, in which case token counts are still recorded and cost is `None`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Normalised token counts for one or more model responses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn is_empty(&self) -> bool {
        self.prompt_tokens == 0 && self.completion_tokens == 0 && self.total_tokens == 0
    }

    fn add(&mut self, other: TokenUsage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
    }
}

/// Per-million-token prices for a single model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_per_million_usd: f64,
    pub output_per_million_usd: f64,
}

/// Operator-configured price list, keyed by model name.
#[derive(Debug, Clone, Default)]
pub struct PricingTable {
    entries: HashMap<String, ModelPricing>,
}

impl PricingTable {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Parses `OTTER_MODEL_PRICING`.
    ///
    /// Format is a comma-separated list of `model=input:output`, where both
    /// prices are USD per one million tokens:
    ///
    /// ```text
    /// mistral-large-3=2.0:6.0,mistral-small-latest=0.2:0.6
    /// ```
    ///
    /// Malformed entries are skipped rather than failing startup: a bad price
    /// string should cost you cost-reporting, not your control plane.
    pub fn parse(raw: &str) -> Self {
        let mut entries = HashMap::new();
        for entry in raw.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let Some((model, prices)) = entry.split_once('=') else {
                continue;
            };
            let Some((input, output)) = prices.split_once(':') else {
                continue;
            };
            let (Ok(input), Ok(output)) =
                (input.trim().parse::<f64>(), output.trim().parse::<f64>())
            else {
                continue;
            };
            if input < 0.0 || output < 0.0 {
                continue;
            }
            entries.insert(
                model.trim().to_string(),
                ModelPricing {
                    input_per_million_usd: input,
                    output_per_million_usd: output,
                },
            );
        }
        Self { entries }
    }

    pub fn pricing_for(&self, model: &str) -> Option<ModelPricing> {
        self.entries.get(model).copied()
    }

    /// Returns estimated USD cost, or `None` when the model has no configured
    /// price. `None` means "unknown", never "free".
    pub fn estimate_cost_usd(&self, model: Option<&str>, usage: &TokenUsage) -> Option<f64> {
        let pricing = self.pricing_for(model?)?;
        let input = (usage.prompt_tokens as f64 / 1_000_000.0) * pricing.input_per_million_usd;
        let output =
            (usage.completion_tokens as f64 / 1_000_000.0) * pricing.output_per_million_usd;
        Some(input + output)
    }
}

/// Sums token usage across an agent transcript.
///
/// The transcript is the array persisted in `job_outputs.raw_json`. At most one
/// usage object is read per array entry — providers frequently expose the same
/// counts at several nesting levels within a single line, and counting each of
/// them would inflate the total. Across entries the counts *are* summed,
/// because an agentic run makes multiple model calls and each reports its own
/// usage.
pub fn extract_token_usage(raw_json: &Value) -> TokenUsage {
    let mut total = TokenUsage::default();
    let Some(entries) = raw_json.as_array() else {
        return total;
    };
    for entry in entries {
        if let Some(usage) = usage_from_entry(entry) {
            total.add(usage);
        }
    }
    total
}

/// Reads the model name the transcript reports, if any.
pub fn extract_model_name(raw_json: &Value) -> Option<String> {
    let entries = raw_json.as_array()?;
    entries.iter().find_map(|entry| {
        entry
            .get("model")
            .or_else(|| entry.pointer("/message/model"))
            .or_else(|| entry.pointer("/response/model"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn usage_from_entry(entry: &Value) -> Option<TokenUsage> {
    let usage = entry
        .get("usage")
        .or_else(|| entry.pointer("/message/usage"))
        .or_else(|| entry.pointer("/response/usage"))
        .or_else(|| entry.pointer("/delta/usage"))?;
    parse_usage_object(usage)
}

fn parse_usage_object(usage: &Value) -> Option<TokenUsage> {
    let prompt_tokens = read_u64(usage, &["prompt_tokens", "input_tokens"]);
    let completion_tokens = read_u64(usage, &["completion_tokens", "output_tokens"]);
    let total_tokens = read_u64(usage, &["total_tokens"]);

    if prompt_tokens.is_none() && completion_tokens.is_none() && total_tokens.is_none() {
        return None;
    }

    let prompt_tokens = prompt_tokens.unwrap_or(0);
    let completion_tokens = completion_tokens.unwrap_or(0);
    Some(TokenUsage {
        prompt_tokens,
        completion_tokens,
        // Providers that omit the total still let us derive it.
        total_tokens: total_tokens.unwrap_or(prompt_tokens + completion_tokens),
    })
}

fn read_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|found| {
            found
                .as_u64()
                // Some providers serialise counts as floats.
                .or_else(|| found.as_f64().filter(|n| *n >= 0.0).map(|n| n as u64))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_usage_from_flat_shape() {
        let transcript = serde_json::json!([
            {"role": "assistant", "content": "hi"},
            {"usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120}}
        ]);
        let usage = extract_token_usage(&transcript);
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 20);
        assert_eq!(usage.total_tokens, 120);
    }

    #[test]
    fn normalises_input_output_aliases() {
        let transcript = serde_json::json!([
            {"usage": {"input_tokens": 7, "output_tokens": 3}}
        ]);
        let usage = extract_token_usage(&transcript);
        assert_eq!(usage.prompt_tokens, 7);
        assert_eq!(usage.completion_tokens, 3);
        // Total is derived when the provider omits it.
        assert_eq!(usage.total_tokens, 10);
    }

    #[test]
    fn sums_usage_across_agent_turns() {
        let transcript = serde_json::json!([
            {"usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}},
            {"role": "assistant", "content": "working"},
            {"usage": {"prompt_tokens": 30, "completion_tokens": 7, "total_tokens": 37}}
        ]);
        let usage = extract_token_usage(&transcript);
        assert_eq!(usage.prompt_tokens, 40);
        assert_eq!(usage.completion_tokens, 12);
        assert_eq!(usage.total_tokens, 52);
    }

    #[test]
    fn counts_one_usage_object_per_entry() {
        // The same counts exposed twice in a single line must not be doubled.
        let transcript = serde_json::json!([
            {
                "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
                "message": {"usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}
            }
        ]);
        let usage = extract_token_usage(&transcript);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn reads_usage_nested_under_message() {
        let transcript = serde_json::json!([
            {"type": "message", "message": {"usage": {"prompt_tokens": 4, "completion_tokens": 6}}}
        ]);
        let usage = extract_token_usage(&transcript);
        assert_eq!(usage.prompt_tokens, 4);
        assert_eq!(usage.completion_tokens, 6);
    }

    #[test]
    fn returns_empty_usage_for_transcript_without_counts() {
        let transcript = serde_json::json!([{"role": "assistant", "content": "hello"}]);
        assert!(extract_token_usage(&transcript).is_empty());
    }

    #[test]
    fn returns_empty_usage_for_non_array_transcript() {
        assert!(extract_token_usage(&serde_json::json!({"usage": {"total_tokens": 5}})).is_empty());
    }

    #[test]
    fn extracts_model_name_from_transcript() {
        let transcript = serde_json::json!([
            {"role": "user", "content": "hi"},
            {"model": "mistral-large-3", "usage": {"total_tokens": 1}}
        ]);
        assert_eq!(
            extract_model_name(&transcript).as_deref(),
            Some("mistral-large-3")
        );
    }

    #[test]
    fn parses_pricing_table() {
        let table = PricingTable::parse("mistral-large-3=2.0:6.0, mistral-small-latest=0.2:0.6");
        assert_eq!(
            table.pricing_for("mistral-large-3"),
            Some(ModelPricing {
                input_per_million_usd: 2.0,
                output_per_million_usd: 6.0
            })
        );
        assert!(table.pricing_for("unknown-model").is_none());
    }

    #[test]
    fn skips_malformed_pricing_entries() {
        let table = PricingTable::parse("good=1:2,,broken,alsobroken=x:y,negative=-1:2");
        assert!(table.pricing_for("good").is_some());
        assert!(table.pricing_for("broken").is_none());
        assert!(table.pricing_for("alsobroken").is_none());
        assert!(table.pricing_for("negative").is_none());
    }

    #[test]
    fn estimates_cost_from_pricing() {
        let table = PricingTable::parse("m=2.0:6.0");
        let usage = TokenUsage {
            prompt_tokens: 1_000_000,
            completion_tokens: 500_000,
            total_tokens: 1_500_000,
        };
        let cost = table.estimate_cost_usd(Some("m"), &usage).unwrap();
        // 1M input at $2 + 0.5M output at $6 = $5.00
        assert!((cost - 5.0).abs() < 1e-9, "unexpected cost {cost}");
    }

    #[test]
    fn unknown_model_has_unknown_cost_not_zero() {
        let table = PricingTable::parse("m=2.0:6.0");
        let usage = TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 100,
            total_tokens: 200,
        };
        assert!(table.estimate_cost_usd(Some("other"), &usage).is_none());
        assert!(table.estimate_cost_usd(None, &usage).is_none());
    }
}
