//! Tests for the opt-in `src/ext/*` extensions (beyond CommonMark + GFM). Each
//! is gated by its Cargo feature, mirroring the GFM tests in `tests/options.rs`.

#[cfg(feature = "diagram")]
mod diagram {
    use sparkdown::{to_html_with, Options};

    const MERMAID: &str = "```mermaid\ngraph TD; A-->B;\n```\n";

    #[test]
    fn renders_client_side_wrapper() {
        let opts = Options { diagram: true, ..Options::default() };
        // `-->` is HTML-escaped; the browser un-escapes it back in textContent.
        assert_eq!(
            to_html_with(MERMAID, &opts),
            "<pre class=\"mermaid\">graph TD; A--&gt;B;\n</pre>\n"
        );
    }

    #[test]
    fn off_by_default_is_a_normal_code_block() {
        let html = to_html_with(MERMAID, &Options::default());
        assert!(html.contains("<pre><code class=\"language-mermaid\">"), "{html}");
    }

    #[test]
    fn non_diagram_language_is_untouched() {
        let opts = Options { diagram: true, ..Options::default() };
        let html = to_html_with("```rust\nfn x() {}\n```\n", &opts);
        assert!(html.contains("<pre><code class=\"language-rust\">"), "{html}");
    }
}
