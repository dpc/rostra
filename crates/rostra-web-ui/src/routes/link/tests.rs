use super::*;

#[test]
fn remote_fetch_policy_rejects_special_use_addresses() {
    for ip in [
        "0.0.0.0",
        "10.0.0.1",
        "127.0.0.1",
        "169.254.169.254",
        "172.16.1.1",
        "192.168.1.1",
        "100.64.1.1",
        "192.0.0.1",
        "192.0.2.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "255.255.255.255",
        "::1",
        "::",
        "::ffff:8.8.8.8",
        "fc00::1",
        "fe80::1",
        "ff02::1",
        "2001:db8::1",
        "2001::1",
        "2001:1ff:ffff::1",
        "2001:2::1",
        "2001:20::1",
        "2002:7f00:1::",
        "3fff::1",
    ] {
        assert!(!public_ip(ip.parse().unwrap()), "{ip}");
    }
    for ip in [
        "8.8.8.8",
        "1.1.1.1",
        "2606:4700:4700::1111",
        "2001:4860:4802:32::77",
    ] {
        assert!(public_ip(ip.parse().unwrap()), "{ip}");
    }
}

#[test]
fn every_dns_answer_must_be_public_before_any_connection() {
    let public = [
        "216.239.38.119:443".parse().unwrap(),
        "[2001:4860:4802:32::77]:443".parse().unwrap(),
    ];
    assert!(public_addrs(&public));
    assert!(!public_addrs(&[]));
    for special in [
        "127.0.0.1:443",
        "169.254.169.254:443",
        "[::1]:443",
        "[::ffff:216.239.38.119]:443",
        "[2001:db8::1]:443",
    ] {
        assert!(!public_addrs(&[public[0], special.parse().unwrap()]));
        assert!(!public_addrs(&[special.parse().unwrap(), public[1]]));
    }
}

#[tokio::test]
async fn blocked_destinations_fail_without_network_access() {
    for url in [
        "http://127.0.0.1/",
        "http://[::1]/",
        "http://169.254.169.254/",
        "https://example.com:8443/",
    ] {
        assert!(fetch_title(&Url::parse(url).unwrap()).await.is_none());
    }
}

#[test]
fn insertion_url_policy_excludes_credentials_and_active_schemes() {
    for url in [
        "javascript:alert(1)",
        "data:text/html,test",
        "ftp://example.com/",
        "https://user:secret@example.com/",
        "https://user@example.com/",
        "not a url",
    ] {
        assert!(parse_url(url).is_none(), "{url}");
    }
    assert!(parse_url(" https://example.com/a?q=x ").is_some());
    // Local destinations can be inserted, but never fetched.
    assert!(parse_url("http://localhost/").is_some());
}

#[test]
fn metadata_is_plain_text_bounded_and_prefers_open_graph() {
    assert_eq!(
        metadata_title("<title>Fallback</title><meta property='og:title' content='A &amp; B'>"),
        Some("A & B".into())
    );
    assert_eq!(metadata_title("<title> A\n B </title>"), Some("A B".into()));
    assert_eq!(metadata_title("<html>No title</html>"), None);
    assert_eq!(
        metadata_title(&format!("<title>{}</title>", "a".repeat(400)))
            .unwrap()
            .len(),
        300
    );
}

#[test]
fn syntax_matches_existing_embed_recognition() {
    for url in [
        "https://youtube.com/watch?v=123",
        "https://www.youtube.com/watch?v=123",
        "https://youtu.be/123",
        "https://x.com/alice/status/123",
        "https://twitter.com/alice/status/123",
    ] {
        assert!(link_syntax(&Url::parse(url).unwrap(), "Video").starts_with("![Video]("));
    }
    for url in [
        "https://example.com/",
        "https://youtube.com/",
        "https://youtube.com.evil.test/watch?v=123",
        "https://x.com/alice",
        "https://x.com.evil.test/alice/status/123",
    ] {
        assert!(link_syntax(&Url::parse(url).unwrap(), "Title").starts_with("[Title]("));
    }
}

#[tokio::test]
async fn hostile_titles_roundtrip_as_nonexecutable_link_text() {
    use jotup::r#async::AsyncRenderOutputExt;
    for title in [
        "[break](javascript:alert(1))",
        "<script>alert('x')</script>",
        "a\\b *bold* ![x]",
        "line\nbreak",
    ] {
        let syntax = link_syntax(&Url::parse("https://example.com/a(b)").unwrap(), title);
        let output =
            super::super::content::make_base_renderer(jotup::html::tokio::Renderer::default())
                .render_into_document(&syntax)
                .await
                .unwrap();
        let document = Html::parse_fragment(std::str::from_utf8(&output.into_inner()).unwrap());
        let links = Selector::parse("a").unwrap();
        let nodes: Vec<_> = document.select(&links).collect();
        assert_eq!(nodes.len(), 1, "{syntax}");
        assert_eq!(
            nodes[0].value().attr("href"),
            Some("https://example.com/a%28b%29")
        );
        assert_eq!(
            nodes[0].text().collect::<String>(),
            title.replace('\n', " ")
        );
        assert!(
            document
                .select(&Selector::parse("script, img, strong").unwrap())
                .next()
                .is_none()
        );
    }
}

#[test]
fn youtube_metadata_uses_fixed_endpoint_and_canonical_renderer_video_id() {
    for input in [
        "https://youtube.com/watch?v=M7lc1UVf-VE&list=playlist&t=20",
        "https://www.youtube.com/watch?v=M7lc1UVf-VE",
        "https://youtu.be/M7lc1UVf-VE?t=20",
    ] {
        let url = youtube_oembed_url(&Url::parse(input).unwrap()).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("www.youtube.com"));
        assert_eq!(url.path(), "/oembed");
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            vec![
                (
                    "url".into(),
                    "https://www.youtube.com/watch?v=M7lc1UVf-VE".into()
                ),
                ("format".into(), "json".into()),
            ]
        );
    }
    for input in [
        "https://example.com/watch?v=M7lc1UVf-VE",
        "https://youtube.com.evil.test/watch?v=M7lc1UVf-VE",
        "https://youtube.com/",
        "https://youtube.com/watch?v=invalid",
        "https://youtube.com/watch?v=M7lc1UVf-V%26",
    ] {
        assert!(youtube_oembed_url(&Url::parse(input).unwrap()).is_none());
    }
}

#[test]
fn x_metadata_uses_fixed_oembed_endpoint_and_discards_tracking() {
    let url = x_oembed_url(
        &Url::parse("https://x.com/HQNewsNow/status/2107261648339702108/video/1?s=20#frag")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(url.host_str(), Some("publish.x.com"));
    assert_eq!(url.path(), "/oembed");
    assert_eq!(
        url.query_pairs().collect::<Vec<_>>(),
        vec![
            (
                "url".into(),
                "https://twitter.com/HQNewsNow/status/2107261648339702108".into()
            ),
            ("omit_script".into(), "true".into()),
            ("dnt".into(), "true".into()),
        ]
    );
    for url in [
        "https://x.com.evil.test/alice/status/123",
        "https://alice@x.com/alice/status/123",
        "https://x.com/alice",
    ] {
        assert!(x_oembed_url(&Url::parse(url).unwrap()).is_none());
    }
}

#[test]
fn x_oembed_titles_drop_only_bare_trailing_shortlinks() {
    let title = |paragraph: &str| {
        x_oembed_title(
            serde_json::json!({"html": format!(
                "<blockquote class='twitter-tweet'><p>{paragraph}</p></blockquote>"
            )})
            .to_string()
            .as_bytes(),
        )
    };
    assert_eq!(
        title(
            "WAIT... WHAT? Anthropic has completely lost its mind. It’s over for OpenAI. <a href='https://t.co/LSN592B4Hh'>https://t.co/LSN592B4Hh</a>"
        ),
        Some("WAIT... WHAT? Anthropic has completely lost its mind. It’s over for OpenAI.".into())
    );
    assert_eq!(
        title("  A &amp; B<br>https://t.co/abc\u{a0}http://t.co/XYZ123  "),
        Some("A & B".into())
    );
    assert_eq!(title("https://t.co/abc"), None);
    assert_eq!(title("https://t.co/abc https://t.co/def"), None);
    assert_eq!(
        title("Too much winning... pic.twitter.com/b4wEQlVZYt"),
        Some("Too much winning...".into())
    );
    assert_eq!(
        title("Too much winning... <a href='https://t.co/abc'>pic.twitter.com/b4wEQlVZYt</a>"),
        Some("Too much winning...".into())
    );
    for prefix in [
        "pic.twitter.com/",
        "https://pic.twitter.com/",
        "http://pic.twitter.com/",
    ] {
        assert_eq!(
            title(&format!("Post {prefix}b4wEQlVZYt")),
            Some("Post".into())
        );
        assert_eq!(title(&format!("{prefix}b4wEQlVZYt")), None);
    }
    assert_eq!(
        title(
            "Post https://t.co/abc\u{a0}pic.twitter.com/b4wEQlVZYt http://pic.twitter.com/XYZ123"
        ),
        Some("Post".into())
    );
    for text in [
        "Read https://t.co/abc for details",
        "Read https://example.com/abc",
        "Read https://t.co.evil.test/abc",
        "Read https://t.co/",
        "Read https://t.co/abc.",
        "Read https://t.co/abc?x=1",
        "Read https://t.co/abc#fragment",
        "Read https://t.co/abc/def",
        "Read https://user@t.co/abc",
        "Read https://t.co:443/abc",
        "Read nothttps://t.co/abc",
        "Read pic.twitter.com/abc for details",
        "Read https://pic.twitter.com/abc for details",
        "Read pic.twitter.com.evil.test/abc",
        "Read pic.twitter.com/",
        "Read pic.twitter.com/abc.",
        "Read pic.twitter.com/abc?x=1",
        "Read pic.twitter.com/abc#fragment",
        "Read pic.twitter.com/abc/def",
        "Read https://user@pic.twitter.com/abc",
        "Read https://pic.twitter.com:443/abc",
        "Read nothttps://pic.twitter.com/abc",
    ] {
        assert_eq!(title(text), Some(text.into()), "{text}");
    }
    assert_eq!(
        title("Read <a href='https://t.co/abc'>example.com</a>"),
        Some("Read example.com".into())
    );
    assert_eq!(
        title("Read <a href='https://pic.twitter.com/abc'>the picture</a>"),
        Some("Read the picture".into())
    );
    assert_eq!(
        title(&format!("{} https://t.co/abc", "é".repeat(400))),
        Some("é".repeat(300))
    );
    assert_eq!(
        oembed_title(br#"{"title":"Video https://t.co/abc"}"#),
        Some("Video https://t.co/abc".into())
    );
    assert_eq!(
        metadata_title("<title>Page https://t.co/abc</title>"),
        Some("Page https://t.co/abc".into())
    );
    assert_eq!(
        oembed_title(br#"{"title":"Video pic.twitter.com/abc"}"#),
        Some("Video pic.twitter.com/abc".into())
    );
    assert_eq!(
        metadata_title("<title>Page pic.twitter.com/abc</title>"),
        Some("Page pic.twitter.com/abc".into())
    );
}

#[test]
fn x_oembed_titles_are_only_bounded_inert_post_text() {
    let json = serde_json::json!({"html":
        "<blockquote class=\"twitter-tweet\"><p>Hello <a href=\"javascript:evil()\">world</a><br>again<script>bad</script><style>bad</style><template>bad</template></p>author</blockquote><script>bad</script>"});
    assert_eq!(
        x_oembed_title(json.to_string().as_bytes()),
        Some("Hello world again".into())
    );
    for html in [
        "",
        "<blockquote class='twitter-tweet'><p> </p></blockquote>",
        "<script>text</script>",
        "<template><blockquote class='twitter-tweet'><p>hidden</p></blockquote></template>",
        "<p>not post text</p>",
    ] {
        assert!(x_oembed_title(serde_json::json!({"html": html}).to_string().as_bytes()).is_none());
    }
    assert_eq!(
        x_oembed_title(
            serde_json::json!({"html":
                format!("<blockquote class='twitter-tweet'><p>{}</p></blockquote>", "a".repeat(400))
            })
            .to_string()
            .as_bytes()
        )
        .unwrap()
        .len(),
        300
    );
}

#[tokio::test]
#[ignore = "Requires access to the public X oEmbed endpoint"]
async fn x_title_live_uses_the_production_safe_transport() {
    let url = Url::parse("https://x.com/HQNewsNow/status/2107261648339702108").unwrap();
    let mut addresses = tokio::net::lookup_host(("publish.x.com", 443))
        .await
        .expect("X metadata hostname must resolve for the live check");
    assert!(addresses.next().is_some());
    let title = fetch_title(&url)
        .await
        .expect("X post metadata must contain post text");
    assert!(!title.is_empty());
    assert!(title.chars().count() <= 300);
}

#[test]
fn oembed_uses_only_a_plain_bounded_title_and_rejects_unusable_responses() {
    let source = serde_json::json!({
        "title": " A\nvideo & <script>not markup</script> ",
        "html": "<script>bad()</script>",
        "thumbnail_url": "http://127.0.0.1/secret",
    });
    assert_eq!(
        oembed_title(&serde_json::to_vec(&source).unwrap()),
        Some("A video & <script>not markup</script>".into())
    );
    assert_eq!(
        oembed_title(&serde_json::to_vec(&serde_json::json!({"title": "a".repeat(400)})).unwrap())
            .unwrap()
            .len(),
        300
    );
    for body in [
        b"not JSON".as_slice(),
        b"{}",
        br#"{"title":null}"#,
        br#"{"title":42}"#,
        br#"{"title":"  "}"#,
        br#"{"html":"<title>wrong</title>"}"#,
    ] {
        assert!(oembed_title(body).is_none());
    }
}

#[test_log::test(tokio::test)]
#[ignore = "Requires access to the public YouTube oEmbed endpoint"]
async fn youtube_title_live_uses_the_production_safe_transport() {
    let title = fetch_title(&Url::parse("https://youtu.be/M7lc1UVf-VE").unwrap())
        .await
        .unwrap();
    assert!(!title.is_empty());
    assert!(title.len() <= 1200);
}

#[test]
fn ordinary_append_separator_uses_only_the_end_of_the_draft() {
    for (prefix, expected) in [
        ("", ""),
        ("text", " "),
        ("text ", ""),
        ("text\n", ""),
        ("text\t", ""),
        ("text\u{a0}", ""),
        ("text\u{85}", ""),
        ("text\u{feff}", " "),
        ("text\nmore", " "),
    ] {
        assert_eq!(link_separator(prefix), expected);
    }
}

#[test]
fn dialog_fields_use_placeholders_accessible_names_and_shared_buttons() {
    let form = LinkInput {
        content: "Draft".into(),
        url: "".into(),
        title: "".into(),
        action: "".into(),
        link_request: "test".into(),
    };
    for enhanced in [false, true] {
        let document = Html::parse_fragment(&link_form(&form, None, None, enhanced).into_string());
        assert!(
            document
                .select(&Selector::parse("label").unwrap())
                .next()
                .is_none()
        );
        for (id, name, placeholder) in [
            ("link-url", "Link", "https://…"),
            ("link-title", "Title (optional)", "Title (optional)"),
        ] {
            let input = document
                .select(&Selector::parse(&format!("#{id}")).unwrap())
                .next()
                .unwrap();
            assert_eq!(input.value().attr("aria-label"), Some(name));
            assert_eq!(input.value().attr("placeholder"), Some(placeholder));
        }
        let fetch = document
            .select(&Selector::parse(".m-linkForm__urlRow button").unwrap())
            .next()
            .unwrap();
        assert_eq!(fetch.text().collect::<String>(), "Fetch");
        assert_eq!(fetch.value().attr("name"), Some("action"));
        assert_eq!(fetch.value().attr("value"), Some("title"));
        let insert = document
            .select(&Selector::parse("button[name=action][value=insert]").unwrap())
            .next()
            .unwrap();
        assert_eq!(insert.text().collect::<String>(), "Insert");
        for button in document.select(&Selector::parse("button").unwrap()) {
            assert!(button.value().classes().any(|class| class == "u-button"));
            assert!(
                button
                    .select(&Selector::parse(".u-buttonIcon").unwrap())
                    .next()
                    .is_some()
            );
        }
        let cancel = document
            .select(&Selector::parse(".o-mediaList__closeButton").unwrap())
            .next()
            .unwrap();
        if enhanced {
            assert_eq!(cancel.value().attr("type"), Some("button"));
            assert!(cancel.value().attr("onclick").is_some());
        } else {
            assert_eq!(cancel.value().attr("value"), Some("cancel"));
            assert!(cancel.value().attr("formnovalidate").is_some());
        }
    }
}

#[test]
fn form_keeps_remote_values_in_escaped_data_and_preserves_the_draft() {
    let form = LinkInput {
        content: "Draft <script>x</script>".into(),
        url: "https://example.com/".into(),
        title: "\" autofocus onfocus=alert(1)".into(),
        action: "".into(),
        link_request: "test".into(),
    };
    let markup = link_form(&form, Some("Manual fallback"), None, true).into_string();
    let document = Html::parse_fragment(&markup);
    let content = document
        .select(&Selector::parse("input[name=content]").unwrap())
        .next()
        .unwrap();
    assert_eq!(content.value().attr("value"), Some(form.content.as_str()));
    let title = document
        .select(&Selector::parse("input[name=title]").unwrap())
        .next()
        .unwrap();
    assert_eq!(title.value().attr("value"), Some(form.title.as_str()));
    assert_eq!(title.value().attr("onfocus"), None);
    let syntax = "[x](https://example.com/\"<script>)".to_owned();
    let result = Html::parse_fragment(&link_form(&form, None, Some(&syntax), true).into_string());
    assert!(
        result
            .select(&Selector::parse("script").unwrap())
            .next()
            .is_none()
    );
    assert_eq!(
        result
            .select(&Selector::parse("[data-link-syntax]").unwrap())
            .next()
            .unwrap()
            .value()
            .attr("data-link-syntax"),
        Some(syntax.as_str())
    );
}
