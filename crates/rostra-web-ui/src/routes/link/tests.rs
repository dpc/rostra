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
        "2002:7f00:1::",
        "3fff::1",
    ] {
        assert!(!public_ip(ip.parse().unwrap()), "{ip}");
    }
    for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
        assert!(public_ip(ip.parse().unwrap()), "{ip}");
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
    ] {
        assert!(link_syntax(&Url::parse(url).unwrap(), "Video").starts_with("![Video]("));
    }
    for url in [
        "https://example.com/",
        "https://youtube.com/",
        "https://youtube.com.evil.test/watch?v=123",
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
