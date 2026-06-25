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
