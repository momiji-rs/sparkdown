//! Tests for the opt-in `src/ext/*` extensions (beyond CommonMark + GFM). Each
//! is gated by its Cargo feature, mirroring the GFM tests in `tests/options.rs`.

#[cfg(feature = "diagram")]
mod diagram {
    use sparkdown::{Options, to_html_with};

    const MERMAID: &str = "```mermaid\ngraph TD; A-->B;\n```\n";

    #[test]
    fn renders_client_side_wrapper() {
        let opts = Options {
            diagram: true,
            ..Options::default()
        };
        // `-->` is HTML-escaped; the browser un-escapes it back in textContent.
        assert_eq!(
            to_html_with(MERMAID, &opts),
            "<pre class=\"mermaid\">graph TD; A--&gt;B;\n</pre>\n"
        );
    }

    #[test]
    fn off_by_default_is_a_normal_code_block() {
        let html = to_html_with(MERMAID, &Options::default());
        assert!(
            html.contains("<pre><code class=\"language-mermaid\">"),
            "{html}"
        );
    }

    #[test]
    fn non_diagram_language_is_untouched() {
        let opts = Options {
            diagram: true,
            ..Options::default()
        };
        let html = to_html_with("```rust\nfn x() {}\n```\n", &opts);
        assert!(
            html.contains("<pre><code class=\"language-rust\">"),
            "{html}"
        );
    }
}

#[cfg(feature = "ast")]
mod frontmatter {
    use sparkdown::Options;
    use sparkdown::ast::to_mdast_json_opts;

    fn on() -> Options {
        Options {
            frontmatter: true,
            ..Options::default()
        }
    }

    #[test]
    fn yaml_node() {
        // A `yaml` node whose value is the content between the fences, no newline.
        let j = to_mdast_json_opts("---\ntitle: Hi\nx: 1\n---\n", on());
        assert!(j.contains(r#""type":"yaml","value":"title: Hi\nx: 1""#), "{j}");
        // Positioned at the document start, ending at the closing fence.
        assert!(j.contains(r#""start":{"line":1,"column":1,"offset":0}"#), "{j}");
    }

    #[test]
    fn toml_node() {
        let j = to_mdast_json_opts("+++\na = 1\n+++\n", on());
        assert!(j.contains(r#""type":"toml","value":"a = 1""#), "{j}");
    }

    #[test]
    fn empty_value() {
        let j = to_mdast_json_opts("---\n---\n", on());
        assert!(j.contains(r#""type":"yaml","value":"""#), "{j}");
    }

    #[test]
    fn off_by_default_is_not_frontmatter() {
        // Without the option, the leading `---` is ordinary markdown — no yaml node.
        let j = to_mdast_json_opts("---\ntitle: Hi\n---\n", Options::default());
        assert!(!j.contains(r#""type":"yaml""#), "{j}");
    }
}

#[cfg(feature = "ast")]
mod footnotes {
    use sparkdown::Options;
    use sparkdown::ast::to_mdast_json_opts;

    fn on() -> Options {
        Options {
            footnotes: true,
            ..Options::default()
        }
    }

    #[test]
    fn reference_and_definition() {
        let j = to_mdast_json_opts("A[^a]\n\n[^a]: note\n", on());
        assert!(j.contains(r#""type":"footnoteReference","identifier":"a","label":"a""#), "{j}");
        assert!(j.contains(r#""type":"footnoteDefinition","identifier":"a","label":"a""#), "{j}");
    }

    #[test]
    fn identifier_is_lowercased() {
        let j = to_mdast_json_opts("A[^Foo]\n\n[^Foo]: note\n", on());
        assert!(j.contains(r#""type":"footnoteReference","identifier":"foo","label":"Foo""#), "{j}");
    }

    #[test]
    fn reference_without_definition_is_literal() {
        // No matching definition → no footnoteReference node.
        let j = to_mdast_json_opts("see [^x] here\n", on());
        assert!(!j.contains(r#""type":"footnoteReference""#), "{j}");
    }

    #[test]
    fn off_by_default() {
        let j = to_mdast_json_opts("A[^a]\n\n[^a]: note\n", Options::default());
        assert!(!j.contains("footnote"), "{j}");
    }
}
