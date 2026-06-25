//! SPIKE: emit sparkdown's mdast (+ our HTML) as JSON for the JS harness.
//!
//! Reads `tests/fixtures/spec.json` (the 652 CommonMark examples) and prints a
//! JSON array of `{ example, section, markdown, html, mdast }`, where `mdast` is
//! sparkdown's nested mdast. The mdast is produced by the lib's own zero-dep
//! serializer (`ast::to_mdast_json`) — the *same* one that crosses the wasm
//! boundary — so the harness validates exactly what wasm would emit.
//!
//! Run: `cargo run --release --features ast --example mdast_json > harness/sparkdown-mdast.json`

use serde_json::{Value, json};

const SPEC: &str = include_str!("../tests/fixtures/spec.json");

fn main() {
    let examples: Vec<Value> = serde_json::from_str(SPEC).expect("spec.json");
    let mut out = Vec::with_capacity(examples.len());
    for ex in &examples {
        let md = ex["markdown"].as_str().unwrap_or("");
        let mdast: Value =
            serde_json::from_str(&sparkdown::ast::to_mdast_json(md)).expect("valid mdast json");
        out.push(json!({
            "example": ex["example"].clone(),
            "section": ex["section"].clone(),
            "markdown": md,
            "html": sparkdown::to_html(md),
            "mdast": mdast,
        }));
    }
    println!("{}", Value::Array(out));
}
