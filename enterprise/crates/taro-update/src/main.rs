//! `taro-update`: the release side of Taro Enterprise updates (the release workflow runs it).
//!
//!   taro-update keygen
//!   taro-update check-key            (the secret's public half is in release-signing.pub)
//!   taro-update pack --component relay --version 0.2.0 --core x86_64=DIR [--core aarch64=DIR]
//!                    [--base enterprise-core-files.json] [--migrations DIR] [--restart taro-relay]
//!                    [--commit SHA] --out enterprise-update.zip --core-files-out FILE
//!   taro-update sign FILE            (writes FILE.sig)
//!   taro-update verify FILE [--keys FILE]
//!   taro-update inspect enterprise-update.zip [--keys FILE]
//!
//! The private key comes from the TARO_ENTERPRISE_SIGNING_KEY environment variable only.

use std::path::PathBuf;
use taro_update::{manifest::CoreFiles, pack, sig};

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("taro-update: {msg}");
    std::process::exit(1)
}

fn key() -> ed25519_dalek::SigningKey {
    let v = std::env::var("TARO_ENTERPRISE_SIGNING_KEY").unwrap_or_default();
    if v.trim().is_empty() {
        die("TARO_ENTERPRISE_SIGNING_KEY is not set: releases are signed with it (add it as a secret of the release workflow)");
    }
    let k =
        sig::parse_secret(&v).unwrap_or_else(|e| die(format!("TARO_ENTERPRISE_SIGNING_KEY: {e}")));
    // what this repository's relays will trust is compiled in from release-signing.pub: a
    // signature by any other key would be refused by every relay built from it
    // (tests sign with a throwaway key and name its public half in TARO_UPDATE_TRUST)
    if !keys(std::env::var("TARO_UPDATE_TRUST").ok()).contains(&k.verifying_key()) {
        die(format!(
            "TARO_ENTERPRISE_SIGNING_KEY does not belong to enterprise/keys/release-signing.pub: add its public half ({}) there, or set the secret that matches it",
            sig::public_text(&k.verifying_key())
        ));
    }
    k
}

fn keys(path: Option<String>) -> sig::TrustedKeys {
    match path {
        Some(p) => sig::TrustedKeys::parse(
            &std::fs::read_to_string(&p).unwrap_or_else(|e| die(format!("{p}: {e}"))),
        )
        .unwrap_or_else(|e| die(format!("{p}: {e}"))),
        None => taro_update::official_keys(),
    }
}

fn now() -> String {
    std::env::var("SOURCE_DATE").unwrap_or_else(|_| {
        let s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // days -> civil date (UTC)
        let (days, rem) = (s / 86400, s % 86400);
        let z = days as i64 + 719468;
        let era = z.div_euclid(146097);
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60
        )
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let flags = |name: &str| {
        args.windows(2)
            .filter(|w| w[0] == name)
            .map(|w| w[1].clone())
            .collect::<Vec<_>>()
    };
    match args.first().map(String::as_str) {
        Some("keygen") => {
            let k = sig::generate();
            println!(
                "secret (TARO_ENTERPRISE_SIGNING_KEY): {}",
                sig::secret_text(&k)
            );
            println!(
                "public (enterprise/keys/release-signing.pub): {}",
                sig::public_text(&k.verifying_key())
            );
        }
        Some("check-key") => {
            let k = key();
            println!("ok: {}", sig::public_text(&k.verifying_key()));
        }
        Some("pack") => {
            let need = |n: &str| flag(n).unwrap_or_else(|| die(format!("{n} is required")));
            let cores = flags("--core")
                .into_iter()
                .map(|c| {
                    let (a, d) = c
                        .split_once('=')
                        .unwrap_or_else(|| die(format!("--core {c}: ARCH=DIR")));
                    (a.to_string(), PathBuf::from(d))
                })
                .collect();
            let base = flag("--base").map(|p| -> CoreFiles {
                serde_json::from_slice(
                    &std::fs::read(&p).unwrap_or_else(|e| die(format!("{p}: {e}"))),
                )
                .unwrap_or_else(|e| die(format!("{p}: {e}")))
            });
            let input = pack::Input {
                component: need("--component"),
                version: need("--version"),
                released_at: now(),
                cores,
                base,
                migrations: flag("--migrations").map(PathBuf::from),
                restart: flags("--restart"),
                source_commit: flag("--commit"),
            };
            let out = PathBuf::from(need("--out"));
            let o = pack::pack(&input, &key(), &out).unwrap_or_else(|e| die(e));
            let cf = need("--core-files-out");
            std::fs::write(&cf, serde_json::to_vec_pretty(&o.core_files).unwrap())
                .unwrap_or_else(|e| die(format!("{cf}: {e}")));
            let m = &o.manifest;
            println!(
                "{}: {} {} for {}..={} — {} file(s) to write, {} to remove, {} migration(s)",
                out.display(),
                m.component,
                m.version,
                m.min_from_version,
                m.max_from_version,
                m.files.len(),
                m.remove.len(),
                m.migrations.len()
            );
            for f in &m.files {
                println!(
                    "  {:7} {}{}",
                    format!("{:?}", f.op).to_lowercase(),
                    f.path,
                    f.arch
                        .as_ref()
                        .map(|a| format!(" ({a})"))
                        .unwrap_or_default()
                );
            }
            for r in &m.remove {
                println!("  remove  {}", r.path);
            }
        }
        Some("sign") => {
            let f = args.get(1).unwrap_or_else(|| die("sign FILE"));
            let bytes = std::fs::read(f).unwrap_or_else(|e| die(format!("{f}: {e}")));
            std::fs::write(format!("{f}.sig"), sig::sign(&key(), &bytes) + "\n")
                .unwrap_or_else(|e| die(e));
            println!("{f}.sig");
        }
        Some("verify") => {
            let f = args.get(1).unwrap_or_else(|| die("verify FILE"));
            let bytes = std::fs::read(f).unwrap_or_else(|e| die(format!("{f}: {e}")));
            let s = std::fs::read_to_string(format!("{f}.sig"))
                .unwrap_or_else(|e| die(format!("{f}.sig: {e}")));
            match keys(flag("--keys")).verify(&bytes, &s) {
                Ok(k) => println!("{f}: signed by {k}"),
                Err(e) => die(format!("{f}: {e}")),
            }
        }
        Some("inspect") => {
            let f = args.get(1).unwrap_or_else(|| die("inspect ZIP"));
            let p = taro_update::apply::open(std::path::Path::new(f), &keys(flag("--keys")))
                .unwrap_or_else(|e| die(e));
            println!("signed by {}", p.signed_by);
            println!("{}", serde_json::to_string_pretty(&p.manifest).unwrap());
        }
        _ => die("usage: taro-update keygen | pack … | sign FILE | verify FILE | inspect ZIP"),
    }
}
