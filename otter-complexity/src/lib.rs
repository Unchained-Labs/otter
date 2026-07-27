//! Complexity and size scoring for natural-language build tasks.
//!
//! Given a prompt like *"build an inventory tracker with low stock alerts"*, this
//! crate answers three questions a scheduler needs:
//!
//! - **complexity** (1..=10) — how hard is this to reason about?
//! - **size** (1..=10) — how much work is there?
//! - **intensity** (0..=100) — the single number a queue can sort on.
//!
//! # Why heuristics rather than a model
//!
//! Scoring runs on the enqueue path, ahead of the work itself. A model call
//! there would add latency and cost to every submission, fail when the provider
//! is down, and return a different answer for the same prompt on different days
//! — so a job's queue position would wobble for reasons no user could see.
//!
//! These heuristics are instant, free, offline and **deterministic**: the same
//! prompt always scores the same. Every score arrives with the [`Signal`]s that
//! produced it, so a surprising position is explainable rather than mysterious.
//! Where a model genuinely helps is refinement after the fact, and
//! [`TaskAssessment::with_refinement`] exists for exactly that — an
//! agent can revise a score, and the origin is recorded.
//!
//! # Accuracy
//!
//! This is a ranking aid, not an oracle. It is tuned so that *relative* order is
//! usually right — a typo fix sorts ahead of a payments integration — which is
//! all the scheduler needs. Treat `estimated_minutes` as an order of magnitude.

use serde::{Deserialize, Serialize};

mod lexicon;

use lexicon::{
    AMBIGUITY_MARKERS, COMPLEXITY_TERMS, CONSTRUCTION_VERBS, MINOR_VERBS, SIZE_TERMS, SURFACES,
};

/// Suffixes a term may pick up and still count as the same word.
///
/// Kept short on purpose. Allowing arbitrary trailing characters would undo the
/// word-boundary check — "port" would match "portal" again.
const INFLECTIONS: &[&str] = &["", "s", "es", "ed", "d", "ing"];

/// Whether `haystack` contains `term` as a whole word.
///
/// Plain substring matching is wrong here and quietly corrupts scores: "port"
/// (as in porting a codebase, weight 5.5) is inside both "export" and "report",
/// so a CSV export scored like a platform migration. "form" hides in "platform",
/// "app" in "application", "test" in "latest". Matching requires a
/// non-alphanumeric boundary before the term and after its inflected ending.
fn contains_term(haystack: &str, term: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut from = 0;

    while let Some(offset) = haystack[from..].find(term) {
        let start = from + offset;
        let end = start + term.len();

        let boundary_before = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        if boundary_before {
            for inflection in INFLECTIONS {
                let stop = end + inflection.len();
                if stop > haystack.len() || !haystack[end..].starts_with(inflection) {
                    continue;
                }
                let boundary_after = stop == haystack.len() || !bytes[stop].is_ascii_alphanumeric();
                if boundary_after {
                    return true;
                }
            }
        }

        // Advance past this occurrence; a later one may sit on a boundary.
        from = start + term.len().max(1);
        if from >= haystack.len() {
            break;
        }
    }
    false
}

/// Coarse label for an assessment, for UI badges and reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplexityBand {
    Trivial,
    Small,
    Moderate,
    Large,
    Epic,
}

impl ComplexityBand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trivial => "trivial",
            Self::Small => "small",
            Self::Moderate => "moderate",
            Self::Large => "large",
            Self::Epic => "epic",
        }
    }

    fn from_intensity(intensity: u16) -> Self {
        match intensity {
            0..=14 => Self::Trivial,
            15..=34 => Self::Small,
            35..=59 => Self::Moderate,
            60..=79 => Self::Large,
            _ => Self::Epic,
        }
    }
}

/// What kind of evidence a [`Signal`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    /// A term arguing for conceptual difficulty.
    ComplexityTerm,
    /// A term arguing for volume of work.
    SizeTerm,
    /// Prompt length.
    Length,
    /// Distinct deliverables requested.
    Deliverables,
    /// Distinct areas of the system touched.
    Surface,
    /// Under-specified wording.
    Ambiguity,
    /// A verb that caps the task as minor.
    MinorVerb,
    /// Work this task waits on.
    Dependencies,
}

/// One piece of evidence behind a score, so an assessment can explain itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub kind: SignalKind,
    /// Human-readable evidence, e.g. `"kubernetes"` or `"touches 3 surfaces"`.
    pub detail: String,
    /// Contribution on the relevant 1..=10 scale.
    pub weight: f32,
}

/// Where a score came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssessmentSource {
    /// Produced by the deterministic heuristics in this crate.
    Heuristic,
    /// Heuristic score subsequently revised by an agent or a human.
    Refined,
}

/// Extra context that sharpens a score beyond the prompt text alone.
#[derive(Debug, Clone, Default)]
pub struct TaskContext {
    /// Jobs this task waits on. Blocked work is worth deprioritising slightly:
    /// it cannot start until its parents land.
    pub dependency_count: usize,
    /// Explicit caller override of the project subpath, which hints the task is
    /// scoped to one area rather than the whole workspace.
    pub scoped_to_project_path: bool,
}

/// A scored task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAssessment {
    /// Conceptual difficulty, 1..=10.
    pub complexity: u8,
    /// Volume of work, 1..=10.
    pub size: u8,
    /// Combined scheduling weight, 0..=100. Lower runs sooner.
    pub intensity: u16,
    pub band: ComplexityBand,
    /// Rough wall-clock estimate. An order of magnitude, not a promise.
    pub estimated_minutes: u32,
    /// How much to trust this assessment, 0.0..=1.0.
    pub confidence: f32,
    pub source: AssessmentSource,
    /// Evidence behind the score, strongest first.
    pub signals: Vec<Signal>,
}

impl TaskAssessment {
    /// Replace heuristic scores with values from an agent or a human.
    ///
    /// The signals that produced the original score are kept, so the record
    /// still shows what the heuristics saw before it was overridden.
    pub fn with_refinement(mut self, complexity: u8, size: u8, confidence: f32) -> Self {
        self.complexity = complexity.clamp(1, 10);
        self.size = size.clamp(1, 10);
        self.confidence = confidence.clamp(0.0, 1.0);
        self.intensity = combine_intensity(self.complexity, self.size);
        self.band = ComplexityBand::from_intensity(self.intensity);
        self.estimated_minutes = estimate_minutes(self.complexity, self.size);
        self.source = AssessmentSource::Refined;
        self
    }
}

/// Score a prompt with no extra context.
pub fn assess(prompt: &str) -> TaskAssessment {
    assess_with_context(prompt, &TaskContext::default())
}

/// Score a prompt, taking surrounding job context into account.
pub fn assess_with_context(prompt: &str, context: &TaskContext) -> TaskAssessment {
    let normalized = normalize(prompt);
    let mut signals = Vec::new();

    // An empty or near-empty prompt carries no evidence at all. Score it as
    // trivial with rock-bottom confidence rather than inventing a middle value.
    if normalized.trim().is_empty() {
        return TaskAssessment {
            complexity: 1,
            size: 1,
            intensity: combine_intensity(1, 1),
            band: ComplexityBand::Trivial,
            estimated_minutes: estimate_minutes(1, 1),
            confidence: 0.1,
            source: AssessmentSource::Heuristic,
            signals,
        };
    }

    let word_count = normalized.split_whitespace().count();

    // ── Complexity ───────────────────────────────────────────────────────────
    // Terms act as floors rather than addends: repeating "button" must not add
    // up to the difficulty of one "consensus".
    let mut complexity = 2.0_f32;
    for (term, weight) in COMPLEXITY_TERMS {
        if contains_term(&normalized, term) {
            if *weight > complexity {
                complexity = *weight;
            }
            signals.push(Signal {
                kind: SignalKind::ComplexityTerm,
                detail: (*term).to_string(),
                weight: *weight,
            });
        }
    }

    // ── Size ─────────────────────────────────────────────────────────────────
    let mut size = 2.0_f32;
    for (term, weight) in SIZE_TERMS {
        if contains_term(&normalized, term) {
            if *weight > size {
                size = *weight;
            }
            signals.push(Signal {
                kind: SignalKind::SizeTerm,
                detail: (*term).to_string(),
                weight: *weight,
            });
        }
    }

    // Building something new is materially more work than adjusting something
    // that already exists, and the lexicon alone misses it: "build an inventory
    // tracker" contains no size term at all.
    if let Some(verb) = CONSTRUCTION_VERBS
        .iter()
        .find(|verb| contains_term(&normalized, verb))
    {
        if size < 4.5 {
            size = 4.5;
        }
        signals.push(Signal {
            kind: SignalKind::SizeTerm,
            detail: format!("builds something new (\"{verb}\")"),
            weight: 4.5,
        });
    }

    // Length is weak evidence of size on its own, so it nudges rather than sets.
    let length_push = match word_count {
        0..=6 => -0.5,
        7..=15 => 0.0,
        16..=35 => 0.6,
        36..=80 => 1.2,
        81..=160 => 1.8,
        _ => 2.4,
    };
    if length_push != 0.0 {
        size += length_push;
        signals.push(Signal {
            kind: SignalKind::Length,
            detail: format!("{word_count} words"),
            weight: length_push,
        });
    }

    // Separate deliverables: "X, Y and Z" is three things to build.
    let deliverables = count_deliverables(&normalized);
    if deliverables > 1 {
        let push = ((deliverables - 1) as f32 * 0.7).min(3.0);
        size += push;
        complexity += push * 0.25;
        signals.push(Signal {
            kind: SignalKind::Deliverables,
            detail: format!("{deliverables} deliverables"),
            weight: push,
        });
    }

    // Breadth across system surfaces is the strongest size signal there is.
    let surfaces = matched_surfaces(&normalized);
    if surfaces.len() > 1 {
        let push = ((surfaces.len() - 1) as f32 * 0.9).min(3.5);
        size += push;
        complexity += push * 0.35;
        signals.push(Signal {
            kind: SignalKind::Surface,
            detail: format!("touches {}: {}", surfaces.len(), surfaces.join(", ")),
            weight: push,
        });
    }

    // ── Confidence ───────────────────────────────────────────────────────────
    let mut confidence = 0.72_f32;

    // Very short prompts and very long ones are both harder to read accurately.
    if word_count < 5 {
        confidence -= 0.22;
    } else if word_count > 120 {
        confidence -= 0.08;
    }
    // Concrete vocabulary means the lexicon actually recognised the domain.
    let recognised = signals
        .iter()
        .filter(|signal| {
            matches!(
                signal.kind,
                SignalKind::ComplexityTerm | SignalKind::SizeTerm
            )
        })
        .count();
    confidence += (recognised as f32 * 0.04).min(0.18);

    let ambiguities = AMBIGUITY_MARKERS
        .iter()
        .filter(|marker| contains_term(&normalized, marker))
        .count();
    if ambiguities > 0 {
        // Vague scope hides work, so nudge complexity up while trusting it less.
        complexity += (ambiguities as f32 * 0.5).min(1.5);
        confidence -= (ambiguities as f32 * 0.14).min(0.4);
        signals.push(Signal {
            kind: SignalKind::Ambiguity,
            detail: format!("{ambiguities} vague phrase(s)"),
            weight: -(ambiguities as f32 * 0.14),
        });
    }

    // ── Context ──────────────────────────────────────────────────────────────
    if context.dependency_count > 0 {
        let push = (context.dependency_count as f32 * 0.4).min(1.5);
        complexity += push;
        signals.push(Signal {
            kind: SignalKind::Dependencies,
            detail: format!("{} dependenc(ies)", context.dependency_count),
            weight: push,
        });
    }
    if context.scoped_to_project_path {
        // An explicit subpath bounds the blast radius.
        size -= 0.5;
    }

    // ── Minor-verb cap ───────────────────────────────────────────────────────
    // Applied last so it overrides everything above: the nouns in "fix the typo
    // in the kubernetes runbook" describe where the typo is, not the work.
    if let Some(verb) = MINOR_VERBS
        .iter()
        .find(|verb| contains_term(&normalized, verb))
    {
        // Only cap genuinely short requests; a long prompt that happens to
        // contain "rename" is not a rename task.
        if word_count <= 25 {
            complexity = complexity.min(2.5);
            size = size.min(2.5);
            confidence += 0.1;
            signals.push(Signal {
                kind: SignalKind::MinorVerb,
                detail: (*verb).to_string(),
                weight: -3.0,
            });
        }
    }

    let complexity = clamp_score(complexity);
    let size = clamp_score(size);
    let intensity = combine_intensity(complexity, size);

    // Strongest evidence first, so a truncated UI list still shows what mattered.
    signals.sort_by(|a, b| {
        b.weight
            .abs()
            .partial_cmp(&a.weight.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    signals.truncate(8);

    TaskAssessment {
        complexity,
        size,
        intensity,
        band: ComplexityBand::from_intensity(intensity),
        estimated_minutes: estimate_minutes(complexity, size),
        confidence: confidence.clamp(0.05, 0.95),
        source: AssessmentSource::Heuristic,
        signals,
    }
}

fn normalize(prompt: &str) -> String {
    prompt.to_lowercase()
}

fn clamp_score(value: f32) -> u8 {
    value.round().clamp(1.0, 10.0) as u8
}

/// Blend the two axes into the single number the queue sorts on.
///
/// Size is weighted slightly above complexity because the scheduler's goal is
/// throughput: finishing short work first shortens average wait, whereas a task
/// being conceptually hard does not by itself make it long.
fn combine_intensity(complexity: u8, size: u8) -> u16 {
    let complexity = f32::from(complexity);
    let size = f32::from(size);
    let blended = 0.45 * complexity + 0.55 * size;
    // Map the 1..=10 blend onto 0..=100.
    (((blended - 1.0) / 9.0) * 100.0).round().clamp(0.0, 100.0) as u16
}

/// Rough wall-clock estimate. Grows super-linearly: hard *and* large work costs
/// more than the sum of its parts.
fn estimate_minutes(complexity: u8, size: u8) -> u32 {
    let complexity = f32::from(complexity);
    let size = f32::from(size);
    let base = 2.0_f32;
    let minutes = base * size.powf(1.35) * (1.0 + (complexity - 1.0) * 0.22);
    minutes.round().clamp(1.0, 100_000.0) as u32
}

/// Count separate things being asked for.
///
/// Deliberately conservative: it counts explicit list structure (commas joining
/// clauses, "and", "plus", bullets, numbered items) rather than trying to parse
/// grammar, because over-counting inflates every multi-clause sentence.
fn count_deliverables(normalized: &str) -> usize {
    let mut count = 1;

    for separator in [" and ", " plus ", " as well as ", " along with ", " then "] {
        count += normalized.matches(separator).count();
    }
    // Bullets and numbered lists.
    count += normalized.matches('\n').filter(|_| true).count().min(12);
    count += normalized.matches(" with ").count().min(3);

    count.min(14)
}

/// Which areas of the system the prompt appears to touch.
fn matched_surfaces(normalized: &str) -> Vec<&'static str> {
    SURFACES
        .iter()
        .filter(|(_, keywords)| {
            keywords
                .iter()
                .any(|keyword| contains_term(normalized, keyword))
        })
        .map(|(name, _)| *name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intensity_of(prompt: &str) -> u16 {
        assess(prompt).intensity
    }

    #[test]
    fn terms_match_only_on_word_boundaries() {
        // "port" (porting a codebase) must not fire inside these words.
        assert!(!contains_term("add csv export to the reports page", "port"));
        assert!(!contains_term(
            "the platform stores the information",
            "form"
        ));
        assert!(!contains_term("run the latest contest", "test"));
        assert!(!contains_term("a rapid capital gain", "api"));
        assert!(!contains_term("the application appeared", "app"));

        // Real occurrences, including common inflections, still match.
        assert!(contains_term("port the service to rust", "port"));
        assert!(contains_term("we ported it already", "port"));
        assert!(contains_term("add two endpoints", "endpoint"));
        assert!(contains_term("write migrations", "migration"));
        assert!(contains_term("kubernetes", "kubernetes"));
        assert!(contains_term(
            "needs high availability now",
            "high availability"
        ));
    }

    #[test]
    fn csv_export_is_not_mistaken_for_a_platform_port() {
        // Regression: "export"/"reports" both contain "port" (weight 5.5), which
        // scored a routine export like a codebase migration.
        let export = assess("add CSV export to the reports page");
        let porting = assess("port the billing service to rust");
        assert!(
            export.complexity < porting.complexity,
            "export={} porting={}",
            export.complexity,
            porting.complexity
        );
        assert!(export.complexity <= 4, "got {}", export.complexity);
    }

    #[test]
    fn building_something_new_outweighs_adjusting_something_existing() {
        let adjust = assess("update the inventory tracker page heading");
        let build = assess("build an inventory tracker with low stock alerts");
        assert!(
            build.size > adjust.size,
            "build={} adjust={}",
            build.size,
            adjust.size
        );
    }

    #[test]
    fn scoring_is_deterministic() {
        let prompt = "Build a dashboard with charts and CSV export";
        let first = assess(prompt);
        let second = assess(prompt);
        assert_eq!(first.intensity, second.intensity);
        assert_eq!(first.complexity, second.complexity);
        assert_eq!(first.size, second.size);
    }

    #[test]
    fn trivial_edits_score_below_substantial_features() {
        assert!(
            intensity_of("fix typo in the readme")
                < intensity_of("add Stripe checkout with webhook handling")
        );
    }

    #[test]
    fn substantial_features_score_below_whole_platforms() {
        assert!(
            intensity_of("add Stripe checkout with webhook handling")
                < intensity_of(
                    "build a complete multi-tenant marketplace platform from scratch with \
                     payments, search, admin panel, and real-time notifications"
                )
        );
    }

    #[test]
    fn ordering_is_stable_across_a_realistic_backlog() {
        let mut backlog = [
            "rename the submit button label",
            "add pagination to the users endpoint",
            "add OAuth login with Google and GitHub",
            "build a distributed job scheduler with sharding and failover",
        ];
        backlog.sort_by_key(|prompt| intensity_of(prompt));

        assert_eq!(backlog[0], "rename the submit button label");
        assert_eq!(
            backlog[3],
            "build a distributed job scheduler with sharding and failover"
        );
    }

    #[test]
    fn minor_verb_caps_intimidating_nouns() {
        // The kubernetes mention describes where the typo is, not the work.
        let capped = assess("fix typo in the kubernetes runbook");
        assert!(
            capped.complexity <= 3,
            "expected a capped score, got {}",
            capped.complexity
        );
        assert!(capped
            .signals
            .iter()
            .any(|s| s.kind == SignalKind::MinorVerb));
    }

    #[test]
    fn minor_verb_does_not_cap_a_long_detailed_request() {
        let prompt = "rename the legacy billing tables, then migrate every subscription record \
                      to the new multi-tenant schema, backfill historical invoices, update the \
                      reporting pipeline, and keep backward compatibility for the public api";
        let assessment = assess(prompt);
        assert!(
            assessment.complexity >= 5,
            "long migration should not be capped, got {}",
            assessment.complexity
        );
    }

    #[test]
    fn breadth_across_surfaces_raises_size() {
        let narrow = assess("add a button to the react component");
        let broad = assess(
            "add a react page, a backend endpoint, a postgres migration, and docker deployment",
        );
        assert!(
            broad.size > narrow.size,
            "broad={} narrow={}",
            broad.size,
            narrow.size
        );
    }

    #[test]
    fn vague_prompts_lose_confidence() {
        let precise = assess("add pagination to the users endpoint with a limit of 50");
        let vague = assess("make it better somehow, maybe add some kind of dashboard or something");
        assert!(
            vague.confidence < precise.confidence,
            "vague={} precise={}",
            vague.confidence,
            precise.confidence
        );
    }

    #[test]
    fn dependencies_raise_complexity() {
        let context = TaskContext {
            dependency_count: 3,
            ..Default::default()
        };
        let solo = assess("add a settings page");
        let blocked = assess_with_context("add a settings page", &context);
        assert!(blocked.complexity >= solo.complexity);
    }

    #[test]
    fn empty_prompt_is_trivial_and_low_confidence() {
        let assessment = assess("   ");
        assert_eq!(assessment.band, ComplexityBand::Trivial);
        assert!(assessment.confidence < 0.2);
    }

    #[test]
    fn scores_stay_within_documented_ranges() {
        let prompts = [
            "",
            "hi",
            "fix typo",
            "add a button",
            "build a compiler with a bytecode interpreter and a distributed build cache \
             across multiple regions with consensus and failover and machine learning",
        ];
        for prompt in prompts {
            let assessment = assess(prompt);
            assert!((1..=10).contains(&assessment.complexity));
            assert!((1..=10).contains(&assessment.size));
            assert!(assessment.intensity <= 100);
            assert!((0.0..=1.0).contains(&assessment.confidence));
            assert!(assessment.estimated_minutes >= 1);
        }
    }

    #[test]
    fn bands_follow_intensity() {
        assert_eq!(ComplexityBand::from_intensity(0), ComplexityBand::Trivial);
        assert_eq!(ComplexityBand::from_intensity(25), ComplexityBand::Small);
        assert_eq!(ComplexityBand::from_intensity(50), ComplexityBand::Moderate);
        assert_eq!(ComplexityBand::from_intensity(70), ComplexityBand::Large);
        assert_eq!(ComplexityBand::from_intensity(95), ComplexityBand::Epic);
    }

    #[test]
    fn refinement_recomputes_derived_fields_and_marks_source() {
        let refined = assess("add a button").with_refinement(9, 8, 0.9);
        assert_eq!(refined.source, AssessmentSource::Refined);
        assert_eq!(refined.complexity, 9);
        assert_eq!(refined.size, 8);
        assert_eq!(refined.intensity, combine_intensity(9, 8));
        assert_eq!(
            refined.band,
            ComplexityBand::from_intensity(refined.intensity)
        );
    }

    #[test]
    fn estimates_grow_with_both_axes() {
        assert!(estimate_minutes(2, 2) < estimate_minutes(2, 6));
        assert!(estimate_minutes(2, 6) < estimate_minutes(8, 6));
    }

    #[test]
    fn signals_explain_the_score() {
        let assessment = assess("add oauth login and a postgres migration");
        assert!(!assessment.signals.is_empty());
        assert!(assessment
            .signals
            .iter()
            .any(|signal| signal.kind == SignalKind::ComplexityTerm));
    }
}
