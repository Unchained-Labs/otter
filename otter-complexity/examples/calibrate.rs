//! Print scores for a reference backlog, to sanity-check weight changes.
//!
//! ```bash
//! cargo run -p otter-complexity --example calibrate
//! ```
//!
//! The unit tests assert *relative* ordering, which catches inversions but not
//! drift — a change that pushes everything into one band still passes them. This
//! prints the whole spread so bands and estimates can be eyeballed after tuning
//! `lexicon.rs`. Prompts run from a one-word edit to a whole platform on purpose.

use otter_complexity::assess;

const REFERENCE_BACKLOG: &[&str] = &[
    "fix typo in readme",
    "bump the react version",
    "rename the submit button label",
    "add a tooltip to the save button",
    "make it better somehow",
    "add pagination to the users endpoint",
    "add CSV export to the reports page",
    "write unit tests for the alerts module",
    "port the billing service to rust",
    "build an inventory tracker with low stock alerts and a live dashboard",
    "add Stripe checkout and a webhook handler for subscription events",
    "add OAuth login with Google and GitHub",
    "migrate the billing tables to a multi-tenant schema with backward compatibility",
    "build a real-time collaborative editor with websockets and conflict resolution",
    "build a complete multi-tenant marketplace platform from scratch with payments, \
     search, admin panel, and real-time notifications",
];

fn main() {
    println!(
        "{:>3} {:>3} {:>4} {:<9} {:>6} {:>6}  PROMPT",
        "CX", "SZ", "INT", "BAND", "EST", "CONF"
    );
    println!("{}", "-".repeat(100));

    let mut scored: Vec<_> = REFERENCE_BACKLOG
        .iter()
        .map(|prompt| (assess(prompt), *prompt))
        .collect();
    scored.sort_by_key(|(assessment, _)| assessment.intensity);

    for (assessment, prompt) in scored {
        let truncated = if prompt.chars().count() > 58 {
            format!("{}…", prompt.chars().take(58).collect::<String>())
        } else {
            prompt.to_string()
        };
        println!(
            "{:>3} {:>3} {:>4} {:<9} {:>5}m {:>5.0}%  {}",
            assessment.complexity,
            assessment.size,
            assessment.intensity,
            assessment.band.as_str(),
            assessment.estimated_minutes,
            assessment.confidence * 100.0,
            truncated
        );
    }
}
