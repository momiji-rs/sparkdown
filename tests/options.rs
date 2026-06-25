//! Coverage for the opt-in [`Options`] surface. Each enabled feature is tested
//! for its effect *and* for not disturbing the default path.

use sparkdown::{Options, Renderer, to_html, to_html_with};

#[test]
fn default_matches_to_html() {
    // `to_html_with(&Options::default())` must be identical to `to_html`.
    for src in [
        "# h\n\npara *em* `code`\n\n- a\n- b\n",
        "> quote\n\n```\nfenced\n```\n",
        "a [link](/u) and ~~tilde~~ and www.x.com\n",
    ] {
        assert_eq!(to_html(src), to_html_with(src, &Options::default()));
    }
}

#[test]
fn hard_wraps() {
    let on = Options {
        hard_wraps: true,
        ..Options::default()
    };
    // Soft break becomes <br /> only when enabled.
    assert_eq!(to_html("a\nb"), "<p>a\nb</p>\n");
    assert_eq!(to_html_with("a\nb", &on), "<p>a<br />\nb</p>\n");
    // An explicit hard break (two trailing spaces) is <br /> either way.
    assert_eq!(to_html("a  \nb"), "<p>a<br />\nb</p>\n");
    assert_eq!(to_html_with("a  \nb", &on), "<p>a<br />\nb</p>\n");
}

#[test]
fn renderer_carries_options() {
    let mut r = Renderer::with_options(Options {
        hard_wraps: true,
        ..Options::default()
    });
    assert_eq!(r.render("a\nb"), "<p>a<br />\nb</p>\n");
    assert_eq!(r.render("c\nd"), "<p>c<br />\nd</p>\n"); // reused buffers + opts
}

#[test]
fn gfm_preset_is_all_on() {
    let g = Options::gfm();
    assert!(g.strikethrough && g.tasklist && g.autolink && g.tagfilter && g.tables);
}

#[test]
fn strikethrough() {
    let st = Options {
        strikethrough: true,
        ..Options::default()
    };
    let go = |s: &str| to_html_with(s, &st);
    // One or two tildes both strike (GFM spec example 491).
    assert_eq!(go("~~foo~~"), "<p><del>foo</del></p>\n");
    assert_eq!(go("~foo~"), "<p><del>foo</del></p>\n");
    assert_eq!(
        go("~~Hi~~ Hello, ~there~ world!"),
        "<p><del>Hi</del> Hello, <del>there</del> world!</p>\n"
    );
    // `~`-like (not `_`-like): intraword strikes.
    assert_eq!(go("a~~b~~c"), "<p>a<del>b</del>c</p>\n");
    // Mismatched run lengths don't match; flanking blocks padded delimiters.
    assert_eq!(go("~~foo~"), "<p>~~foo~</p>\n");
    assert_eq!(go("~~ foo ~~"), "<p>~~ foo ~~</p>\n");
    // 3+ tildes on their own line is a CommonMark tilde code fence, not strike.
    assert_eq!(
        go("~~~foo~~~"),
        "<pre><code class=\"language-foo~~~\"></code></pre>\n"
    );
    // Off by default.
    assert_eq!(to_html("~~foo~~"), "<p>~~foo~~</p>\n");
}

#[test]
fn tasklist() {
    let tl = Options {
        tasklist: true,
        ..Options::default()
    };
    let go = |s: &str| to_html_with(s, &tl);
    // Tight list: bare <li>, checkbox replaces the marker (GFM §5.3).
    assert_eq!(
        go("- [ ] foo\n- [x] bar\n"),
        "<ul>\n<li><input disabled=\"\" type=\"checkbox\"> foo</li>\n\
         <li><input checked=\"\" disabled=\"\" type=\"checkbox\"> bar</li>\n</ul>\n"
    );
    // Loose list: checkbox sits at the start of the wrapped <p>.
    assert_eq!(
        go("- [ ] a\n\n- [x] b\n"),
        "<ul>\n<li>\n<p><input disabled=\"\" type=\"checkbox\"> a</p>\n</li>\n\
         <li>\n<p><input checked=\"\" disabled=\"\" type=\"checkbox\"> b</p>\n</li>\n</ul>\n"
    );
    // `[X]` and `*` bullets work; a marker not followed by space is not a task.
    assert_eq!(
        go("* [X] up\n"),
        "<ul>\n<li><input checked=\"\" disabled=\"\" type=\"checkbox\"> up</li>\n</ul>\n"
    );
    assert_eq!(go("- [ ]x\n"), "<ul>\n<li>[ ]x</li>\n</ul>\n");
    // Off by default.
    assert_eq!(to_html("- [ ] foo\n"), "<ul>\n<li>[ ] foo</li>\n</ul>\n");
}
