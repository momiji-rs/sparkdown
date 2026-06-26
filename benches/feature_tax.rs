//! Feature-tax benchmark: compare the parser's code paths under each Cargo
//! feature, on one corpus, so the numbers are directly comparable.
//!
//! Paths measured (each present only when its feature is compiled in):
//!   CommonMark   to_html(src)             — the default fast path        (always)
//!   GFM          to_html_with(src, gfm)   — every GFM extension active   (`gfm`)
//!   AST build    to_mdast(src)            — owned nested mdast in memory  (`ast`)
//!   AST → JSON   to_mdast_json(src)       — mdast serialized (wasm→JS)    (`ast`)
//!
//! Two questions, one bench:
//!  1. Path cost *within a build*: read the table's `vs base` column.
//!  2. Feature *compile-tax on the hot path*: compare the `CommonMark (to_html)`
//!     row across builds — enabling `gfm`/`ast` must not slow the base path
//!     (the default build is meant to be the byte-identical CommonMark fast path).
//!
//! Run per feature set (manual timing, no criterion harness):
//!   cargo bench --bench feature_tax
//!   cargo bench --features gfm        --bench feature_tax
//!   cargo bench --features ast        --bench feature_tax
//!   cargo bench --features "gfm ast"  --bench feature_tax

use std::hint::black_box;
use std::time::Instant;

const DATA: &str = include_str!("../tests/fixtures/data.md");

/// Best-of-`trials` ns/op, each trial averaging `iters` runs after a warm-up.
/// The minimum is the most stable estimator for a microbenchmark — it strips
/// scheduler/background noise, so the base path is comparable across builds.
fn time(iters: u32, mut f: impl FnMut()) -> f64 {
    let warm = (iters / 5).max(50);
    for _ in 0..warm {
        f();
    }
    let mut best = f64::INFINITY;
    for _ in 0..5 {
        let t = Instant::now();
        for _ in 0..iters {
            f();
        }
        let per = t.elapsed().as_nanos() as f64 / iters as f64;
        best = best.min(per);
    }
    best
}

fn main() {
    let iters = 1500u32;
    let bytes = DATA.len() as f64;
    let kb = DATA.len() / 1024;

    // Which features this binary was compiled with (drives the table + the
    // cross-build comparison).
    let feats = {
        let mut v: Vec<&str> = Vec::new();
        if cfg!(feature = "gfm") {
            v.push("gfm");
        }
        if cfg!(feature = "ast") {
            v.push("ast");
        }
        if v.is_empty() {
            "(default — pure CommonMark)".to_string()
        } else {
            v.join(" + ")
        }
    };

    // (label, ns/op). The base path is always present; the rest are feature-gated.
    let mut rows: Vec<(&str, f64)> = Vec::new();

    rows.push((
        "CommonMark (to_html)",
        time(iters, || {
            black_box(sparkdown::to_html(black_box(DATA)));
        }),
    ));

    #[cfg(feature = "gfm")]
    {
        let opts = sparkdown::Options::gfm();
        rows.push((
            "GFM (to_html_with)",
            time(iters, || {
                black_box(sparkdown::to_html_with(black_box(DATA), &opts));
            }),
        ));
    }

    #[cfg(feature = "ast")]
    {
        rows.push((
            "AST build (to_mdast)",
            time(iters, || {
                black_box(sparkdown::ast::to_mdast(black_box(DATA)));
            }),
        ));
        rows.push((
            "AST -> JSON (mdast_json)",
            time(iters, || {
                black_box(sparkdown::ast::to_mdast_json(black_box(DATA)));
            }),
        ));
    }

    let base = rows[0].1;
    let us = |ns: f64| ns / 1000.0;
    let mbps = |ns: f64| bytes / ns; // bytes/ns == GB/s*… ; bytes/ns * 1e3 = MB/s

    println!("\nfeature-tax — CommonMark spec ({kb} KB), {iters} iters");
    println!("build features: {feats}\n");
    println!(
        "  {:<26} {:>9} {:>9} {:>8}",
        "path", "us/op", "MB/s", "vs base"
    );
    println!("  {:-<26} {:->9} {:->9} {:->8}", "", "", "", "");
    for (label, ns) in &rows {
        println!(
            "  {:<26} {:>9.1} {:>9.0} {:>7.2}x",
            label,
            us(*ns),
            mbps(*ns) * 1000.0,
            ns / base
        );
    }
    println!();
}
