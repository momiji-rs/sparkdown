//! SPIKE: measure the cost of building a nested mdast vs the direct fast path.
//!
//!   fast:  to_html(src)     source -> HTML
//!   mdast: to_mdast(src)    source -> nested owned mdast (blocks + inline nodes)
//!
//! Manual timing (no criterion), CommonMark spec fixture (~200 KB).
//! Run: `cargo bench --features ast --bench ast_tax`.

use std::hint::black_box;
use std::time::Instant;

use sparkdown::ast::{node_count, to_mdast};

const DATA: &str = include_str!("../tests/fixtures/data.md");

fn time(iters: u32, mut f: impl FnMut()) -> f64 {
    let warm = (iters / 5).max(50);
    for _ in 0..warm {
        f();
    }
    let t = Instant::now();
    for _ in 0..iters {
        f();
    }
    t.elapsed().as_nanos() as f64 / iters as f64
}

fn main() {
    let nodes = node_count(&to_mdast(DATA));
    let iters = 3000u32;

    let fast = time(iters, || {
        black_box(sparkdown::to_html(black_box(DATA)));
    });
    let mdast = time(iters, || {
        black_box(to_mdast(black_box(DATA)));
    });

    let us = |ns: f64| ns / 1000.0;
    println!(
        "\nmdast build cost — CommonMark spec ({} KB), {iters} iters\n",
        DATA.len() / 1024
    );
    println!("  nested mdast nodes : {nodes}\n");
    println!("  {:<20} {:>9} {:>8}", "path", "us/op", "vs fast");
    println!("  {:-<20} {:->9} {:->8}", "", "", "");
    println!("  {:<20} {:>9.2} {:>7.2}x", "fast (to_html)", us(fast), 1.0);
    println!(
        "  {:<20} {:>9.2} {:>7.2}x",
        "to_mdast",
        us(mdast),
        mdast / fast
    );
    println!();
}
