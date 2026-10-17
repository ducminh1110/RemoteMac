//! Host / Origin / token checks.

mod common;
use axum::body::Body;
use axum::extract::Request;
use common::*;

fn req(
    method: &str,
    path: &str,
    host: Option<&str>,
    origin: Option<&str>,
    token: Option<&str>,
) -> Request {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(h) = host {
        b = b.header("host", h);
    }
    if let Some(o) = origin {
        b = b.header("origin", o);
    }
    if let Some(t) = token {
        b = b.header("x-setup-token", t);
    }
    let body = if method == "POST" {
        b = b.header("content-type", "application/json");
        Body::from(r##"{"color":"#123456"}"##)
    } else {
        Body::empty()
    };
    b.body(body).unwrap()
}

#[tokio::test]
async fn guard_accepts_good_and_rejects_bad_requests() {
    let w = wizard("guard").await;
    let host = format!("127.0.0.1:{WIZ_PORT}");
    let origin = format!("http://127.0.0.1:{WIZ_PORT}");

    // Good
    let (s, _) = w
        .raw(req("GET", "/api/state", Some(&host), None, Some(ACCESS)))
        .await;
    assert_eq!(s, 200);
    let (s, _) = w
        .raw(req(
            "POST",
            "/api/branding/tokens",
            Some(&host),
            Some(&origin),
            Some(ACCESS),
        ))
        .await;
    assert_eq!(s, 200);
    let (s, page) = w
        .raw(req(
            "GET",
            &format!("/?t={ACCESS}"),
            Some(&host),
            None,
            None,
        ))
        .await;
    assert_eq!(s, 200);
    assert!(page.contains("Taro Enterprise Setup"));
    assert!(!page.contains(ACCESS), "the page must not embed the token");
    let (s, _) = w
        .raw(req("GET", "/assets/app.js", Some(&host), None, None))
        .await;
    assert_eq!(s, 200);
    // the shared console look, embedded
    for f in ["console.css", "console.js"] {
        let (s, body) = w
            .raw(req("GET", &format!("/common/{f}"), Some(&host), None, None))
            .await;
        assert_eq!(s, 200, "{f}");
        assert!(body.len() > 100, "{f}");
    }
    let (s, _) = w
        .raw(req("GET", "/common/../Cargo.toml", Some(&host), None, None))
        .await;
    assert_eq!(s, 404);
    let (s, _) = w
        .raw(req(
            "GET",
            "/common/console.js",
            Some("evil.example:1"),
            None,
            None,
        ))
        .await;
    assert_eq!(s, 421);

    // DNS rebinding: wrong Host values.
    for bad_host in [
        "evil.example.com".to_string(),
        format!("evil.example.com:{WIZ_PORT}"),
        format!("localhost:{WIZ_PORT}"),
        "127.0.0.1:1".to_string(),
        "127.0.0.1".to_string(),
    ] {
        let (s, _) = w
            .raw(req(
                "GET",
                "/api/state",
                Some(&bad_host),
                None,
                Some(ACCESS),
            ))
            .await;
        assert_eq!(s, 421, "{bad_host}");
        let (s, _) = w
            .raw(req(
                "GET",
                &format!("/?t={ACCESS}"),
                Some(&bad_host),
                None,
                None,
            ))
            .await;
        assert_eq!(s, 421, "{bad_host}");
    }
    let (s, _) = w
        .raw(req("GET", "/api/state", None, None, Some(ACCESS)))
        .await;
    assert_eq!(s, 421);

    // Token
    let (s, _) = w
        .raw(req("GET", "/api/state", Some(&host), None, None))
        .await;
    assert_eq!(s, 401);
    let (s, _) = w
        .raw(req("GET", "/api/state", Some(&host), None, Some("wrong")))
        .await;
    assert_eq!(s, 401);
    let mut almost = ACCESS.to_string();
    almost.pop();
    almost.push('0');
    let (s, _) = w
        .raw(req("GET", "/api/state", Some(&host), None, Some(&almost)))
        .await;
    assert_eq!(s, 401);
    let (s, _) = w.raw(req("GET", "/", Some(&host), None, None)).await;
    assert_eq!(s, 403);
    let (s, _) = w.raw(req("GET", "/?t=nope", Some(&host), None, None)).await;
    assert_eq!(s, 403);
    let (s, _) = w
        .raw(req("GET", "/api/diagnostics.txt", Some(&host), None, None))
        .await;
    assert_eq!(s, 401);

    // Origin
    for bad_origin in [
        "http://evil.example.com",
        "http://localhost:45999",
        "null",
        "http://127.0.0.1:1",
    ] {
        let (s, _) = w
            .raw(req(
                "POST",
                "/api/branding/tokens",
                Some(&host),
                Some(bad_origin),
                Some(ACCESS),
            ))
            .await;
        assert_eq!(s, 403, "{bad_origin}");
        let (s, _) = w
            .raw(req(
                "GET",
                "/api/state",
                Some(&host),
                Some(bad_origin),
                Some(ACCESS),
            ))
            .await;
        assert_eq!(s, 403, "{bad_origin}");
    }
    // Non-GET API calls must carry an Origin.
    let (s, _) = w
        .raw(req(
            "POST",
            "/api/branding/tokens",
            Some(&host),
            None,
            Some(ACCESS),
        ))
        .await;
    assert_eq!(s, 403);

    // Sec-Fetch-Site cross-site
    let r = Request::builder()
        .method("GET")
        .uri("/api/state")
        .header("host", &host)
        .header("x-setup-token", ACCESS)
        .header("sec-fetch-site", "cross-site")
        .body(Body::empty())
        .unwrap();
    let (s, _) = w.raw(r).await;
    assert_eq!(s, 403);

    // The reveal endpoint is guarded like every other API call.
    let (s, _) = w
        .raw(req(
            "POST",
            "/api/relay/key/reveal",
            Some(&host),
            Some("http://evil.example.com"),
            Some(ACCESS),
        ))
        .await;
    assert_eq!(s, 403);
    let (s, _) = w
        .raw(req(
            "POST",
            "/api/relay/key/reveal",
            Some(&host),
            Some(&origin),
            None,
        ))
        .await;
    assert_eq!(s, 401);
    let (s, _) = w
        .raw(req(
            "GET",
            "/api/branding/logo/image",
            Some(&host),
            None,
            None,
        ))
        .await;
    assert_eq!(s, 401);

    // Rejected responses never echo the token.
    for body in w.responses.lock().unwrap().iter() {
        assert!(!body.contains(ACCESS), "token echoed");
    }
}

#[tokio::test]
async fn security_headers_present() {
    let w = wizard("guard").await;
    let host = format!("127.0.0.1:{WIZ_PORT}");
    let resp = tower::ServiceExt::oneshot(
        w.router.clone(),
        req("GET", &format!("/?t={ACCESS}"), Some(&host), None, None),
    )
    .await
    .unwrap();
    let h = resp.headers();
    assert!(h["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("script-src 'self'"));
    assert_eq!(h["referrer-policy"], "no-referrer");
    assert_eq!(h["x-frame-options"], "DENY");
    assert_eq!(h["cache-control"], "no-store");
}
