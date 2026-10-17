//! Shared test harness: fake official Taro packages built in the test, a component manifest
//! signed with a test key, a local fake of the GitHub releases API and of the asset hosts
//! (github.com redirecting to objects.githubusercontent.com), and a client that drives the
//! wizard's router in-process.
#![allow(dead_code)]

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use taro_enterprise_setup::config::{Endpoints, Timings};
use taro_enterprise_setup::state::{AppState, Options};
use taro_update::manifest::{Component, Components, COMPONENTS_SCHEMA};
use taro_update::sig::{self, TrustedKeys};
use tower::ServiceExt;

pub const REPO: &str = "ducminh1110/RemoteMac";
pub const TARO: &str = "1.4.1";
pub const ENT: &str = "0.2.0";
pub const RELAY: &str = "0.2.0";

/// The bytes of the fake official programs (checked to be unchanged in every package).
pub const TARO_EXE: &[u8] = b"MZ\x90\x00fake Taro.exe for Windows\x00\x01\x02\xff";
pub const MAC_BIN: &[u8] = b"\xcf\xfa\xed\xfefake universal taro, ad-hoc signed\x00\x07";
pub const TARO_SH: &[u8] = b"#!/bin/bash\necho taro.sh\n";
pub const TARO_UPDATE_SH: &[u8] = b"#!/bin/bash\necho taro-update.sh\n";
pub const RELAY_BIN: &[u8] = b"\x7fELFfake taro-relay\x00\x01";
pub const INSTALL_SH: &[u8] = b"#!/bin/sh\necho install the relay\n";

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (n, b) in entries {
        if n.ends_with('/') {
            z.add_directory(*n, o).unwrap();
        } else {
            z.start_file(*n, o).unwrap();
            z.write_all(b).unwrap();
        }
    }
    z.finish().unwrap().into_inner()
}

/// `Taro-windows.zip`: with a `Taro/` folder, or flat (as `Compress-Archive dist\Taro\*` makes it).
pub fn windows_zip(flat: bool) -> Vec<u8> {
    let p = if flat { "" } else { "Taro/" };
    let exe = format!("{p}Taro.exe");
    let readme = format!("{p}README.txt");
    let lic = format!("{p}licenses/THIRD-PARTY.txt");
    let mut v: Vec<(&str, &[u8])> = vec![];
    if !flat {
        v.push(("Taro/", b""));
    }
    v.push((&exe, TARO_EXE));
    v.push((&readme, b"Taro for Windows\r\n"));
    v.push((&lic, b"third-party licences\n"));
    zip_bytes(&v)
}

/// (path, bytes or None for a folder, mode, symlink target)
type TarItem<'a> = (&'a str, Option<&'a [u8]>, u32, Option<&'a str>);

fn tgz(items: &[TarItem<'_>]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut b = tar::Builder::new(gz);
    for (path, data, mode, link) in items {
        let mut h = tar::Header::new_gnu();
        h.set_mode(*mode);
        h.set_mtime(1_790_000_000);
        if let Some(t) = link {
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            b.append_link(&mut h, path, t).unwrap();
        } else if let Some(d) = data {
            h.set_entry_type(tar::EntryType::Regular);
            h.set_size(d.len() as u64);
            b.append_data(&mut h, path, *d).unwrap();
        } else {
            h.set_entry_type(tar::EntryType::Directory);
            h.set_size(0);
            b.append_data(&mut h, path, std::io::empty()).unwrap();
        }
    }
    b.into_inner().unwrap().finish().unwrap()
}

pub fn macos_tgz() -> Vec<u8> {
    tgz(&[
        ("Taro/", None, 0o755, None),
        ("Taro/taro", Some(MAC_BIN), 0o755, None),
        ("Taro/taro.sh", Some(TARO_SH), 0o755, None),
        ("Taro/taro-update.sh", Some(TARO_UPDATE_SH), 0o755, None),
        ("Taro/README.txt", Some(b"Taro for macOS\n"), 0o644, None),
        ("Taro/._taro", Some(b"AppleDouble junk"), 0o644, None),
    ])
}

pub fn relay_top(arch: &str) -> String {
    format!("taro-relay-{RELAY}-linux-{arch}")
}

/// The generic relay package as `enterprise/scripts/package-relay.sh` makes it (plus a symbolic
/// link, to exercise links).
pub fn relay_tgz(arch: &str) -> Vec<u8> {
    let t = relay_top(arch);
    let bin = [RELAY_BIN, arch.as_bytes()].concat();
    let arch_txt = format!("{arch}\n");
    let ver_txt = format!("{RELAY}\n");
    #[allow(clippy::type_complexity)]
    let items: Vec<(String, Option<Vec<u8>>, u32, Option<&str>)> = vec![
        (format!("{t}/"), None, 0o755, None),
        (format!("{t}/ARCH"), Some(arch_txt.into_bytes()), 0o644, None),
        (format!("{t}/README.md"), Some(b"# taro-relay\n".to_vec()), 0o644, None),
        (format!("{t}/VERSION"), Some(ver_txt.into_bytes()), 0o644, None),
        (format!("{t}/core/"), None, 0o755, None),
        (format!("{t}/core/bin/"), None, 0o755, None),
        (format!("{t}/core/bin/relay"), None, 0o777, Some("taro-relay")),
        (format!("{t}/core/bin/taro-relay"), Some(bin), 0o755, None),
        (format!("{t}/core/share/"), None, 0o755, None),
        (format!("{t}/core/share/LICENSE"), Some(b"GPL-3.0-or-later\n".to_vec()), 0o644, None),
        (format!("{t}/core/share/NOTICE.md"), Some(b"# Notice\n".to_vec()), 0o644, None),
        (format!("{t}/core/web/"), None, 0o755, None),
        (format!("{t}/core/web/relay/"), None, 0o755, None),
        (format!("{t}/core/web/relay/setup/"), None, 0o755, None),
        (format!("{t}/core/web/relay/setup/index.html"), Some(b"<main>setup</main>\n".to_vec()), 0o644, None),
        (format!("{t}/core/web/relay/status/"), None, 0o755, None),
        (format!("{t}/core/web/relay/status/index.html"), Some(b"<main>status</main>\n".to_vec()), 0o644, None),
        (format!("{t}/install.sh"), Some(INSTALL_SH.to_vec()), 0o755, None),
        (format!("{t}/provider/"), None, 0o755, None),
        (format!("{t}/provider/README.txt"), Some(b"The Taro Enterprise package builder (Wizard 1) puts provider.json, logo.png and icon.png here.\n".to_vec()), 0o644, None),
    ];
    let refs: Vec<TarItem<'_>> = items
        .iter()
        .map(|(p, d, m, l)| (p.as_str(), d.as_deref(), *m, *l))
        .collect();
    tgz(&refs)
}

/// One repository of the fake GitHub (Step 5).
pub struct GhRepo {
    /// the X25519 key Actions secrets are sealed for (the test opens them with it)
    pub secrets_key: crypto_box::SecretKey,
    pub secrets: HashMap<String, String>,
    /// path -> (text, blob sha)
    pub files: HashMap<String, (String, String)>,
    pub secret_puts: u32,
    pub file_puts: u32,
    pub dispatches: Vec<Value>,
    /// workflow runs; each look advances the newest: queued -> in_progress -> completed
    pub runs: Vec<Value>,
    /// releases published by the fake Actions
    pub releases: Vec<Value>,
    pub parent: Option<String>,
    /// GitHub Actions allowed to run (off in a new fork)
    pub actions: bool,
    pub syncs: u32,
    /// each change of a secret (by Start or "by hand" in a test); GitHub's updated_at follows it
    pub secret_changes: u32,
    /// GitHub refuses secrets (an organization policy, a token without Secrets access)
    pub refuse_secrets: bool,
    /// default_workflow_permissions set by Start
    pub workflow_permissions: Option<String>,
}

impl GhRepo {
    fn new(
        files: HashMap<String, (String, String)>,
        parent: Option<String>,
        actions: bool,
    ) -> Self {
        GhRepo {
            secrets_key: crypto_box::SecretKey::generate(&mut crypto_box::aead::OsRng),
            secrets: HashMap::new(),
            files,
            secret_puts: 0,
            file_puts: 0,
            dispatches: vec![],
            runs: vec![],
            releases: vec![],
            parent,
            actions,
            syncs: 0,
            secret_changes: 0,
            refuse_secrets: false,
            workflow_permissions: None,
        }
    }
}

/// The fake GitHub API: repositories by `owner/name`.
pub struct Gh {
    pub repos: HashMap<String, GhRepo>,
    /// how the fake release workflow ends
    pub run_conclusion: String,
    /// every request body the fake GitHub API received
    pub bodies: Vec<String>,
    pub forks: u32,
}

impl Gh {
    pub fn repo(&mut self, full: &str) -> &mut GhRepo {
        self.repos
            .get_mut(full)
            .unwrap_or_else(|| panic!("no fake repository {full}"))
    }
}

/// A classic token with repo and workflow, and one without workflow.
pub const GH_TOKEN: &str = "github-token-for-tests-0123456789";
pub const GH_TOKEN_NO_WORKFLOW: &str = "github-token-noworkflow-0123456789";
pub const KEYS_PATH: &str = "enterprise/keys/release-signing.pub";
pub const OLD_KEY: &str = "ed25519:VBCM7iMltXcKPW7U05JsFlpOzPdy1bO/BPWJmWR1kio=";
/// the operator's own copy, made by Start
pub const FORK: &str = "octo/RemoteMac";

pub struct Fake {
    pub gh: Mutex<Gh>,
    pub base: Mutex<String>,
    /// served files by URL path below the fake (`/objects/…`, `/gh-web/…`)
    pub files: Mutex<HashMap<String, Vec<u8>>>,
    pub requests: Mutex<Vec<String>>,
    /// serve a modified Taro-windows.zip (size kept, contents changed)
    pub tamper: AtomicBool,
    /// redirect component downloads to a host outside the policy
    pub evil_redirect: AtomicBool,
    /// stream downloads slowly (for cancel)
    pub slow: AtomicBool,
    pub downloads: AtomicU32,
}

impl Fake {
    pub fn base(&self) -> String {
        self.base.lock().unwrap().clone()
    }
    pub fn reqs(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

type F = Arc<Fake>;

async fn record(State(f): State<F>, req: Request, next: axum::middleware::Next) -> Response {
    f.requests.lock().unwrap().push(
        req.uri()
            .path_and_query()
            .map(|p| p.to_string())
            .unwrap_or_default(),
    );
    next.run(req).await
}

fn upstream_releases() -> Vec<Value> {
    let asset = |tag: &str, n: &str| json!({"name": n, "size": 1, "browser_download_url": format!("https://github.com/{REPO}/releases/download/{tag}/{n}")});
    let rel = |tag: &str, draft: bool| {
        json!({
            "tag_name": tag, "name": format!("Taro Enterprise {tag}"), "draft": draft, "prerelease": false,
            "published_at": "2026-10-16T10:00:00Z", "html_url": format!("https://github.com/{REPO}/releases/tag/{tag}"),
            "assets": [asset(tag, "taro-enterprise-components.json"), asset(tag, "taro-enterprise-components.json.sig"), asset(tag, "enterprise-update.zip")]
        })
    };
    vec![
        json!({"tag_name": TARO, "draft": false, "assets": [asset(TARO, "Taro-windows.zip")]}),
        rel("enterprise-v0.1.0", false),
        rel("enterprise-v0.2.0", false),
        rel("enterprise-v0.9.0", true),
    ]
}

async fn releases(State(f): State<F>, Path((o, r)): Path<(String, String)>) -> Response {
    let full = format!("{o}/{r}");
    let g = f.gh.lock().unwrap();
    let Some(repo) = g.repos.get(&full) else {
        return not_found();
    };
    let mut v = if full == REPO {
        upstream_releases()
    } else {
        vec![]
    };
    v.extend(repo.releases.iter().cloned());
    Json(Value::Array(v)).into_response()
}

async fn serve(f: &Fake, key: &str) -> Response {
    let Some(mut bytes) = f.files.lock().unwrap().get(key).cloned() else {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    if key.ends_with("/Taro-windows.zip") && f.tamper.load(Ordering::SeqCst) {
        let n = bytes.len();
        bytes[n / 2] ^= 0x55;
    }
    f.downloads.fetch_add(1, Ordering::SeqCst);
    if f.slow.load(Ordering::SeqCst) {
        let len = bytes.len();
        let stream = futures_util::stream::unfold((bytes, 0usize), |(b, pos)| async move {
            if pos >= b.len() {
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let end = (pos + 64).min(b.len());
            let chunk = b[pos..end].to_vec();
            Some((Ok::<_, std::io::Error>(chunk), (b, end)))
        });
        let mut r = Response::new(Body::from_stream(stream));
        r.headers_mut()
            .insert("content-length", len.to_string().parse().unwrap());
        return r;
    }
    bytes.into_response()
}

async fn gh_web(State(f): State<F>, Path(rest): Path<String>) -> Response {
    let key = format!("/gh-web/{rest}");
    if f.files.lock().unwrap().contains_key(&key) {
        return serve(&f, &key).await;
    }
    // component downloads: redirect like GitHub does
    let name = rest.rsplit('/').next().unwrap_or_default();
    let to = if f.evil_redirect.load(Ordering::SeqCst) {
        format!("https://evil.example.com/{name}")
    } else {
        format!("https://objects.githubusercontent.com/assets/{name}?sig=abc")
    };
    (StatusCode::FOUND, [("location", to)]).into_response()
}

async fn objects(State(f): State<F>, Path(rest): Path<String>) -> Response {
    serve(&f, &format!("/objects/{rest}")).await
}

pub struct Release {
    pub key: ed25519_dalek::SigningKey,
    pub sources: HashMap<String, Vec<u8>>,
}

fn comp(id: &str, version: &str, file: &str, url: &str, bytes: &[u8]) -> Component {
    Component {
        id: id.into(),
        version: version.into(),
        file: file.into(),
        url: url.into(),
        sha256: taro_update::sha256_hex(bytes),
        size: bytes.len() as u64,
    }
}

/// The fake world: packages, two signed component manifests (0.2.0 is good; 0.1.0 lists a
/// Windows client on a host outside the download policy), the server.
pub async fn start_fake(key: &ed25519_dalek::SigningKey) -> (Arc<Fake>, HashMap<String, Vec<u8>>) {
    let mut gh_files = HashMap::new();
    gh_files.insert(
        KEYS_PATH.to_string(),
        (
            format!("# Taro Enterprise release signing keys (ed25519).\n{OLD_KEY}\n"),
            "sha-keys-0".to_string(),
        ),
    );
    gh_files.insert(
        "enterprise/Cargo.toml".to_string(),
        (
            "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.3.0\"\n".to_string(),
            "sha-cargo-0".to_string(),
        ),
    );
    gh_files.insert(
        ".github/workflows/enterprise-release.yml".to_string(),
        (
            "name: Enterprise release\n".to_string(),
            "sha-wf-0".to_string(),
        ),
    );
    let mut repos = HashMap::new();
    repos.insert(REPO.to_string(), GhRepo::new(gh_files, None, true));
    let f = Arc::new(Fake {
        gh: Mutex::new(Gh {
            repos,
            run_conclusion: "success".into(),
            bodies: vec![],
            forks: 0,
        }),
        base: Mutex::new(String::new()),
        files: Mutex::new(HashMap::new()),
        requests: Mutex::new(vec![]),
        tamper: AtomicBool::new(false),
        evil_redirect: AtomicBool::new(false),
        slow: AtomicBool::new(false),
        downloads: AtomicU32::new(0),
    });
    let mut sources = HashMap::new();
    sources.insert("Taro-windows.zip".to_string(), windows_zip(false));
    sources.insert("Taro-macos.tar.gz".to_string(), macos_tgz());
    for a in ["x86_64", "aarch64"] {
        sources.insert(format!("{}.tar.gz", relay_top(a)), relay_tgz(a));
    }
    let dl =
        |tag: &str, file: &str| format!("https://github.com/{REPO}/releases/download/{tag}/{file}");
    let comps = |win_url: String| {
        let mut v = vec![
            comp(
                "windows-client",
                TARO,
                "Taro-windows.zip",
                &win_url,
                &sources["Taro-windows.zip"],
            ),
            comp(
                "macos-client",
                TARO,
                "Taro-macos.tar.gz",
                &dl(TARO, "Taro-macos.tar.gz"),
                &sources["Taro-macos.tar.gz"],
            ),
        ];
        for a in ["x86_64", "aarch64"] {
            let file = format!("{}.tar.gz", relay_top(a));
            v.push(comp(
                &format!("linux-relay-{a}"),
                RELAY,
                &file,
                &dl("enterprise-v0.2.0", &file),
                &sources[&file],
            ));
        }
        v
    };
    let manifest = |ver: &str, win_url: String| Components {
        schema: COMPONENTS_SCHEMA.into(),
        enterprise_version: ver.into(),
        taro_version: TARO.into(),
        released_at: "2026-10-16T10:00:00Z".into(),
        components: comps(win_url),
    };
    {
        let mut files = f.files.lock().unwrap();
        for (tag, ver, win) in [
            ("enterprise-v0.2.0", ENT, dl(TARO, "Taro-windows.zip")),
            (
                "enterprise-v0.1.0",
                "0.1.0",
                "https://evil.example.com/Taro-windows.zip".to_string(),
            ),
        ] {
            let m = serde_json::to_vec_pretty(&manifest(ver, win)).unwrap();
            let s = sig::sign(key, &m);
            files.insert(
                format!("/gh-web/{REPO}/releases/download/{tag}/taro-enterprise-components.json"),
                m,
            );
            files.insert(
                format!(
                    "/gh-web/{REPO}/releases/download/{tag}/taro-enterprise-components.json.sig"
                ),
                s.into_bytes(),
            );
        }
        for (name, bytes) in &sources {
            files.insert(format!("/objects/assets/{name}"), bytes.clone());
        }
    }
    let api = "/gh-api/repos/{o}/{r}";
    let app = Router::new()
        .route("/gh-api/user", get(gh_user))
        .route(api, get(gh_repo))
        .route(&format!("{api}/forks"), axum::routing::post(gh_fork))
        .route(
            &format!("{api}/merge-upstream"),
            axum::routing::post(gh_merge_upstream),
        )
        .route(
            &format!("{api}/actions/permissions"),
            axum::routing::put(gh_actions_on),
        )
        .route(
            &format!("{api}/actions/permissions/workflow"),
            axum::routing::put(gh_workflow_permissions),
        )
        .route(
            &format!("{api}/actions/workflows/{{wf}}/enable"),
            axum::routing::put(gh_workflow_on),
        )
        .route(
            &format!("{api}/actions/secrets/public-key"),
            get(gh_secrets_key),
        )
        .route(
            &format!("{api}/actions/secrets/{{name}}"),
            get(gh_secret_get).put(gh_secret_put),
        )
        .route(
            &format!("{api}/contents/{{*path}}"),
            get(gh_file_get).put(gh_file_put),
        )
        .route(
            &format!("{api}/actions/workflows/{{wf}}/dispatches"),
            axum::routing::post(gh_dispatch),
        )
        .route(
            &format!("{api}/actions/workflows/{{wf}}/runs"),
            get(gh_runs),
        )
        .route(&format!("{api}/releases"), get(releases))
        .route("/gh-web/", get(|| async { "github.com" }))
        .route("/gh-web/{*rest}", get(gh_web))
        .route(
            "/objects/",
            get(|| async { (StatusCode::NOT_FOUND, "objects") }),
        )
        .route("/objects/{*rest}", get(objects))
        .layer(axum::middleware::from_fn_with_state(f.clone(), record))
        .with_state(f.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    *f.base.lock().unwrap() = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (f, sources)
}

type H = axum::http::HeaderMap;
type P2 = Path<(String, String)>;

/// The token of a request, if it is one of the fake's.
fn token(h: &H) -> Option<&'static str> {
    let v = h.get("authorization").and_then(|v| v.to_str().ok())?;
    [GH_TOKEN, GH_TOKEN_NO_WORKFLOW]
        .into_iter()
        .find(|t| v == format!("Bearer {t}"))
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"message": "Bad credentials"})),
    )
        .into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response()
}

macro_rules! authed {
    ($h:expr) => {
        if token(&$h).is_none() {
            return unauthorized();
        }
    };
}

async fn gh_user(h: H) -> Response {
    let Some(t) = token(&h) else {
        return unauthorized();
    };
    let scopes = if t == GH_TOKEN {
        "repo, workflow, read:org"
    } else {
        "repo"
    };
    ([("x-oauth-scopes", scopes)], Json(json!({"login": "octo"}))).into_response()
}

fn repo_json(full: &str, r: &GhRepo) -> Value {
    json!({"full_name": full, "html_url": format!("https://github.com/{full}"), "default_branch": "main",
        "private": false, "fork": r.parent.is_some(),
        "parent": r.parent.as_ref().map(|p| json!({"full_name": p})),
        "permissions": {"admin": true, "push": true}})
}

async fn gh_repo(State(f): State<F>, h: H, Path((o, r)): P2) -> Response {
    authed!(h);
    let full = format!("{o}/{r}");
    match f.gh.lock().unwrap().repos.get(&full) {
        Some(x) => Json(repo_json(&full, x)).into_response(),
        None => not_found(),
    }
}

async fn gh_fork(State(f): State<F>, h: H, Path((o, r)): P2, body: String) -> Response {
    authed!(h);
    let up = format!("{o}/{r}");
    let mut g = f.gh.lock().unwrap();
    g.bodies.push(body);
    // one fork per account: an existing one is returned
    if let Some((full, x)) = g
        .repos
        .iter()
        .find(|(_, x)| x.parent.as_deref() == Some(&up))
    {
        return (StatusCode::ACCEPTED, Json(repo_json(full, x))).into_response();
    }
    let files = g.repo(&up).files.clone();
    let full = format!("octo/{r}");
    g.repos
        .insert(full.clone(), GhRepo::new(files, Some(up), false));
    g.forks += 1;
    let v = repo_json(&full, &g.repos[&full]);
    (StatusCode::ACCEPTED, Json(v)).into_response()
}

async fn gh_merge_upstream(State(f): State<F>, h: H, Path((o, r)): P2) -> Response {
    authed!(h);
    let full = format!("{o}/{r}");
    let mut g = f.gh.lock().unwrap();
    let Some(up) = g.repos.get(&full).and_then(|x| x.parent.clone()) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"message": "not a fork"})),
        )
            .into_response();
    };
    let ups = g.repo(&up).files.clone();
    let me = g.repo(&full);
    me.syncs += 1;
    let mut changed = false;
    for (path, v) in ups {
        // the operator's own commit (the keys file) stays theirs
        if path == KEYS_PATH && me.files.contains_key(&path) {
            continue;
        }
        if me.files.get(&path) != Some(&v) {
            me.files.insert(path, v);
            changed = true;
        }
    }
    let msg = if changed {
        "Successfully fetched and fast-forwarded from upstream ducminh1110:main."
    } else {
        "This branch is not behind the upstream ducminh1110:main."
    };
    Json(json!({"message": msg})).into_response()
}

async fn gh_actions_on(State(f): State<F>, h: H, Path((o, r)): P2, body: String) -> Response {
    authed!(h);
    let mut g = f.gh.lock().unwrap();
    let v: Value = serde_json::from_str(&body).unwrap_or_default();
    g.repo(&format!("{o}/{r}")).actions = v["enabled"] == true;
    StatusCode::NO_CONTENT.into_response()
}

async fn gh_workflow_permissions(
    State(f): State<F>,
    h: H,
    Path((o, r)): P2,
    body: String,
) -> Response {
    authed!(h);
    let v: Value = serde_json::from_str(&body).unwrap_or_default();
    f.gh.lock()
        .unwrap()
        .repo(&format!("{o}/{r}"))
        .workflow_permissions = v["default_workflow_permissions"].as_str().map(String::from);
    StatusCode::NO_CONTENT.into_response()
}

async fn gh_workflow_on(h: H) -> Response {
    authed!(h);
    StatusCode::NO_CONTENT.into_response()
}

async fn gh_secrets_key(State(f): State<F>, h: H, Path((o, r)): P2) -> Response {
    authed!(h);
    let pk =
        f.gh.lock()
            .unwrap()
            .repo(&format!("{o}/{r}"))
            .secrets_key
            .public_key();
    Json(json!({"key_id": "kid-1", "key": taro_enterprise_setup::util::b64(pk.as_bytes())}))
        .into_response()
}

async fn gh_secret_get(
    State(f): State<F>,
    h: H,
    Path((o, r, name)): Path<(String, String, String)>,
) -> Response {
    authed!(h);
    let mut g = f.gh.lock().unwrap();
    let repo = g.repo(&format!("{o}/{r}"));
    if repo.secrets.contains_key(&name) {
        let at = format!("2026-10-17T10:00:{:02}Z", repo.secret_changes);
        Json(json!({"name": name, "updated_at": at})).into_response()
    } else {
        not_found()
    }
}

async fn gh_secret_put(
    State(f): State<F>,
    h: H,
    Path((o, r, name)): Path<(String, String, String)>,
    body: String,
) -> Response {
    authed!(h);
    let mut g = f.gh.lock().unwrap();
    g.bodies.push(body.clone());
    let v: Value = serde_json::from_str(&body).unwrap_or_default();
    assert_eq!(v["key_id"], "kid-1");
    let repo = g.repo(&format!("{o}/{r}"));
    if repo.refuse_secrets {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "Resource not accessible by personal access token"})),
        )
            .into_response();
    }
    repo.secret_changes += 1;
    repo.secrets.insert(
        name,
        v["encrypted_value"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    );
    repo.secret_puts += 1;
    StatusCode::CREATED.into_response()
}

async fn gh_file_get(
    State(f): State<F>,
    h: H,
    Path((o, r, path)): Path<(String, String, String)>,
) -> Response {
    authed!(h);
    let mut g = f.gh.lock().unwrap();
    let Some(repo) = g.repos.get_mut(&format!("{o}/{r}")) else {
        return not_found();
    };
    match repo.files.get(&path) {
        Some((text, sha)) => Json(json!({"content": taro_enterprise_setup::util::b64(text.as_bytes()), "sha": sha, "encoding": "base64"})).into_response(),
        None => not_found(),
    }
}

async fn gh_file_put(
    State(f): State<F>,
    h: H,
    Path((o, r, path)): Path<(String, String, String)>,
    body: String,
) -> Response {
    authed!(h);
    let mut g = f.gh.lock().unwrap();
    g.bodies.push(body.clone());
    let v: Value = serde_json::from_str(&body).unwrap_or_default();
    let repo = g.repo(&format!("{o}/{r}"));
    let current = repo.files.get(&path).map(|x| x.1.clone());
    if v["sha"].as_str().map(str::to_string) != current {
        return (
            StatusCode::CONFLICT,
            Json(json!({"message": "sha does not match"})),
        )
            .into_response();
    }
    let text = String::from_utf8(
        taro_enterprise_setup::util::b64_decode(v["content"].as_str().unwrap_or_default()).unwrap(),
    )
    .unwrap();
    repo.file_puts += 1;
    let sha = format!("sha-{}-{}", path.len(), repo.file_puts);
    repo.files.insert(path, (text, sha));
    Json(json!({"commit": {"sha": "c0ffee0123456789"}})).into_response()
}

async fn gh_dispatch(
    State(f): State<F>,
    h: H,
    Path((o, r, wf)): Path<(String, String, String)>,
    body: String,
) -> Response {
    authed!(h);
    assert_eq!(wf, "enterprise-release.yml");
    let full = format!("{o}/{r}");
    let mut g = f.gh.lock().unwrap();
    g.bodies.push(body.clone());
    let repo = g.repo(&full);
    if !repo.actions {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "Actions are disabled for this repository."})),
        )
            .into_response();
    }
    repo.dispatches
        .push(serde_json::from_str(&body).unwrap_or_default());
    let id = 100 + repo.runs.len() as u64;
    let now = taro_enterprise_setup::util::rfc3339(taro_enterprise_setup::util::now_secs());
    repo.runs.insert(0, json!({"id": id, "status": "queued", "conclusion": null, "html_url": format!("https://github.com/{full}/actions/runs/{id}"), "created_at": now}));
    StatusCode::NO_CONTENT.into_response()
}

/// What the real release workflow checks before it signs: the secret is a signing key whose
/// public half is in the repository's keys file. Returns that key.
fn workflow_key(repo: &GhRepo) -> Option<ed25519_dalek::SigningKey> {
    let sealed = repo.secrets.get("TARO_ENTERPRISE_SIGNING_KEY")?;
    let raw = taro_enterprise_setup::util::b64_decode(sealed)?;
    let text = String::from_utf8(repo.secrets_key.unseal(&raw).ok()?).ok()?;
    let k = sig::parse_secret(text.trim()).ok()?;
    let public = sig::public_text(&k.verifying_key());
    repo.files
        .get(KEYS_PATH)
        .is_some_and(|(t, _)| t.lines().any(|l| l.trim() == public))
        .then_some(k)
}

/// The fake release workflow's result: a release of `full`, its component manifest (the Taro
/// clients of upstream's release, the relays of this one) signed with the repository's secret.
fn publish(f: &Fake, g: &mut Gh, full: &str, version: &str) -> bool {
    let Some(key) = workflow_key(g.repo(full)) else {
        return false;
    };
    let tag = format!("enterprise-v{version}");
    let up = format!(
        "/gh-web/{REPO}/releases/download/enterprise-v0.2.0/taro-enterprise-components.json"
    );
    let mut files = f.files.lock().unwrap();
    let mut m: Components = serde_json::from_slice(&files[&up]).unwrap();
    m.enterprise_version = version.into();
    for c in &mut m.components {
        if c.id.starts_with("linux-relay-") {
            c.url = format!(
                "https://github.com/{full}/releases/download/{tag}/{}",
                c.file
            );
        }
    }
    let bytes = serde_json::to_vec_pretty(&m).unwrap();
    let signature = sig::sign(&key, &bytes);
    let dir = format!("/gh-web/{full}/releases/download/{tag}");
    files.insert(format!("{dir}/taro-enterprise-components.json"), bytes);
    files.insert(
        format!("{dir}/taro-enterprise-components.json.sig"),
        signature.into_bytes(),
    );
    let asset = |n: &str| json!({"name": n, "size": 1, "browser_download_url": format!("https://github.com/{full}/releases/download/{tag}/{n}")});
    g.repo(full).releases.push(json!({
        "tag_name": tag, "name": format!("Taro Enterprise {version}"), "draft": false, "prerelease": true,
        "published_at": "2026-10-17T10:00:00Z", "html_url": format!("https://github.com/{full}/releases/tag/{tag}"),
        "assets": [asset("taro-enterprise-components.json"), asset("taro-enterprise-components.json.sig")]
    }));
    true
}

async fn gh_runs(
    State(f): State<F>,
    h: H,
    Path((o, r, _wf)): Path<(String, String, String)>,
) -> Response {
    authed!(h);
    let full = format!("{o}/{r}");
    let mut g = f.gh.lock().unwrap();
    let conclusion = g.run_conclusion.clone();
    let repo = g.repo(&full);
    let version = repo.dispatches.last().map(|d| {
        d["inputs"]["version"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    });
    let mut finishing = false;
    if let Some(run) = repo.runs.first_mut() {
        match run["status"].as_str() {
            Some("queued") => run["status"] = json!("in_progress"),
            Some("in_progress") => finishing = true,
            _ => {}
        }
    }
    if finishing {
        let ok = conclusion == "success"
            && version
                .as_deref()
                .is_some_and(|v| publish(&f, &mut g, &full, v));
        let run = g.repo(&full).runs.first_mut().unwrap();
        run["status"] = json!("completed");
        run["conclusion"] = json!(if ok { "success" } else { "failure" });
    }
    Json(json!({"workflow_runs": g.repo(&full).runs})).into_response()
}

/// Replace one release's signature with a signature by another key.
pub fn forge_signature(f: &Fake, tag: &str) {
    let other = sig::generate();
    let mut files = f.files.lock().unwrap();
    let m = files
        [&format!("/gh-web/{REPO}/releases/download/{tag}/taro-enterprise-components.json")]
        .clone();
    files.insert(
        format!("/gh-web/{REPO}/releases/download/{tag}/taro-enterprise-components.json.sig"),
        sig::sign(&other, &m).into_bytes(),
    );
}

pub fn endpoints(base: &str) -> Endpoints {
    Endpoints {
        github_api: format!("{base}/gh-api"),
        repo: REPO.into(),
        reachability: vec![
            (
                "github".into(),
                "github.com reachable (HTTPS)".into(),
                "https://github.com/".into(),
            ),
            (
                "objects".into(),
                "objects.githubusercontent.com reachable (HTTPS)".into(),
                "https://objects.githubusercontent.com/".into(),
            ),
        ],
        rewrite: vec![
            ("https://github.com/".into(), format!("{base}/gh-web/")),
            (
                "https://objects.githubusercontent.com/".into(),
                format!("{base}/objects/"),
            ),
            (
                "https://release-assets.githubusercontent.com/".into(),
                format!("{base}/objects/"),
            ),
        ],
    }
}

pub const WIZ_PORT: u16 = 45999;
pub const ACCESS: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

pub struct Wiz {
    pub app: Arc<AppState>,
    pub router: Router,
    pub responses: Mutex<Vec<String>>,
    pub fake: Arc<Fake>,
    pub sources: HashMap<String, Vec<u8>>,
    pub out: PathBuf,
    pub opened: Arc<Mutex<Vec<PathBuf>>>,
    pub key: ed25519_dalek::SigningKey,
}

pub fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "taro-setup-{name}-{}-{}",
        std::process::id(),
        taro_enterprise_setup::secret::random_hex(4)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub async fn wizard_at(name: &str, addr: SocketAddr) -> Wiz {
    let key = sig::generate();
    let (fake, sources) = start_fake(&key).await;
    let out = tmp(name);
    let opened = Arc::new(Mutex::new(Vec::new()));
    let o2 = opened.clone();
    let app = AppState::new(
        addr,
        ACCESS.to_string(),
        Options {
            endpoints: endpoints(&fake.base()),
            timings: Timings::fast(),
            keys: TrustedKeys::from_keys(vec![key.verifying_key()]),
            output_dir: out.clone(),
            key_dir: tmp(&format!("{name}-keys")),
            open_folder: Arc::new(move |p: &std::path::Path| {
                o2.lock().unwrap().push(p.to_path_buf());
                Ok(())
            }),
        },
    );
    let router = taro_enterprise_setup::server::router(app.clone());
    Wiz {
        app,
        router,
        responses: Mutex::new(vec![]),
        fake,
        sources,
        out,
        opened,
        key,
    }
}

/// Another wizard session on the same fake GitHub and the same key backup folder (the same PC).
pub async fn wizard_sharing(name: &str, w: &Wiz) -> Wiz {
    let out = tmp(&format!("{name}-2"));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let o2 = opened.clone();
    let app = AppState::new(
        SocketAddr::from(([127, 0, 0, 1], WIZ_PORT)),
        ACCESS.to_string(),
        Options {
            endpoints: endpoints(&w.fake.base()),
            timings: Timings::fast(),
            keys: TrustedKeys::from_keys(vec![w.key.verifying_key()]),
            output_dir: out.clone(),
            key_dir: w.app.key_dir.clone(),
            open_folder: Arc::new(move |p: &std::path::Path| {
                o2.lock().unwrap().push(p.to_path_buf());
                Ok(())
            }),
        },
    );
    let router = taro_enterprise_setup::server::router(app.clone());
    Wiz {
        app,
        router,
        responses: Mutex::new(vec![]),
        fake: w.fake.clone(),
        sources: w.sources.clone(),
        out,
        opened,
        key: w.key.clone(),
    }
}

pub async fn wizard(name: &str) -> Wiz {
    wizard_at(name, SocketAddr::from(([127, 0, 0, 1], WIZ_PORT))).await
}

impl Wiz {
    /// Send a well-formed request (correct Host, Origin and token).
    pub async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("host", format!("127.0.0.1:{WIZ_PORT}"))
            .header("origin", format!("http://127.0.0.1:{WIZ_PORT}"))
            .header("x-setup-token", ACCESS);
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let (s, text) = self.raw(b.body(body).unwrap()).await;
        (
            s,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    pub async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.call("POST", path, Some(body)).await
    }

    pub async fn upload(&self, slot: &str, bytes: Vec<u8>) -> (StatusCode, Value) {
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/branding/{slot}"))
            .header("host", format!("127.0.0.1:{WIZ_PORT}"))
            .header("origin", format!("http://127.0.0.1:{WIZ_PORT}"))
            .header("x-setup-token", ACCESS)
            .header("content-type", "application/octet-stream")
            .body(Body::from(bytes))
            .unwrap();
        let (s, text) = self.raw(req).await;
        (
            s,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    pub async fn raw(&self, req: Request) -> (StatusCode, String) {
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let s = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 32 * 1024 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.responses.lock().unwrap().push(text.clone());
        (s, text)
    }

    pub async fn state(&self) -> Value {
        self.call("GET", "/api/state", None).await.1["state"].clone()
    }

    /// Poll the state until `pred` holds (or panic after ~20 s).
    pub async fn wait_for(&self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..2000 {
            let s = self.state().await;
            if pred(&s) {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}: {}", self.state().await);
    }

    /// Steps 2-5 with valid inputs (custom colour `#3D7A5B`, logo and icon).
    pub async fn fill(&self) {
        let (s, v) = self
            .post(
                "/api/identity",
                json!({"provider_id": "acme-cloud", "name": "Acme Connect", "short_name": "Acme", "vendor": "Acme Cloud Ltd", "support_url": "https://acme.example/help", "support_email": "help@acme.example"}),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        let (s, v) = self
            .post(
                "/api/branding",
                json!({"primary_color": "3d7a5b", "theme": "system"}),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        let png = |w, h| taro_enterprise_setup::branding::fixtures::png(w, h);
        assert_eq!(self.upload("logo", png(400, 120)).await.0, 200);
        assert_eq!(self.upload("icon", png(256, 256)).await.0, 200);
        let (s, v) = self.post("/api/relay/key/generate", json!({})).await;
        assert_eq!(s, 200, "{v}");
        let (s, v) = self
            .post(
                "/api/relay",
                json!({"host": "relay.acme.example", "port": 7470, "client_updates": "notify"}),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        let (s, v) = self.post("/api/components/discover", json!({})).await;
        assert_eq!(s, 200, "{v}");
    }

    pub async fn select(&self, tag: &str) -> (StatusCode, Value) {
        self.post("/api/components/select", json!({"tag": tag}))
            .await
    }

    pub async fn build(&self) -> (StatusCode, Value) {
        self.post(
            "/api/build/start",
            json!({"output_dir": self.out.display().to_string()}),
        )
        .await
    }

    pub async fn wait_build(&self) -> Value {
        self.wait_for("the build to end", |s| s["build"]["running"] == false)
            .await
    }

    /// The relay key (via the explicit reveal call).
    pub async fn relay_key(&self) -> String {
        let (s, v) = self.post("/api/relay/key/reveal", json!({})).await;
        assert_eq!(s, 200, "{v}");
        v["key"].as_str().unwrap().to_string()
    }

    /// Names of everything in the output folder (recursively).
    pub fn out_names(&self) -> Vec<String> {
        let mut v = Vec::new();
        fn walk(root: &std::path::Path, d: &std::path::Path, v: &mut Vec<String>) {
            for e in std::fs::read_dir(d).unwrap() {
                let e = e.unwrap();
                let rel = e.path().strip_prefix(root).unwrap().display().to_string();
                if e.file_type().unwrap().is_dir() {
                    v.push(format!("{rel}/"));
                    walk(root, &e.path(), v);
                } else {
                    v.push(rel);
                }
            }
        }
        walk(&self.out, &self.out, &mut v);
        v.sort();
        v
    }
}
