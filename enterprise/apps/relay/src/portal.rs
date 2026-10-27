//! The Customer Portal, the Provider Console and their API, run by the relay itself (Taro
//! Enterprise 0.2: one server, one web site on the relay's HTTPS).
//!
//! - **Database**: the relay's own PostgreSQL, shipped in the core (`core/share/pg`: inside the
//!   folders a 0.1 relay's updater accepts), run as a child
//!   of the relay with its data in `/var/lib/taro/pg` and only a Unix socket there (no TCP port);
//!   or a `postgres://` database run elsewhere (`portal.database`).
//! - **Secrets**: the server's key in `/var/lib/taro/portal/secret.key` (made once, readable by
//!   the relay only) and the first-run setup code in `/var/lib/taro/portal/setup-code` (removed
//!   once the first administrator exists; `sudo taro-relay portal setup-code` shows or remakes it).
//! - **Turning it on**: Wizard 2 for a new server; for a relay set up before 0.2 the update does it
//!   (`ensure`), so updating the relay installs the portal and the console.

use crate::config::{Portal, Runtime, TlsMode};
use crate::system::System;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use taro_update::policy::Layout;

pub const USER: &str = crate::system::USER;
/// The Unix socket's "port" (its file name): not a TCP port, nothing listens on the network.
const PG_PORT: u16 = 5433;

pub fn dir(layout: &Layout) -> PathBuf {
    layout.state.join("portal")
}
pub fn secret_file(layout: &Layout) -> PathBuf {
    dir(layout).join("secret.key")
}
pub fn code_file(layout: &Layout) -> PathBuf {
    dir(layout).join("setup-code")
}
pub fn pg_dir(layout: &Layout) -> PathBuf {
    layout.state.join("pg")
}
/// The web apps (portal, console, their shared look), in the core.
pub fn web_dir(layout: &Layout) -> PathBuf {
    let installed = layout.core().join("web/enterprise");
    if installed.join("apps/customer-portal").is_dir() {
        return installed;
    }
    // a development build run from the source tree
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}
/// The bundled PostgreSQL's programs (TARO_PG_BIN: another installation, for tests).
pub fn pg_bin(layout: &Layout) -> PathBuf {
    std::env::var_os("TARO_PG_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| layout.core().join("share/pg/bin"))
}

/// `ABCD-EFGH-JKLM` from characters that cannot be mistaken for one another.
pub fn new_code() -> String {
    use rand::Rng;
    const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut r = rand::rngs::OsRng;
    let c: String = (0..12)
        .map(|_| A[r.gen_range(0..A.len())] as char)
        .collect();
    format!("{}-{}-{}", &c[..4], &c[4..8], &c[8..])
}

fn write_private(sys: &dyn System, p: &Path, text: &str) -> Result<(), String> {
    taro_update::migrate::write_atomic(p, text.as_bytes())?;
    crate::config::set_mode(p, 0o600)?;
    sys.chown(p, USER, USER)
}

fn mkdir(sys: &dyn System, p: &Path) -> Result<(), String> {
    std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
    crate::config::set_mode(p, 0o700)?;
    sys.chown(p, USER, USER)
}

/// Folders and secrets (as root, from setup or the updater). The setup code is made with the
/// key, the first time: once the first administrator exists the API removes it.
pub fn prepare(layout: &Layout, sys: &dyn System) -> Result<Vec<String>, String> {
    let mut done = vec![];
    mkdir(sys, &dir(layout))?;
    mkdir(sys, &pg_dir(layout))?;
    // the socket's folder (made here, as root, for the service's user)
    mkdir(sys, &socket_dir(layout))?;
    if !secret_file(layout).exists() {
        let mut b = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
        write_private(sys, &secret_file(layout), &hex::encode(b))?;
        write_private(sys, &code_file(layout), &new_code())?;
        done.push("portal key and setup code made".into());
    }
    done.push(format!("portal folders in {}", layout.state.display()));
    Ok(done)
}

/// The setup code, made again if it is gone (root). The API accepts it only while the console
/// has no administrator.
pub fn setup_code(layout: &Layout, sys: &dyn System) -> Result<String, String> {
    let p = code_file(layout);
    if let Ok(c) = std::fs::read_to_string(&p) {
        if !c.trim().is_empty() {
            return Ok(c.trim().to_string());
        }
    }
    mkdir(sys, &dir(layout))?;
    let c = new_code();
    write_private(sys, &p, &c)?;
    Ok(c)
}

/// A relay set up before 0.2 gets the portal when it is updated: turned on (with HTTPS; the
/// portal needs it), its folders and secrets made. Returns what was done (nothing: already on).
pub fn ensure(layout: &Layout, sys: &dyn System) -> Result<Vec<String>, String> {
    let Some(mut r) = Runtime::load(layout)? else {
        return Ok(vec![]);
    };
    if r.portal.is_some() {
        return Ok(vec![]);
    }
    let mut p = Portal::on();
    p.enabled = r.tls.mode != TlsMode::Off;
    r.portal = Some(p.clone());
    r.save(layout, &crate::setup::stamp())?;
    let mut done = vec![if p.enabled {
        format!(
            "portal and console turned on: https://{}/ and https://{}/console/",
            r.relay.public_host, r.relay.public_host
        )
    } else {
        "portal and console: off (they need HTTPS: sudo taro-relay setup)".into()
    }];
    if p.enabled {
        done.extend(prepare(layout, sys)?);
    }
    Ok(done)
}

/// The database URL for `postgres://` or the embedded one.
fn socket_dir(layout: &Layout) -> PathBuf {
    pg_dir(layout).join("run")
}

pub fn embedded_url(layout: &Layout, db: &str) -> String {
    format!(
        "postgres://taro@localhost:{PG_PORT}/{db}?host={}",
        socket_dir(layout).display()
    )
}

/// The relay's own PostgreSQL, running: stopped (fast) when this is dropped.
pub struct Embedded {
    child: Child,
}

impl Drop for Embedded {
    fn drop(&mut self) {
        // SIGINT: a fast shutdown (sessions closed, data flushed)
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGINT);
        }
        for _ in 0..100 {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Embedded {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    /// It ended on its own (a crash): the relay ends too, and systemd starts both again.
    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }
}

fn pg_command(layout: &Layout, program: &str) -> Command {
    let mut c = Command::new(pg_bin(layout).join(program));
    // tests run as root, which PostgreSQL refuses: TARO_PG_UID names the user to run it as
    #[cfg(unix)]
    if let Some(uid) = std::env::var("TARO_PG_UID")
        .ok()
        .and_then(|u| u.parse().ok())
    {
        use std::os::unix::process::CommandExt;
        c.uid(uid);
    }
    c
}

/// Start the bundled PostgreSQL (making its data folder the first time).
pub fn start_embedded(layout: &Layout) -> Result<Embedded, String> {
    let data = pg_dir(layout).join("data");
    let run = socket_dir(layout);
    std::fs::create_dir_all(&run).map_err(|e| format!("{}: {e}", run.display()))?;
    crate::config::set_mode(&run, 0o700)?;
    if !data.join("PG_VERSION").exists() {
        let out = pg_command(layout, "initdb")
            .arg("-D")
            .arg(&data)
            .args([
                "-U",
                "taro",
                "--auth=trust",
                "--encoding=UTF8",
                "--no-locale",
                "--no-instructions",
            ])
            .output()
            .map_err(|e| format!("initdb ({}): {e}", pg_bin(layout).display()))?;
        if !out.status.success() {
            return Err(format!(
                "initdb: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        eprintln!(
            "taro-relay: the portal's database was made in {}",
            data.display()
        );
    }
    let child = pg_command(layout, "postgres")
        .arg("-D")
        .arg(&data)
        .args([
            "-c",
            "listen_addresses=",
            "-c",
            &format!("unix_socket_directories={}", run.display()),
            "-c",
            &format!("port={PG_PORT}"),
            "-c",
            "unix_socket_permissions=0700",
            "-c",
            // no shared memory segments in /dev/shm (the service's sandbox)
            "dynamic_shared_memory_type=mmap",
            "-c",
            "max_connections=60",
            "-c",
            "shared_buffers=64MB",
            "-c",
            "log_min_messages=warning",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| format!("postgres ({}): {e}", pg_bin(layout).display()))?;
    let mut e = Embedded { child };
    // up when its socket answers
    let sock = run.join(format!(".s.PGSQL.{PG_PORT}"));
    for _ in 0..300 {
        if let Some(st) = e.exited() {
            return Err(format!("postgres stopped at start ({st}); see the journal"));
        }
        if sock.exists() && std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            return Ok(e);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("postgres did not start in 30 seconds".into())
}

/// The database of the portal: made if missing (embedded), the migrations applied.
pub async fn database(layout: &Layout, p: &Portal) -> Result<sqlx::PgPool, String> {
    let url = if p.database == "embedded" {
        // its socket answers a moment before it takes connections ("starting up")
        let mut tries = 0;
        let admin = loop {
            match taro_enterprise::db::connect(&embedded_url(layout, "postgres")).await {
                Ok(p) => break p,
                Err(e) if tries < 60 => {
                    tries += 1;
                    if tries == 60 {
                        return Err(format!("database: {e}"));
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(e) => return Err(format!("database: {e}")),
            }
        };
        let (have,): (bool,) =
            sqlx::query_as("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'taro')")
                .fetch_one(&admin)
                .await
                .map_err(|e| format!("database: {e}"))?;
        if !have {
            sqlx::query("CREATE DATABASE taro")
                .execute(&admin)
                .await
                .map_err(|e| format!("database: {e}"))?;
        }
        admin.close().await;
        embedded_url(layout, "taro")
    } else {
        p.database.clone()
    };
    let db = taro_enterprise::db::connect(&url).await.map_err(|e| {
        format!(
            "database: {}",
            taro_enterprise::redact::redact(&e.to_string())
        )
    })?;
    taro_enterprise::db::migrate(&db)
        .await
        .map_err(|e| format!("database migrations: {e}"))?;
    Ok(db)
}

/// The site's address: `https://host` (with the port when HTTPS is not on 443).
pub fn site(r: &Runtime) -> String {
    let port = r
        .tls
        .https_listen
        .parse::<std::net::SocketAddr>()
        .map(|a| a.port())
        .unwrap_or(443);
    if port == 443 {
        format!("https://{}", r.relay.public_host)
    } else {
        format!("https://{}:{port}", r.relay.public_host)
    }
}

/// The API's configuration for this server.
pub fn api_config(
    layout: &Layout,
    r: &Runtime,
    provider: &rm_brand::Provider,
    p: &Portal,
) -> Result<taro_enterprise::config::Config, String> {
    use taro_enterprise::config::{Config, Env, Secret};
    let key = std::fs::read_to_string(secret_file(layout)).map_err(|e| {
        format!(
            "{}: {e} (run: sudo taro-relay setup)",
            secret_file(layout).display()
        )
    })?;
    let site = site(r);
    let mut c = Config::for_tests("postgres://unused");
    c.env = Env::Production;
    c.database_url = Secret::new("postgres://(the relay's)");
    c.public_url = url::Url::parse(&site).map_err(|e| e.to_string())?;
    c.ops_url = url::Url::parse(&format!("{site}/console")).map_err(|e| e.to_string())?;
    c.secret_key = Secret::new(key.trim());
    c.smtp_url = p.smtp_url.clone().map(Secret::new);
    c.mail_from = p.mail_from.clone().unwrap_or_else(|| {
        format!(
            "{} <no-reply@{}>",
            provider.product.name, r.relay.public_host
        )
    });
    c.stripe = None;
    c.provider = "fleet".into();
    c.relay_address = Some(provider.relay.address());
    c.setup_code = None;
    c.setup_code_file = Some(code_file(layout));
    c.bootstrap_admin_email = None;
    c.web_dir = web_dir(layout);
    c.trusted_proxies = false;
    c.app_download_url = p.app_download_url.clone();
    Ok(c)
}

/// The web site's portal and console (the database started, migrated, the API's workers
/// running). `pg` keeps the embedded PostgreSQL alive while the relay runs.
pub async fn start_site(
    layout: &Layout,
    r: &Runtime,
    provider: &rm_brand::Provider,
    p: &Portal,
    pg: &mut Option<Embedded>,
) -> Result<axum::Router, String> {
    if p.database == "embedded" {
        let l = layout.clone();
        let e = tokio::task::spawn_blocking(move || start_embedded(&l))
            .await
            .map_err(|e| e.to_string())??;
        *pg = Some(e);
    }
    let db = database(layout, p).await?;
    let cfg = api_config(layout, r, provider, p)?;
    let fleet = std::sync::Arc::new(taro_enterprise::provider::fleet::Fleet::new(
        db.clone(),
        cfg.secret_key.expose(),
        cfg.relay_address.clone(),
    ));
    let state = taro_enterprise::build_state(cfg, db, fleet, None, None)
        .await
        .map_err(|e| format!("portal: {e}"))?;
    if let Err(e) = taro_enterprise::reconcile::sync_inventory(&state).await {
        eprintln!("taro-relay: first inventory sync: {e}");
    }
    taro_enterprise::spawn_workers(&state);
    Ok(taro_enterprise::http::single(&state))
}
