//! sparkdown vs pulldown-cmark — the speed bar to beat.
//!
//! Throughput on the CommonMark spec (`data.md`, ~198 KB), the same fixture
//! yuin/rushdown benchmarks. Run: `cargo bench`.
//!
//! NOTE (scaffold): until conformance is high the sparkdown number is
//! **meaningless for comparison** — the parser currently emits only
//! paragraphs and skips most of the document, so it looks artificially fast.
//! The harness exists so the speed gap is measurable from the first real
//! feature onward.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};

const DATA: &str = include_str!("../tests/fixtures/data.md");

fn bench(c: &mut Criterion) {
    let mut g = c.benchmark_group("commonmark-spec-198KB");
    g.throughput(Throughput::Bytes(DATA.len() as u64));

    g.bench_function("sparkdown", |b| {
        b.iter(|| criterion::black_box(sparkdown::to_html(DATA)))
    });

    g.bench_function("pulldown-cmark", |b| {
        b.iter(|| {
            let parser = pulldown_cmark::Parser::new(DATA);
            let mut out = String::new();
            pulldown_cmark::html::push_html(&mut out, parser);
            criterion::black_box(out);
        })
    });

    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
