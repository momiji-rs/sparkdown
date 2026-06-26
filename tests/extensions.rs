//! Tests for the opt-in `src/ext/*` extensions (beyond CommonMark + GFM). Each
//! is gated by its Cargo feature, mirroring the GFM tests in `tests/options.rs`.

#[cfg(feature = "emoji")]
mod emoji {
    use sparkdown::{Options, to_html, to_html_with};

    fn on() -> Options {
        Options {
            emoji: true,
            ..Options::default()
        }
    }

    #[test]
    fn basic_shortcodes() {
        assert_eq!(to_html_with(":smile:\n", &on()), "<p>😄</p>\n");
        // No word boundary required; `+`/`-` are valid shortcode chars.
        assert_eq!(to_html_with("a:smile:b\n", &on()), "<p>a😄b</p>\n");
        assert_eq!(to_html_with(":+1: :-1:\n", &on()), "<p>👍 👎</p>\n");
    }

    #[test]
    fn unknown_and_case_sensitive_stay_literal() {
        assert_eq!(
            to_html_with(":notanemoji:\n", &on()),
            "<p>:notanemoji:</p>\n"
        );
        // Shortcodes are lowercase; `:SMILE:` is not a match.
        assert_eq!(to_html_with(":SMILE:\n", &on()), "<p>:SMILE:</p>\n");
        // No closing colon → literal.
        assert_eq!(to_html_with(":smile\n", &on()), "<p>:smile</p>\n");
    }

    #[test]
    fn nested_colons_match_inner() {
        assert_eq!(to_html_with("::smile::\n", &on()), "<p>:😄:</p>\n");
    }

    #[test]
    fn code_span_is_untouched() {
        assert_eq!(
            to_html_with("`:smile:`\n", &on()),
            "<p><code>:smile:</code></p>\n"
        );
    }

    #[test]
    fn off_by_default() {
        assert_eq!(to_html(":smile:\n"), "<p>:smile:</p>\n");
    }
}

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

#[cfg(all(feature = "ast", feature = "frontmatter"))]
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
        assert!(
            j.contains(r#""type":"yaml","value":"title: Hi\nx: 1""#),
            "{j}"
        );
        // Positioned at the document start, ending at the closing fence.
        assert!(
            j.contains(r#""start":{"line":1,"column":1,"offset":0}"#),
            "{j}"
        );
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

#[cfg(feature = "deflist")]
mod definition_lists {
    use sparkdown::{Options, to_html_with};

    fn on() -> Options {
        Options {
            deflist: true,
            ..Options::default()
        }
    }

    #[test]
    fn tight_single() {
        assert_eq!(
            to_html_with("Term\n: Definition\n", &on()),
            "<dl>\n<dt>Term</dt>\n<dd>Definition\n</dd>\n</dl>\n"
        );
    }

    #[test]
    fn multi_term_multi_def() {
        assert_eq!(
            to_html_with("T1\nT2\n: D\n", &on()),
            "<dl>\n<dt>T1</dt>\n<dt>T2</dt>\n<dd>D\n</dd>\n</dl>\n"
        );
        assert_eq!(
            to_html_with("Apple\n: fruit\n: company\n", &on()),
            "<dl>\n<dt>Apple</dt>\n<dd>fruit\n</dd>\n<dd>company\n</dd>\n</dl>\n"
        );
    }

    #[test]
    fn loose_wraps_in_paragraph() {
        // A blank line before the marker makes the description loose.
        assert_eq!(
            to_html_with("Term\n\n: Loose def\n", &on()),
            "<dl>\n<dt>Term</dt>\n<dd>\n<p>Loose def</p>\n</dd>\n</dl>\n"
        );
    }

    #[test]
    fn groups_merge_across_blanks() {
        assert_eq!(
            to_html_with("T1\n: D1\n\nT2\n: D2\n", &on()),
            "<dl>\n<dt>T1</dt>\n<dd>D1\n</dd>\n<dt>T2</dt>\n<dd>D2\n</dd>\n</dl>\n"
        );
    }

    #[test]
    fn trailing_paragraph_is_evicted() {
        assert_eq!(
            to_html_with("before\n\nTerm\n: Def\n\nafter\n", &on()),
            "<p>before</p>\n<dl>\n<dt>Term</dt>\n<dd>Def\n</dd>\n</dl>\n<p>after</p>\n"
        );
    }

    #[test]
    fn inline_markup_in_term_and_def() {
        assert_eq!(
            to_html_with("*Rich* term\n: def with *em*\n", &on()),
            "<dl>\n<dt><em>Rich</em> term</dt>\n<dd>def with <em>em</em>\n</dd>\n</dl>\n"
        );
    }

    #[test]
    fn marker_requires_space() {
        // `:NoSpace` is not a marker → plain paragraph.
        assert_eq!(
            to_html_with("Term\n:NoSpace\n", &on()),
            "<p>Term\n:NoSpace</p>\n"
        );
        // An orphan marker (no preceding term) stays literal.
        assert_eq!(to_html_with(": orphan\n", &on()), "<p>: orphan</p>\n");
    }

    #[test]
    fn off_by_default() {
        assert_eq!(
            to_html_with("Term\n: Definition\n", &Options::default()),
            "<p>Term\n: Definition</p>\n"
        );
    }
}

#[cfg(all(feature = "ast", feature = "deflist"))]
mod definition_lists_mdast {
    use sparkdown::Options;
    use sparkdown::ast::to_mdast_json_opts;

    fn on() -> Options {
        Options {
            deflist: true,
            ..Options::default()
        }
    }

    #[test]
    fn custom_node_types() {
        // remark-definition-list shape: defList → defListTerm + defListDescription.
        let j = to_mdast_json_opts("Term\n: Def\n", on());
        assert!(j.contains(r#""type":"defList""#), "{j}");
        assert!(j.contains(r#""type":"defListTerm""#), "{j}");
        assert!(
            j.contains(r#""type":"defListDescription","spread":false"#),
            "{j}"
        );
    }

    #[test]
    fn loose_sets_spread() {
        let j = to_mdast_json_opts("Term\n\n: Def\n", on());
        assert!(
            j.contains(r#""type":"defListDescription","spread":true"#),
            "{j}"
        );
    }

    #[test]
    fn off_by_default() {
        let j = to_mdast_json_opts("Term\n: Def\n", Options::default());
        assert!(!j.contains("defList"), "{j}");
    }
}

#[cfg(feature = "directives")]
mod directives {
    use sparkdown::{Options, to_html_with};

    fn on() -> Options {
        Options {
            directives: true,
            ..Options::default()
        }
    }

    #[test]
    fn text_directive() {
        // Convention: name → element, attributes → HTML attributes; label inline.
        assert_eq!(
            to_html_with(":name[label]{#id .cls}\n", &on()),
            "<p><name id=\"id\" class=\"cls\">label</name></p>\n"
        );
    }

    #[test]
    fn text_directive_mid_paragraph() {
        assert_eq!(
            to_html_with("a :em[x] b\n", &on()),
            "<p>a <em>x</em> b</p>\n"
        );
    }

    #[test]
    fn trailing_colon_is_not_a_directive() {
        // `:foo:` is an emoji-shaped token, not a directive (matches micromark).
        assert_eq!(to_html_with(":foo: bar\n", &on()), "<p>:foo: bar</p>\n");
    }

    #[test]
    fn leaf_directive() {
        assert_eq!(
            to_html_with("::leaf[lab]{.warn}\n", &on()),
            "<leaf class=\"warn\">lab</leaf>\n"
        );
    }

    #[test]
    fn container_directive() {
        assert_eq!(
            to_html_with(":::note\n## Title\n\ntext\n:::\n", &on()),
            "<note>\n<h2>Title</h2>\n<p>text</p>\n</note>\n"
        );
    }

    #[test]
    fn off_by_default() {
        assert_eq!(
            to_html_with(":name[x]\n", &Options::default()),
            "<p>:name[x]</p>\n"
        );
        assert_eq!(
            to_html_with(":::note\nbody\n:::\n", &Options::default()),
            "<p>:::note\nbody\n:::</p>\n"
        );
    }
}

#[cfg(all(feature = "ast", feature = "directives"))]
mod directives_mdast {
    use sparkdown::Options;
    use sparkdown::ast::to_mdast_json_opts;

    fn on() -> Options {
        Options {
            directives: true,
            ..Options::default()
        }
    }

    #[test]
    fn text_directive_node() {
        let j = to_mdast_json_opts(":name[label]{#id key=val}\n", on());
        assert!(
            j.contains(
                r#""type":"textDirective","name":"name","attributes":{"id":"id","key":"val"}"#
            ),
            "{j}"
        );
        // The label is inline children.
        assert!(j.contains(r#""type":"text","value":"label""#), "{j}");
    }

    #[test]
    fn leaf_and_container_nodes() {
        let j = to_mdast_json_opts("::leaf{.warn}\n", on());
        assert!(j.contains(r#""type":"leafDirective","name":"leaf""#), "{j}");
        let c = to_mdast_json_opts(":::note\nbody\n:::\n", on());
        assert!(
            c.contains(r#""type":"containerDirective","name":"note""#),
            "{c}"
        );
    }

    #[test]
    fn container_label_is_directive_label_paragraph() {
        let j = to_mdast_json_opts(":::note[Title]\nbody\n:::\n", on());
        assert!(j.contains(r#""data":{"directiveLabel":true}"#), "{j}");
    }

    #[test]
    fn off_by_default() {
        let j = to_mdast_json_opts(":name[x]\n", Options::default());
        assert!(!j.contains("Directive"), "{j}");
    }
}

#[cfg(all(feature = "ast", feature = "footnotes"))]
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
        assert!(
            j.contains(r#""type":"footnoteReference","identifier":"a","label":"a""#),
            "{j}"
        );
        assert!(
            j.contains(r#""type":"footnoteDefinition","identifier":"a","label":"a""#),
            "{j}"
        );
    }

    #[test]
    fn identifier_is_lowercased() {
        let j = to_mdast_json_opts("A[^Foo]\n\n[^Foo]: note\n", on());
        assert!(
            j.contains(r#""type":"footnoteReference","identifier":"foo","label":"Foo""#),
            "{j}"
        );
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
