//! CommonMark 0.31.2 conformance runner — the live progress bar.
//!
//! Reads the official 652-example suite (`fixtures/spec.json`, vendored from
//! <https://spec.commonmark.org/0.31.2/spec.json>) and reports how many
//! examples sparkdown reproduces byte-for-byte. Run it verbose to see the
//! number and the first few failing sections:
//!
//! ```text
//! cargo test --test spec -- --nocapture
//! ```
//!
//! The assertion is a **ratchet**: it only requires that conformance not
//! regress below a floor, so CI stays green while the parser grows. Raise
//! `FLOOR` as you implement block/inline features; flip it to
//! `assert_eq!(pass, total)` when you reach 100%.

use serde_json::Value;

/// Minimum passing examples the suite must not drop below. Bump this up as
/// conformance improves so regressions are caught.
/// History: 86 (paragraphs only) → 172 (leaf blocks: headings, thematic
/// breaks, fenced + indented code).
const FLOOR: usize = 652;

#[test]
fn commonmark_conformance() {
    let raw = include_str!("fixtures/spec.json");
    let cases: Value = serde_json::from_str(raw).expect("spec.json parses");
    let arr = cases.as_array().expect("spec.json is a JSON array");
    let total = arr.len();

    let mut pass = 0usize;
    let mut sample_failures = Vec::new();

    for case in arr {
        let md = case["markdown"].as_str().expect("case has markdown");
        let expected = case["html"].as_str().expect("case has html");
        let got = sparkdown::to_html(md);
        if got == expected {
            pass += 1;
        } else if sample_failures.len() < 8 {
            let section = case["section"].as_str().unwrap_or("?");
            let example = case["example"].as_u64().unwrap_or(0);
            sample_failures.push(format!("ex{example} [{section}]"));
        }
    }

    let pct = pass as f64 / total as f64 * 100.0;
    println!("\nCommonMark 0.31.2 conformance: {pass}/{total} ({pct:.1}%)");
    if !sample_failures.is_empty() {
        println!("first failing examples: {}", sample_failures.join(", "));
    }

    assert!(
        pass >= FLOOR,
        "conformance regressed: {pass}/{total} passing, floor is {FLOOR}"
    );
}
