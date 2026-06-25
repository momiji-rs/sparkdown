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

#[test]
fn tagfilter() {
    let tf = Options {
        tagfilter: true,
        ..Options::default()
    };
    let go = |s: &str| to_html_with(s, &tf);
    // Inline raw HTML — only blacklisted tags are neutralized (GFM §6.11).
    assert_eq!(
        go("<strong> <title> <style> <em>\n"),
        "<p><strong> &lt;title> &lt;style> <em></p>\n"
    );
    // Prefix match alone is not enough — `<scriptx>` is left intact.
    assert_eq!(
        go("<scriptx> and <script>\n"),
        "<p><scriptx> and &lt;script></p>\n"
    );
    // HTML block: opening tag filtered, closing tag (`</`) left as-is.
    assert_eq!(
        go("<div>\n<script>\nok\n</script>\n</div>\n"),
        "<div>\n&lt;script>\nok\n</script>\n</div>\n"
    );
    // Off by default (inline raw HTML passes through verbatim).
    assert_eq!(to_html("a <title> b\n"), "<p>a <title> b</p>\n");
}

#[test]
fn tables() {
    let t = Options {
        tables: true,
        ..Options::default()
    };
    let go = |s: &str| to_html_with(s, &t);
    // Basic table: thead + tbody, cells parsed as inline (GFM §4.10).
    assert_eq!(
        go("| foo | bar |\n| --- | --- |\n| baz | bim |\n"),
        "<table>\n<thead>\n<tr>\n<th>foo</th>\n<th>bar</th>\n</tr>\n</thead>\n\
         <tbody>\n<tr>\n<td>baz</td>\n<td>bim</td>\n</tr>\n</tbody>\n</table>\n"
    );
    // Alignment via the delimiter row; outer pipes optional on data rows.
    assert_eq!(
        go("| abc | defghi |\n:-: | -----------:\nbar | baz\n"),
        "<table>\n<thead>\n<tr>\n<th align=\"center\">abc</th>\n\
         <th align=\"right\">defghi</th>\n</tr>\n</thead>\n<tbody>\n<tr>\n\
         <td align=\"center\">bar</td>\n<td align=\"right\">baz</td>\n</tr>\n\
         </tbody>\n</table>\n"
    );
    // `\|` is an escaped pipe (→ `|`) resolved before inline parsing (ex. 200).
    assert_eq!(
        go("| f\\|oo |\n| ------ |\n| b `\\|` az |\n"),
        "<table>\n<thead>\n<tr>\n<th>f|oo</th>\n</tr>\n</thead>\n<tbody>\n<tr>\n\
         <td>b <code>|</code> az</td>\n</tr>\n</tbody>\n</table>\n"
    );
    // Header + delimiter with no data rows → no <tbody>.
    assert_eq!(
        go("| abc | def |\n| --- | --- |\n"),
        "<table>\n<thead>\n<tr>\n<th>abc</th>\n<th>def</th>\n</tr>\n</thead>\n</table>\n"
    );
    // A column-count mismatch is not a table.
    assert_eq!(go("| a | b |\n| - |\n"), "<p>| a | b |\n| - |</p>\n");
    // Off by default.
    assert_eq!(
        to_html("| a | b |\n| - | - |\n"),
        "<p>| a | b |\n| - | - |</p>\n"
    );
}

#[test]
fn autolink() {
    let a = Options {
        autolink: true,
        ..Options::default()
    };
    let go = |s: &str| to_html_with(s, &a);
    // `www.` gets an http:// href; visible text is verbatim (GFM §6.9).
    assert_eq!(
        go("Visit www.commonmark.org/help\n"),
        "<p>Visit <a href=\"http://www.commonmark.org/help\">www.commonmark.org/help</a></p>\n"
    );
    // Scheme URLs; `&` is part of the URL (overrides the entity scan) and escaped.
    assert_eq!(
        go("x https://a.org/p?a=1&b=2 y\n"),
        "<p>x <a href=\"https://a.org/p?a=1&amp;b=2\">https://a.org/p?a=1&amp;b=2</a> y</p>\n"
    );
    // Bare email → mailto.
    assert_eq!(
        go("ask foo.bar@example.com please\n"),
        "<p>ask <a href=\"mailto:foo.bar@example.com\">foo.bar@example.com</a> please</p>\n"
    );
    // Trailing `.` is trimmed; an unbalanced `)` is excluded.
    assert_eq!(
        go("see www.x.org/a.b. ok\n"),
        "<p>see <a href=\"http://www.x.org/a.b\">www.x.org/a.b</a>. ok</p>\n"
    );
    assert_eq!(
        go("(www.example.com)\n"),
        "<p>(<a href=\"http://www.example.com\">www.example.com</a>)</p>\n"
    );
    // No false positives, and off by default.
    assert_eq!(go("website hhttp plain\n"), "<p>website hhttp plain</p>\n");
    assert_eq!(
        to_html("see http://example.com\n"),
        "<p>see http://example.com</p>\n"
    );
    // Full path: autolinks alongside emphasis / links in the same paragraph; a
    // URL trigger fires before any `_`/`*` in the path becomes a delimiter.
    assert_eq!(
        go("**bold** http://example.com\n"),
        "<p><strong>bold</strong> <a href=\"http://example.com\">http://example.com</a></p>\n"
    );
    assert_eq!(
        go("see *this*: www.x.org/p?a=1&b=2\n"),
        "<p>see <em>this</em>: <a href=\"http://www.x.org/p?a=1&amp;b=2\">www.x.org/p?a=1&amp;b=2</a></p>\n"
    );
    assert_eq!(
        go("x http://example.com/a_b yz\n"),
        "<p>x <a href=\"http://example.com/a_b\">http://example.com/a_b</a> yz</p>\n"
    );
}
