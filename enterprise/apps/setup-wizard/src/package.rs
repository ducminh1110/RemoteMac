//! Assembling a provider's packages from the verified downloads (pure Rust: zip, tar, flate2,
//! so it runs the same on Windows).
//!
//! Every file of the official package is carried over byte for byte (zip entries are copied
//! raw, without recompressing; tar entries keep their mode and time). Only three things change:
//! the top folder is renamed for the clients, the Windows exe is renamed, and the provider
//! folder (and, for the clients, a README naming the product) is added. Each function returns
//! what the archive must contain ([`Built::entries`]), which [`crate::check`] later compares
//! with what is really in the file.

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use rm_brand::Provider;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

/// The largest single file accepted inside an archive, and all files together.
pub const MAX_ENTRY: u64 = 512 << 20;
pub const MAX_TOTAL: u64 = 4 << 30;

/// The provider folder's files, exactly as written to `<output>/provider/`.
#[derive(Clone)]
pub struct ProviderFiles {
    pub json: Vec<u8>,
    pub logo: Option<Vec<u8>>,
    pub icon: Option<Vec<u8>>,
}

impl ProviderFiles {
    /// `(file name, bytes)`, provider.json first.
    pub fn files(&self) -> Vec<(&'static str, &[u8])> {
        let mut v: Vec<(&'static str, &[u8])> = vec![(rm_brand::FILE, &self.json)];
        if let Some(l) = &self.logo {
            v.push(("logo.png", l));
        }
        if let Some(i) = &self.icon {
            v.push(("icon.png", i));
        }
        v
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Dir,
    Symlink,
}

/// One entry an archive must have.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub kind: Kind,
    pub sha256: Option<String>,
    /// permission bits (tar only; zip does not carry them reliably)
    pub mode: Option<u32>,
    pub link: Option<String>,
    /// `download:<path in the official package>`, `provider` or `generated`
    pub origin: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Zip,
    TarGz,
}

/// A produced archive and what it must contain.
#[derive(Clone, Debug, Serialize)]
pub struct Built {
    pub format: Format,
    /// the top folder inside the archive
    pub top: String,
    pub entries: BTreeMap<String, Entry>,
    /// executables carried over: (path in the archive, sha256), equal to the official files
    pub binaries: Vec<(String, String)>,
}

impl Built {
    fn new(format: Format, top: &str) -> Built {
        Built {
            format,
            top: top.into(),
            entries: BTreeMap::new(),
            binaries: vec![],
        }
    }

    fn add(&mut self, path: String, e: Entry) -> Result<(), String> {
        if self.entries.insert(path.clone(), e).is_some() {
            return Err(format!("{path} would be in the package twice"));
        }
        Ok(())
    }
}

/// sha256 of a stream, at most `max` bytes.
fn hash_reader(r: &mut dyn Read, max: u64) -> Result<(String, u64), String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut n: u64 = 0;
    loop {
        let k = r.read(&mut buf).map_err(|e| e.to_string())?;
        if k == 0 {
            break;
        }
        n += k as u64;
        if n > max {
            return Err(format!("a file is larger than {} MB", max >> 20));
        }
        h.update(&buf[..k]);
    }
    Ok((hex::encode(h.finalize()), n))
}

fn sha_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// A tidy relative archive path (no absolute paths, `..`, `.`, drive letters or control
/// characters). Directories may end in `/`.
pub fn check_path(p: &str) -> Result<(), String> {
    let t = p.strip_suffix('/').unwrap_or(p);
    if t.is_empty()
        || t.starts_with('/')
        || t.contains('\\')
        || t.contains(':')
        || t.chars().any(char::is_control)
        || t.split('/').any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err(format!("unsafe path {p:?} in the package; refused"));
    }
    Ok(())
}

/// The single top folder all `names` are in (none: the files are at the root).
fn top_folder<'a>(names: impl Iterator<Item = &'a str> + Clone) -> Option<String> {
    let first = names.clone().next()?;
    let top = first.split('/').next()?.to_string();
    let all_in = names.into_iter().all(|n| {
        n.strip_prefix(top.as_str())
            .is_some_and(|r| r.is_empty() || r.starts_with('/'))
    });
    // a lone file at the root is not a folder
    (all_in && first.len() > top.len()).then_some(top)
}

fn rel_of<'a>(name: &'a str, top: &Option<String>) -> &'a str {
    match top {
        Some(t) => name[t.len()..].trim_start_matches('/'),
        None => name,
    }
}

/// macOS metadata that has no place in a package.
fn is_mac_junk(rel: &str) -> bool {
    let base = rel.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    base.starts_with("._") || base == ".DS_Store" || rel.starts_with("__MACOSX")
}

/// The files the builder writes into a provider folder.
const OURS: &[&str] = &["provider.json", "logo.png", "icon.png"];

fn under_provider(rel: &str) -> bool {
    let l = rel.to_ascii_lowercase();
    l == "provider" || l == "provider/" || l.starts_with("provider/")
}

fn zip_time(secs: u64) -> zip::DateTime {
    let (y, m, d, hh, mm, ss) = crate::util::civil(secs);
    zip::DateTime::from_date_and_time(
        y.clamp(1980, 2107) as u16,
        m as u8,
        d as u8,
        hh as u8,
        mm as u8,
        ss as u8,
    )
    .unwrap_or_default()
}

/// `Taro-windows.zip` (its files at the root or in one top folder; `Taro.exe` required) to the
/// provider's zip: `<short>/<short>.exe`, every other file unchanged, `<short>/provider/…`,
/// and a new README.txt.
pub fn windows_zip(
    src: &Path,
    out: &Path,
    short: &str,
    pf: &ProviderFiles,
    readme: &str,
    mtime: u64,
) -> Result<Built, String> {
    let open = |p: &Path| -> Result<zip::ZipArchive<BufReader<File>>, String> {
        let f = File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
        zip::ZipArchive::new(BufReader::new(f))
            .map_err(|e| format!("Taro-windows.zip is not a valid zip ({e})"))
    };
    let mut za = open(src)?;
    let mut names = Vec::new();
    for i in 0..za.len() {
        let e = za.by_index_raw(i).map_err(|e| e.to_string())?;
        let n = e.name().map_err(|e| e.to_string())?.to_string();
        check_path(&n)?;
        if e.is_symlink() {
            return Err(format!(
                "{n}: symbolic links are not expected in Taro-windows.zip"
            ));
        }
        names.push((i, n, e.is_dir()));
    }
    let top = top_folder(names.iter().map(|(_, n, _)| n.as_str()));
    let exe_rel = names
        .iter()
        .map(|(_, n, _)| rel_of(n, &top))
        .find(|r| r.eq_ignore_ascii_case("Taro.exe"))
        .ok_or("Taro-windows.zip has no Taro.exe; refused")?
        .to_string();

    let file = File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut zw = zip::ZipWriter::new(BufWriter::new(file));
    let t = zip_time(mtime);
    let dir_opts = zip::write::SimpleFileOptions::default()
        .last_modified_time(t)
        .unix_permissions(0o755);
    let file_opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(t)
        .unix_permissions(0o644);
    let ze = |e: zip::result::ZipError| e.to_string();
    let mut b = Built::new(Format::Zip, short);
    let dir = |kind_origin: &str| Entry {
        kind: Kind::Dir,
        sha256: None,
        mode: None,
        link: None,
        origin: kind_origin.into(),
    };
    zw.add_directory(format!("{short}/"), dir_opts)
        .map_err(ze)?;
    b.add(format!("{short}/"), dir("generated"))?;
    let mut total: u64 = 0;
    for (i, name, is_dir) in &names {
        let rel = rel_of(name, &top);
        if rel.is_empty() || under_provider(rel) {
            continue;
        }
        let lower = rel.to_ascii_lowercase();
        if lower == "readme.txt" || lower == "readme.md" {
            continue; // regenerated below
        }
        if *is_dir {
            let p = format!("{short}/{}", rel.trim_end_matches('/'));
            zw.add_directory(&p, dir_opts).map_err(ze)?;
            b.add(format!("{p}/"), dir(&format!("download:{name}")))?;
            continue;
        }
        let (sha, n) = {
            let mut e = za.by_index(*i).map_err(ze)?;
            hash_reader(&mut e, MAX_ENTRY)?
        };
        total += n;
        if total > MAX_TOTAL {
            return Err("Taro-windows.zip unpacks to more than 4 GB; refused".into());
        }
        let is_exe = rel == exe_rel;
        let new_rel = if is_exe {
            format!("{short}.exe")
        } else {
            rel.to_string()
        };
        let p = format!("{short}/{new_rel}");
        let raw = za.by_index_raw(*i).map_err(ze)?;
        zw.raw_copy_file_rename(raw, &p).map_err(ze)?;
        if is_exe {
            b.binaries.push((p.clone(), sha.clone()));
        }
        b.add(
            p,
            Entry {
                kind: Kind::File,
                sha256: Some(sha),
                mode: None,
                link: None,
                origin: format!("download:{name}"),
            },
        )?;
    }
    zw.add_directory(format!("{short}/provider/"), dir_opts)
        .map_err(ze)?;
    b.add(format!("{short}/provider/"), dir("provider"))?;
    for (n, bytes) in pf.files() {
        let p = format!("{short}/provider/{n}");
        zw.start_file(&p, file_opts).map_err(ze)?;
        zw.write_all(bytes).map_err(|e| e.to_string())?;
        b.add(
            p,
            Entry {
                kind: Kind::File,
                sha256: Some(sha_hex(bytes)),
                mode: None,
                link: None,
                origin: "provider".into(),
            },
        )?;
    }
    let p = format!("{short}/README.txt");
    zw.start_file(&p, file_opts).map_err(ze)?;
    zw.write_all(readme.as_bytes()).map_err(|e| e.to_string())?;
    b.add(
        p,
        Entry {
            kind: Kind::File,
            sha256: Some(sha_hex(readme.as_bytes())),
            mode: None,
            link: None,
            origin: "generated".into(),
        },
    )?;
    let w = zw.finish().map_err(ze)?;
    w.into_inner()
        .map_err(|e| e.to_string())?
        .sync_all()
        .map_err(|e| e.to_string())?;
    Ok(b)
}

/// How a tar.gz is carried over.
pub struct TarPlan<'a> {
    /// what the package is called in messages
    pub what: &'a str,
    /// the output's top folder (none: keep the official one)
    pub rename_top: Option<&'a str>,
    /// paths (below the top folder) that must be present
    pub required: &'a [&'a str],
    /// a regenerated README.txt replaces the official one
    pub readme: Option<&'a str>,
    /// files below the top folder whose (trimmed) text must be this (`VERSION`, `ARCH`)
    pub expect_text: &'a [(&'a str, &'a str)],
    /// permission bits of provider.json in the package (logo and icon: 0644)
    pub provider_json_mode: u32,
}

struct HashRead<'r, R: Read> {
    inner: &'r mut R,
    h: Sha256,
}

impl<R: Read> Read for HashRead<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.h.update(&buf[..n]);
        Ok(n)
    }
}

fn open_tgz(p: &Path) -> Result<tar::Archive<GzDecoder<BufReader<File>>>, String> {
    let f = File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(tar::Archive::new(GzDecoder::new(BufReader::new(f))))
}

fn tar_path(e: &tar::Entry<'_, impl Read>) -> Result<String, String> {
    let p = e.path().map_err(|e| e.to_string())?;
    let s = p
        .to_str()
        .ok_or("a file name in the package is not UTF-8; refused")?
        .replace('\\', "/");
    Ok(s)
}

fn skip_type(t: tar::EntryType) -> bool {
    matches!(
        t,
        tar::EntryType::XGlobalHeader | tar::EntryType::XHeader | tar::EntryType::GNULongName
    )
}

fn check_link(path: &str, target: &str) -> Result<(), String> {
    if target.is_empty()
        || target.starts_with('/')
        || target.contains('\\')
        || target.split('/').any(|c| c == "..")
    {
        return Err(format!(
            "{path}: symbolic link to {target:?} leaves the package; refused"
        ));
    }
    Ok(())
}

/// Carry a `.tar.gz` over to `out`, replacing its `provider/` folder with `pf`.
pub fn repack_tgz(
    src: &Path,
    out: &Path,
    plan: &TarPlan<'_>,
    pf: &ProviderFiles,
    mtime: u64,
) -> Result<Built, String> {
    // pass 1: the layout
    let mut names: Vec<String> = Vec::new();
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    {
        let mut a = open_tgz(src)?;
        for e in a
            .entries()
            .map_err(|e| format!("{} is not a valid tar.gz ({e})", plan.what))?
        {
            let mut e = e.map_err(|e| format!("{} is damaged ({e})", plan.what))?;
            if skip_type(e.header().entry_type()) {
                continue;
            }
            let p = tar_path(&e)?;
            check_path(&p)?;
            let rel = p.split_once('/').map(|x| x.1).unwrap_or("");
            if plan.expect_text.iter().any(|(f, _)| *f == rel) && e.size() <= 4096 {
                let mut t = String::new();
                e.read_to_string(&mut t)
                    .map_err(|_| format!("{p} is not text"))?;
                texts.insert(rel.to_string(), t.trim().to_string());
            }
            names.push(p);
        }
    }
    for (f, want) in plan.expect_text {
        match texts.get(*f) {
            Some(t) if t == want => {}
            Some(t) => {
                return Err(format!(
                    "{} says {f} = {t:?}, the signed manifest says {want:?}; refused",
                    plan.what
                ))
            }
            None => return Err(format!("{} has no {f}; refused", plan.what)),
        }
    }
    let top = top_folder(names.iter().map(String::as_str))
        .ok_or_else(|| format!("{} does not have one top folder; refused", plan.what))?;
    let rels: BTreeSet<String> = names
        .iter()
        .map(|n| {
            rel_of(n, &Some(top.clone()))
                .trim_end_matches('/')
                .to_string()
        })
        .collect();
    for r in plan.required {
        let ok = if let Some(dir) = r.strip_suffix('/') {
            rels.iter()
                .any(|x| x.starts_with(&format!("{dir}/")) || x == dir)
        } else {
            rels.contains(*r)
        };
        if !ok {
            return Err(format!("{} has no {top}/{r}; refused", plan.what));
        }
    }
    let out_top = plan.rename_top.unwrap_or(&top).to_string();

    // pass 2: write
    let file = File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let gz = GzEncoder::new(BufWriter::new(file), flate2::Compression::default());
    let mut tb = tar::Builder::new(gz);
    tb.mode(tar::HeaderMode::Complete);
    let io = |e: std::io::Error| e.to_string();
    let mut b = Built::new(Format::TarGz, &out_top);
    let dir_header = |mode: u32, mtime: u64| {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Directory);
        h.set_mode(mode);
        h.set_mtime(mtime);
        h.set_uid(0);
        h.set_gid(0);
        h.set_size(0);
        h
    };
    let mut h = dir_header(0o755, mtime);
    tb.append_data(&mut h, format!("{out_top}/"), std::io::empty())
        .map_err(io)?;
    b.add(
        format!("{out_top}/"),
        Entry {
            kind: Kind::Dir,
            sha256: None,
            mode: Some(0o755),
            link: None,
            origin: "generated".into(),
        },
    )?;
    let mut total: u64 = 0;
    let mut provider_dir = false;
    let mut a = open_tgz(src)?;
    for e in a.entries().map_err(|e| e.to_string())? {
        let mut e = e.map_err(|e| format!("{} is damaged ({e})", plan.what))?;
        let et = e.header().entry_type();
        if skip_type(et) {
            continue;
        }
        let name = tar_path(&e)?;
        let rel = rel_of(&name, &Some(top.clone()))
            .trim_end_matches('/')
            .to_string();
        if rel.is_empty() || is_mac_junk(&rel) {
            continue;
        }
        if under_provider(&rel) {
            // the official package's own provider folder (and e.g. its README.txt) stays;
            // only the files the builder writes are replaced
            let base = rel.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
            let ours = rel.contains('/') && OURS.contains(&base.as_str());
            if ours || rel.matches('/').count() > 1 {
                continue;
            }
            if et == tar::EntryType::Directory {
                if !rel.eq_ignore_ascii_case("provider") {
                    continue;
                }
                provider_dir = true;
            }
        }
        if plan.readme.is_some() && rel.eq_ignore_ascii_case("README.txt") {
            continue;
        }
        let mode = e.header().mode().map_err(io)? & 0o7777;
        let mt = e.header().mtime().unwrap_or(mtime);
        let origin = format!("download:{name}");
        match et {
            tar::EntryType::Directory => {
                let p = format!("{out_top}/{rel}/");
                let mut h = dir_header(mode, mt);
                tb.append_data(&mut h, &p, std::io::empty()).map_err(io)?;
                b.add(
                    p,
                    Entry {
                        kind: Kind::Dir,
                        sha256: None,
                        mode: Some(mode),
                        link: None,
                        origin,
                    },
                )?;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let size = e.size();
                if size > MAX_ENTRY {
                    return Err(format!(
                        "{name} is larger than {} MB; refused",
                        MAX_ENTRY >> 20
                    ));
                }
                total += size;
                if total > MAX_TOTAL {
                    return Err(format!("{} unpacks to more than 4 GB; refused", plan.what));
                }
                let p = format!("{out_top}/{rel}");
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Regular);
                h.set_mode(mode);
                h.set_mtime(mt);
                h.set_uid(0);
                h.set_gid(0);
                h.set_size(size);
                let mut hr = HashRead {
                    inner: &mut e,
                    h: Sha256::new(),
                };
                tb.append_data(&mut h, &p, &mut hr).map_err(io)?;
                let sha = hex::encode(hr.h.finalize());
                if mode & 0o111 != 0 {
                    b.binaries.push((p.clone(), sha.clone()));
                }
                b.add(
                    p,
                    Entry {
                        kind: Kind::File,
                        sha256: Some(sha),
                        mode: Some(mode),
                        link: None,
                        origin,
                    },
                )?;
            }
            tar::EntryType::Symlink => {
                let target = e
                    .link_name()
                    .map_err(io)?
                    .and_then(|t| t.to_str().map(str::to_string))
                    .ok_or_else(|| format!("{name}: a symbolic link without a target"))?;
                check_link(&name, &target)?;
                let p = format!("{out_top}/{rel}");
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_mode(mode);
                h.set_mtime(mt);
                h.set_uid(0);
                h.set_gid(0);
                h.set_size(0);
                tb.append_link(&mut h, &p, &target).map_err(io)?;
                b.add(
                    p,
                    Entry {
                        kind: Kind::Symlink,
                        sha256: None,
                        mode: Some(mode),
                        link: Some(target),
                        origin,
                    },
                )?;
            }
            other => {
                return Err(format!(
                    "{name}: unsupported entry type {other:?} in {}; refused",
                    plan.what
                ))
            }
        }
    }
    // the provider folder (unless the package has one), its files, then the README
    if !provider_dir {
        let p = format!("{out_top}/provider/");
        let mut h = dir_header(0o755, mtime);
        tb.append_data(&mut h, &p, std::io::empty()).map_err(io)?;
        b.add(
            p,
            Entry {
                kind: Kind::Dir,
                sha256: None,
                mode: Some(0o755),
                link: None,
                origin: "provider".into(),
            },
        )?;
    }
    let mut extra: Vec<(String, &[u8], &str, u32)> = pf
        .files()
        .into_iter()
        .map(|(n, bytes)| {
            let mode = if n == rm_brand::FILE {
                plan.provider_json_mode
            } else {
                0o644
            };
            (format!("{out_top}/provider/{n}"), bytes, "provider", mode)
        })
        .collect();
    if let Some(r) = plan.readme {
        extra.push((
            format!("{out_top}/README.txt"),
            r.as_bytes(),
            "generated",
            0o644,
        ));
    }
    for (p, bytes, origin, mode) in extra {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_mode(mode);
        h.set_mtime(mtime);
        h.set_uid(0);
        h.set_gid(0);
        h.set_size(bytes.len() as u64);
        tb.append_data(&mut h, &p, bytes).map_err(io)?;
        b.add(
            p,
            Entry {
                kind: Kind::File,
                sha256: Some(sha_hex(bytes)),
                mode: Some(mode),
                link: None,
                origin: origin.into(),
            },
        )?;
    }
    let gz = tb.into_inner().map_err(io)?;
    let w = gz.finish().map_err(io)?;
    w.into_inner()
        .map_err(|e| e.to_string())?
        .sync_all()
        .map_err(io)?;
    Ok(b)
}

/// The macOS client: `<short>/` with `taro`, `taro.sh`, `taro-update.sh` unchanged.
pub fn macos_tgz(
    src: &Path,
    out: &Path,
    short: &str,
    pf: &ProviderFiles,
    readme: &str,
    mtime: u64,
) -> Result<Built, String> {
    repack_tgz(
        src,
        out,
        &TarPlan {
            what: "Taro-macos.tar.gz",
            rename_top: Some(short),
            required: &["taro", "taro.sh", "taro-update.sh"],
            readme: Some(readme),
            expect_text: &[],
            provider_json_mode: 0o644,
        },
        pf,
        mtime,
    )
}

/// The relay: the same tree, its `provider/` filled (provider.json 0640, as `install.sh`
/// installs it). `VERSION` and `ARCH` must say what the signed manifest says.
pub fn relay_tgz(
    src: &Path,
    out: &Path,
    pf: &ProviderFiles,
    version: &str,
    arch: &str,
    mtime: u64,
) -> Result<Built, String> {
    repack_tgz(
        src,
        out,
        &TarPlan {
            what: "the relay package",
            rename_top: None,
            required: &["install.sh", "VERSION", "ARCH", "core/bin/taro-relay"],
            readme: None,
            expect_text: &[("VERSION", version), ("ARCH", arch)],
            provider_json_mode: 0o640,
        },
        pf,
        mtime,
    )
}

// ---------------------------------------------------------------------------------------------
// Reading packages back (validation)
// ---------------------------------------------------------------------------------------------

/// What an archive really holds: path -> entry (origin empty), plus the bytes of the entries
/// `capture` asks for.
pub struct Contents {
    pub entries: BTreeMap<String, Entry>,
    pub captured: BTreeMap<String, Vec<u8>>,
}

pub fn read_zip(p: &Path, capture: &dyn Fn(&str) -> bool) -> Result<Contents, String> {
    let f = File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut za = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| e.to_string())?;
    let mut c = Contents {
        entries: BTreeMap::new(),
        captured: BTreeMap::new(),
    };
    for i in 0..za.len() {
        let mut e = za.by_index(i).map_err(|e| e.to_string())?;
        let name = e.name().map_err(|e| e.to_string())?.to_string();
        check_path(&name)?;
        let entry = if e.is_dir() {
            Entry {
                kind: Kind::Dir,
                sha256: None,
                mode: None,
                link: None,
                origin: String::new(),
            }
        } else {
            let mut bytes = Vec::new();
            let sha = if capture(&name) {
                (&mut e)
                    .take(MAX_ENTRY + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                sha_hex(&bytes)
            } else {
                hash_reader(&mut e, MAX_ENTRY)?.0
            };
            if capture(&name) {
                c.captured.insert(name.clone(), bytes);
            }
            Entry {
                kind: Kind::File,
                sha256: Some(sha),
                mode: None,
                link: None,
                origin: String::new(),
            }
        };
        if c.entries.insert(name.clone(), entry).is_some() {
            return Err(format!("{name} is in the zip twice"));
        }
    }
    Ok(c)
}

pub fn read_tgz(p: &Path, capture: &dyn Fn(&str) -> bool) -> Result<Contents, String> {
    let mut a = open_tgz(p)?;
    let mut c = Contents {
        entries: BTreeMap::new(),
        captured: BTreeMap::new(),
    };
    for e in a.entries().map_err(|e| e.to_string())? {
        let mut e = e.map_err(|e| e.to_string())?;
        let et = e.header().entry_type();
        if skip_type(et) {
            continue;
        }
        let name = tar_path(&e)?;
        check_path(&name)?;
        let mode = e.header().mode().map_err(|e| e.to_string())? & 0o7777;
        let entry = match et {
            tar::EntryType::Directory => Entry {
                kind: Kind::Dir,
                sha256: None,
                mode: Some(mode),
                link: None,
                origin: String::new(),
            },
            tar::EntryType::Symlink => Entry {
                kind: Kind::Symlink,
                sha256: None,
                mode: Some(mode),
                link: e
                    .link_name()
                    .map_err(|e| e.to_string())?
                    .and_then(|t| t.to_str().map(str::to_string)),
                origin: String::new(),
            },
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let sha = if capture(&name) {
                    let mut bytes = Vec::new();
                    (&mut e)
                        .take(MAX_ENTRY + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|e| e.to_string())?;
                    let s = sha_hex(&bytes);
                    c.captured.insert(name.clone(), bytes);
                    s
                } else {
                    hash_reader(&mut e, MAX_ENTRY)?.0
                };
                Entry {
                    kind: Kind::File,
                    sha256: Some(sha),
                    mode: Some(mode),
                    link: None,
                    origin: String::new(),
                }
            }
            other => return Err(format!("{name}: unexpected entry type {other:?}")),
        };
        if c.entries.insert(name.clone(), entry).is_some() {
            return Err(format!("{name} is in the archive twice"));
        }
    }
    Ok(c)
}

/// The inner files of a source package: path -> sha256 (for "the binaries are unchanged").
pub fn source_hashes(src: &Path, format: Format) -> Result<BTreeMap<String, String>, String> {
    let all = |_: &str| false;
    let c = match format {
        Format::Zip => read_zip(src, &all)?,
        Format::TarGz => read_tgz(src, &all)?,
    };
    Ok(c.entries
        .into_iter()
        .filter_map(|(k, v)| v.sha256.map(|s| (k, s)))
        .collect())
}

// ---------------------------------------------------------------------------------------------
// Texts
// ---------------------------------------------------------------------------------------------

fn support_lines(p: &Provider) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(u) = &p.product.support_url {
        v.push(format!("Help: {u}"));
    }
    if let Some(e) = &p.product.support_email {
        v.push(format!("Support: {e}"));
    }
    v
}

/// README.txt of the Windows package (CRLF, for Notepad).
pub fn readme_windows(p: &Provider, taro_version: &str) -> String {
    let short = &p.product.short_name;
    let mut s = vec![
        format!("{} for Windows", p.product.name),
        format!("by {}", p.product.vendor),
        String::new(),
        "1. On the Mac run:  ./taro.sh --password YOUR-PASSWORD".into(),
        format!("2. Open {short}.exe, type the ID and the password the Mac shows, press Connect."),
        String::new(),
        format!("Relay server: {}", p.relay.address()),
        String::new(),
        format!(
            "The provider folder next to {short}.exe holds {}'s settings (provider.json), logo and",
            p.product.name
        ),
        "icon. Keep it next to the exe: updates of the program leave it in place.".into(),
        "Connections are end-to-end encrypted.".into(),
    ];
    let sup = support_lines(p);
    if !sup.is_empty() {
        s.push(String::new());
        s.extend(sup);
    }
    s.push(String::new());
    s.push(format!("Built on Taro {taro_version}."));
    s.join("\r\n") + "\r\n"
}

/// README.txt of the macOS package.
pub fn readme_macos(p: &Provider, taro_version: &str) -> String {
    let mut s = vec![
        format!(
            "{} for macOS (14 or later, Apple silicon and Intel)",
            p.product.name
        ),
        format!("by {}", p.product.vendor),
        String::new(),
        "  xattr -d com.apple.quarantine taro 2>/dev/null; chmod +x taro".into(),
        "  ./taro.sh --password YOUR-PASSWORD".into(),
        String::new(),
        "taro.sh checks the program, macOS and the permissions first, then shows the".into(),
        "ID and the password to type on the other computer (./taro.sh --check only checks).".into(),
        String::new(),
        format!("Relay server: {}", p.relay.address()),
        String::new(),
        format!(
            "The provider folder holds {}'s settings (provider.json), logo and icon.",
            p.product.name
        ),
        "Keep it next to taro: ./taro-update.sh updates the program and leaves it in place.".into(),
    ];
    let sup = support_lines(p);
    if !sup.is_empty() {
        s.push(String::new());
        s.extend(sup);
    }
    s.push(String::new());
    s.push(format!("Built on Taro {taro_version}."));
    s.join("\n") + "\n"
}

/// What an INSTALL.md describes.
pub struct InstallInfo<'a> {
    pub provider: &'a Provider,
    pub taro_version: &'a str,
    pub enterprise_version: &'a str,
    pub windows: Option<&'a str>,
    pub macos: Option<&'a str>,
    /// (arch, artifact path, top folder)
    pub relays: Vec<(&'a str, &'a str, &'a str)>,
}

pub fn install_md(i: &InstallInfo<'_>) -> String {
    let p = i.provider;
    let name = &p.product.name;
    let short = &p.product.short_name;
    let mut s = String::new();
    s.push_str(&format!("# {name}: installation\n\n"));
    s.push_str(&format!(
        "Built by {} for {} ({}), from Taro {} and Taro Enterprise {}.\n\n",
        crate::config::BUILT_BY,
        p.product.vendor,
        p.provider_id,
        i.taro_version,
        i.enterprise_version
    ));
    s.push_str("Check the files first: `sha256sum -c SHA256SUMS` (Linux, macOS) or `Get-FileHash` (Windows) against `SHA256SUMS`.\n\n");
    s.push_str(&format!(
        "Every part reads the same `provider/provider.json` (product, colour, relay `{}`). The programs themselves are the official, unmodified Taro builds.\n\n",
        p.relay.address()
    ));
    if let Some(w) = i.windows {
        s.push_str(&format!(
            "## Windows\n\n1. Unzip `{w}` anywhere (for example `C:\\Program Files\\{short}` or a user folder).\n2. Run `{short}\\{short}.exe`. Keep the `provider` folder next to it.\n\nThe exe is not signed with {}'s code-signing certificate (it is Taro's official build), so SmartScreen may ask once.\n\n",
            p.product.vendor
        ));
    }
    if let Some(m) = i.macos {
        s.push_str(&format!(
            "## macOS\n\n```sh\ntar xzf {file}\ncd {short}\nxattr -d com.apple.quarantine taro 2>/dev/null; chmod +x taro\n./taro.sh --password YOUR-PASSWORD\n```\n\n`taro.sh` shows the ID and the password to type on the Windows side. `./taro --stop` ends it.\n\nThis is a tar.gz, not a signed and notarized app: the package builder has no Apple Developer ID credentials or notarization infrastructure, so `taro` keeps Taro's own ad-hoc signature (editing it would break that signature). macOS asks for permission the first time; the `xattr` line above removes the download quarantine.\n\n",
            file = m.rsplit('/').next().unwrap_or(m),
        ));
    }
    for (arch, path, top) in &i.relays {
        let file = path.rsplit('/').next().unwrap_or(path);
        s.push_str(&format!(
            "## Linux relay ({arch})\n\nOn the server (DNS name `{host}` must point at it; port {port}):\n\n```sh\nscp {path} root@{host}:\nssh root@{host}\ntar xzf {file}\ncd {top}\nsudo ./install.sh\n```\n\n`install.sh` installs the relay (core in `/opt/taro`, this provider folder in `/etc/taro/provider`, provider.json readable by root only) and starts its first-run setup (Wizard 2), which checks DNS, obtains the TLS certificate and starts the service. The relay admits only clients that send the admission key in `provider.json`.\n\n",
            host = p.relay.host,
            port = p.relay.port,
        ));
    }
    s.push_str("## Updates\n\n");
    s.push_str("- **Relay:** each Taro Enterprise release publishes `enterprise-update.zip` (signed). On the server: `sudo taro-relay update apply --latest --yes` (it finds and verifies the newer releases itself), or `sudo taro-relay update apply enterprise-update.zip --yes` for a downloaded file. It changes only the shared core (`/opt/taro/core`), never `/etc/taro/provider` or the relay's data, and rolls back if the new version does not start.\n");
    s.push_str(&format!(
        "- **Clients:** {} Clients are updated in place from official Taro releases; the `provider` folder next to the program is never replaced.\n",
        match p.updates.clients {
            rm_brand::ClientUpdates::Notify => "they look for new Taro releases and offer them (`updates.clients = notify`).",
            rm_brand::ClientUpdates::Off => "they do not look for updates (`updates.clients = off`); ship new packages built with this wizard.",
        }
    ));
    s.push_str("- **Compatibility:** clients and relay must speak the same Taro protocol generation. Clients update only to Taro releases the relay supports: update the relay first, then the clients, or rebuild the packages from the newer Enterprise release with the same `provider.json` (same relay key).\n\n");
    s.push_str("## What is not here\n\nNo server administration credentials, TLS keys, database passwords or signing keys are part of these packages. The relay admission key in `provider.json` is the shared key every client of this provider sends, like the key built into standard Taro.\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_checked() {
        for ok in ["Taro/Taro.exe", "a/b/", "x"] {
            assert!(check_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "C:/x",
            "a\\b",
            "./a",
            "",
        ] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
        assert!(check_link("t/l", "lib.so.1").is_ok());
        assert!(check_link("t/l", "../../etc").is_err());
        assert!(check_link("t/l", "/etc/passwd").is_err());
    }

    #[test]
    fn top_folders() {
        let v = ["Taro/", "Taro/Taro.exe", "Taro/README.txt"];
        assert_eq!(top_folder(v.iter().copied()), Some("Taro".into()));
        let flat = ["Taro.exe", "README.txt"];
        assert_eq!(top_folder(flat.iter().copied()), None);
        let one = ["Taro.exe"];
        assert_eq!(top_folder(one.iter().copied()), None);
        let mixed = ["Taro/Taro.exe", "Other/x"];
        assert_eq!(top_folder(mixed.iter().copied()), None);
        let prefix = ["Taro/a", "TaroX/b"];
        assert_eq!(top_folder(prefix.iter().copied()), None);
    }

    #[test]
    fn junk_and_provider() {
        assert!(is_mac_junk("._taro"));
        assert!(is_mac_junk("x/.DS_Store"));
        assert!(!is_mac_junk("taro"));
        assert!(under_provider("provider/provider.json"));
        assert!(under_provider("Provider/"));
        assert!(!under_provider("providers.txt"));
    }
}
