//! Steps 2-5 through the API: rm-brand's validation per field, the colour rules (the server
//! enforces them whatever the UI does), images, the relay key, importing a provider.json.

mod common;
use common::*;
use serde_json::json;

#[tokio::test]
async fn colours_are_validated_by_the_server() {
    let w = wizard("colours").await;
    for (input, want) in [
        ("#7C3AED", "#7C3AED"),
        ("7c3aed", "#7C3AED"),
        ("#7c3AeD", "#7C3AED"),
        ("  #0f766e ", "#0F766E"),
        ("3D7A5B", "#3D7A5B"),
    ] {
        let (s, v) = w
            .post("/api/branding/tokens", json!({ "color": input }))
            .await;
        assert_eq!(s, 200, "{input}: {v}");
        assert_eq!(v["color"], want);
        let t = rm_brand::Tokens::derive(rm_brand::Color::parse(want).unwrap());
        assert_eq!(v["tokens"], serde_json::to_value(&t).unwrap(), "{input}");
        assert!(v["contrast"]["on_primary"].as_f64().unwrap() >= 1.0);
    }
    for bad in [
        "",
        "#",
        "#fff",
        "fff",
        "#12345",
        "#1234567",
        "#12345G",
        "blue",
        "##123456",
        "rgb(1,2,3)",
    ] {
        let (s, v) = w
            .post("/api/branding/tokens", json!({ "color": bad }))
            .await;
        assert_eq!(s, 400, "{bad:?} must be refused: {v}");
        assert!(v["fields"]["primary_color"].is_string(), "{v}");
        let (s, _) = w
            .post(
                "/api/branding",
                json!({ "primary_color": bad, "theme": "light" }),
            )
            .await;
        assert_eq!(s, 400, "{bad:?}");
    }
    // nothing invalid was stored; the default is Taro's own colour, not a preset
    let st = w.state().await;
    assert_eq!(st["branding"]["primary_color"], "#7C4AE8");
    assert_eq!(st["branding"]["saved"], false);
    // a custom colour is stored exactly (normalized), with its derived tokens in the view
    let (s, v) = w
        .post(
            "/api/branding",
            json!({ "primary_color": "#3d7a5b", "theme": "dark" }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let st = &v["state"];
    assert_eq!(st["branding"]["primary_color"], "#3D7A5B");
    assert_eq!(st["branding"]["theme"], "dark");
    assert_eq!(st["tokens"]["primary"], "#3D7A5B");
    assert_eq!(st["steps"][2]["status"], "ok");
    // a pale colour is accepted with a warning (contrast is advice, not a rule)
    let (s, v) = w
        .post(
            "/api/branding",
            json!({ "primary_color": "#FDE68A", "theme": "light" }),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        v["state"]["contrast"]["on_primary"].as_f64().unwrap() >= 4.5,
        v["state"]["contrast"]["light_ok"].as_bool().unwrap()
    );
    let (s, _) = w
        .post(
            "/api/branding",
            json!({ "primary_color": "#FFFF00", "theme": "neon" }),
        )
        .await;
    assert_eq!(s, 400);
}

#[tokio::test]
async fn identity_fields_are_checked_by_rm_brand() {
    let w = wizard("identity").await;
    let (s, v) = w
        .post(
            "/api/identity",
            json!({"provider_id": "Acme Cloud", "name": "", "short_name": "a/b", "vendor": "Acme", "support_url": "http://acme.example", "support_email": "nope"}),
        )
        .await;
    assert_eq!(s, 400);
    for f in [
        "provider_id",
        "product.name",
        "product.short_name",
        "product.support_url",
        "product.support_email",
    ] {
        assert!(v["fields"][f].is_string(), "{f} not reported: {v}");
    }
    assert!(v["fields"]["product.vendor"].is_null());
    let (s, v) = w
        .post(
            "/api/identity",
            json!({"provider_id": "acme", "name": "Acme", "short_name": "CON", "vendor": "Acme"}),
        )
        .await;
    assert_eq!(s, 400);
    assert!(v["fields"]["product.short_name"]
        .as_str()
        .unwrap()
        .contains("Windows"));
    let (s, v) = w
        .post(
            "/api/identity",
            json!({"provider_id": " acme ", "name": " Acme Connect ", "short_name": "Acme", "vendor": "Acme Ltd", "support_url": "", "support_email": ""}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["state"]["identity"]["provider_id"], "acme");
    assert_eq!(v["state"]["identity"]["name"], "Acme Connect");
    assert!(v["state"]["identity"]["support_url"].is_null());
}

#[tokio::test]
async fn images_are_png_only_and_the_icon_square() {
    let w = wizard("images").await;
    let fx = |w, h| taro_enterprise_setup::branding::fixtures::png(w, h);
    let (s, v) = w
        .upload(
            "logo",
            taro_enterprise_setup::branding::fixtures::jpeg(64, 64),
        )
        .await;
    assert_eq!(s, 400);
    assert!(v["error"].as_str().unwrap().contains("PNG only"), "{v}");
    let (s, v) = w.upload("logo", b"<svg onload=alert(1)/>".to_vec()).await;
    assert_eq!(s, 400);
    assert!(v["error"].as_str().unwrap().contains("SVG"), "{v}");
    let (s, v) = w.upload("icon", fx(256, 128)).await;
    assert_eq!(s, 400);
    assert!(v["error"].as_str().unwrap().contains("square"), "{v}");
    assert_eq!(w.upload("favicon", fx(32, 32)).await.0, 400);
    let logo = fx(300, 100);
    assert_eq!(w.upload("logo", logo.clone()).await.0, 200);
    let st = w.state().await;
    assert_eq!(st["branding"]["logo"]["info"]["width"], 300);
    assert_eq!(
        st["branding"]["logo"]["sha256"],
        taro_update::sha256_hex(&logo)
    );
    let (s, img) = w
        .raw(
            axum::extract::Request::builder()
                .uri("/api/branding/logo/image")
                .header("host", format!("127.0.0.1:{WIZ_PORT}"))
                .header("x-setup-token", ACCESS)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(s, 200);
    assert!(img.as_bytes().starts_with(b"\x89PNG") || img.contains("PNG"));
    assert_eq!(w.call("DELETE", "/api/branding/logo", None).await.0, 200);
    assert!(w.state().await["branding"]["logo"].is_null());
}

#[tokio::test]
async fn the_relay_key_is_generated_entered_and_never_shown_unasked() {
    let w = wizard("relay").await;
    // no key yet
    let (s, v) = w
        .post(
            "/api/relay",
            json!({"host": "relay.acme.example", "port": 7470, "client_updates": "notify"}),
        )
        .await;
    assert_eq!(s, 400);
    assert!(v["fields"]["relay.key"].is_string(), "{v}");
    for (host, port) in [
        ("not a host", json!(7470)),
        ("relay.acme.example", json!(0)),
        ("relay.acme.example", json!(70000)),
        ("relay.acme.example", json!("x")),
    ] {
        let (s, v) = w
            .post("/api/relay", json!({"host": host, "port": port, "key": "0123456789abcdef-_XYZ", "client_updates": "off"}))
            .await;
        assert_eq!(s, 400, "{host} {port}");
        assert!(
            v["fields"]["relay.host"].is_string() || v["fields"]["relay.port"].is_string(),
            "{v}"
        );
    }
    let (s, v) = w
        .post("/api/relay", json!({"host": "relay.acme.example", "port": 7470, "key": "short", "client_updates": "notify"}))
        .await;
    assert_eq!(s, 400);
    assert!(v["fields"]["relay.key"].is_string());
    // an existing key (a rebuild keeps its key)
    let mine = "existing-relay-key_0123456789";
    let (s, v) = w
        .post("/api/relay", json!({"host": "Relay.Acme.Example.", "port": "7471", "key": mine, "client_updates": "off"}))
        .await;
    assert_eq!(s, 200, "{v}");
    let st = &v["state"]["relay"];
    assert_eq!(st["host"], "relay.acme.example");
    assert_eq!(st["port"], 7471);
    assert_eq!(st["key_source"], "entered");
    assert_eq!(st["client_updates"], "off");
    assert!(!v.to_string().contains(mine));
    assert_eq!(w.relay_key().await, mine);
    // generate a new one
    let (s, v) = w.post("/api/relay/key/generate", json!({})).await;
    assert_eq!(s, 200);
    assert_eq!(v["state"]["relay"]["key_source"], "generated");
    let k = w.relay_key().await;
    assert_ne!(k, mine);
    assert_eq!(k.len(), 32);
    assert!(k
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    // saving without a key keeps it
    let (s, _) = w
        .post(
            "/api/relay",
            json!({"host": "relay.acme.example", "port": 7470, "client_updates": "notify"}),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(w.relay_key().await, k);
    // the key never appears in the log or another response
    let (_, diag) = w.call("GET", "/api/diagnostics.txt", None).await;
    assert!(!diag.as_str().unwrap().contains(&k));
    assert!(!diag.as_str().unwrap().contains(mine));
    let n = w
        .responses
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.contains(&k))
        .count();
    assert_eq!(n, 2, "only the two reveal responses carry it");
}

#[tokio::test]
async fn an_existing_provider_json_can_be_imported() {
    let w = wizard("import").await;
    let p = rm_brand::Provider {
        schema: rm_brand::SCHEMA.into(),
        provider_id: "globex".into(),
        product: rm_brand::Product {
            name: "Globex Remote".into(),
            short_name: "Globex".into(),
            vendor: "Globex Corp".into(),
            support_url: None,
            support_email: Some("it@globex.example".into()),
        },
        branding: rm_brand::Branding {
            primary_color: "#0d9488".into(),
            tokens: None,
            theme: rm_brand::Theme::Light,
            logo: Some("logo.png".into()),
            icon: None,
        },
        relay: rm_brand::Relay {
            host: "relay.globex.example".into(),
            port: 443,
            key: "globex-key-0123456789abcdef".into(),
        },
        account: None,
        updates: rm_brand::Updates {
            clients: rm_brand::ClientUpdates::Off,
        },
        generated: None,
    }
    .normalized();
    let (s, v) = w
        .post("/api/provider/import", json!({"json": "{ not json"}))
        .await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = w
        .post("/api/provider/import", json!({"json": p.to_json()}))
        .await;
    assert_eq!(s, 200, "{v}");
    assert!(v["message"].as_str().unwrap().contains("logo.png"));
    let st = &v["state"];
    assert_eq!(st["identity"]["provider_id"], "globex");
    assert_eq!(st["branding"]["primary_color"], "#0D9488");
    assert_eq!(st["branding"]["theme"], "light");
    assert_eq!(st["relay"]["port"], 443);
    assert_eq!(st["relay"]["client_updates"], "off");
    assert_eq!(st["relay"]["key_source"], "imported");
    assert!(!v.to_string().contains("globex-key-0123456789abcdef"));
    assert_eq!(w.relay_key().await, "globex-key-0123456789abcdef");
    for i in 1..=3 {
        assert_eq!(st["steps"][i]["status"], "ok", "{}", st["steps"][i]);
    }
}

#[tokio::test]
async fn steps_refuse_to_run_out_of_order() {
    let w = wizard("order").await;
    let (s, v) = w.build().await;
    assert_eq!(s, 400);
    assert!(
        v["error"].as_str().unwrap().contains("Enterprise release"),
        "{v}"
    );
    let (s, _) = w.select("enterprise-v0.2.0").await;
    assert_eq!(s, 400, "releases must be discovered first");
    assert_eq!(w.post("/api/validate/run", json!({})).await.0, 400);
    assert_eq!(w.post("/api/output/open", json!({})).await.0, 400);
    assert_eq!(w.post("/api/build/cancel", json!({})).await.0, 400);
    // with a release but no identity
    assert_eq!(w.post("/api/components/discover", json!({})).await.0, 200);
    assert_eq!(w.select("enterprise-v0.2.0").await.0, 200);
    let (s, v) = w.build().await;
    assert_eq!(s, 400);
    assert!(v["error"].as_str().unwrap().contains("identity"), "{v}");
    let (s, v) = w
        .post("/api/build/start", json!({"output_dir": "relative/folder"}))
        .await;
    assert_eq!(s, 400);
    assert!(v["error"].as_str().unwrap().contains("full path"), "{v}");
    let _ = std::fs::remove_dir_all(&w.out);
}
