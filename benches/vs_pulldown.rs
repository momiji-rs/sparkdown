//! sparkdown vs pulldown-cmark.
//!
//! Throughput on the CommonMark spec (`data.md`, ~200 KB), the same fixture
//! `cmark` and yuin/rushdown benchmark. Run: `cargo bench`. Both engines parse
//! the full document (sparkdown is 100% conformant), so the comparison is fair;
//! the wider cross-engine field is in the project README.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

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
