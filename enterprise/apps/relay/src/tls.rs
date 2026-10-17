//! TLS for the relay's web endpoints (status page, health over HTTPS): certificates from an ACME
//! CA by HTTP-01, kept in `/var/lib/taro/tls/`, renewed by the running relay 30 days before they
//! expire and swapped in without a restart. The relay protocol itself does not depend on it
//! (sessions are end-to-end encrypted by the clients).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Renew when fewer days than this remain.
pub const RENEW_DAYS: i64 = 30;

#[derive(Clone, Debug, serde::Serialize)]
pub struct CertInfo {
    pub names: Vec<String>,
    pub issuer: String,
    /// unix seconds
    pub not_after: i64,
    pub days_left: i64,
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The leaf certificate of a PEM chain.
pub fn cert_info(chain_pem: &[u8]) -> Result<CertInfo, String> {
    use rustls_pki_types::{pem::PemObject, CertificateDer};
    let first = CertificateDer::pem_slice_iter(chain_pem)
        .next()
        .ok_or("no certificate in the file")?
        .map_err(|e| e.to_string())?;
    let (_, cert) = x509_parser::parse_x509_certificate(first.as_ref())
        .map_err(|e| format!("not a certificate: {e}"))?;
    let mut names = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for n in &san.value.general_names {
            if let x509_parser::extensions::GeneralName::DNSName(d) = n {
                names.push(d.to_string());
            }
        }
    }
    let not_after = cert.validity().not_after.timestamp();
    Ok(CertInfo {
        names,
        issuer: cert.issuer().to_string(),
        not_after,
        days_left: (not_after - now_unix()) / 86400,
    })
}

/// The certificate covers `host` (exactly, or by a one-level wildcard).
pub fn covers(info: &CertInfo, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    info.names.iter().any(|n| {
        let n = n.to_ascii_lowercase();
        n == host
            || n.strip_prefix("*.")
                .is_some_and(|base| host.split_once('.').is_some_and(|(_, rest)| rest == base))
    })
}

fn certified_key(
    chain_pem: &[u8],
    key_pem: &[u8],
) -> Result<Arc<rustls::sign::CertifiedKey>, String> {
    use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
    let certs: Vec<_> = CertificateDer::pem_slice_iter(chain_pem)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("certificate: {e}"))?;
    if certs.is_empty() {
        return Err("no certificate".into());
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| format!("key: {e}"))?;
    let provider = rustls::crypto::ring::default_provider();
    let signing = provider
        .key_provider
        .load_private_key(key)
        .map_err(|e| format!("key: {e}"))?;
    let ck = rustls::sign::CertifiedKey::new(certs, signing);
    ck.keys_match()
        .map_err(|_| "the private key does not belong to the certificate".to_string())?;
    Ok(Arc::new(ck))
}

/// Check a certificate and key pair from files (for `tls.mode = files` and after ACME).
pub fn check_pair(cert: &Path, key: &Path, host: &str) -> Result<CertInfo, String> {
    let c = std::fs::read(cert).map_err(|e| format!("{}: {e}", cert.display()))?;
    let k = std::fs::read(key).map_err(|e| format!("{}: {e}", key.display()))?;
    certified_key(&c, &k)?;
    let info = cert_info(&c)?;
    if !covers(&info, host) {
        return Err(format!(
            "the certificate is for {} and not {host}",
            info.names.join(", ")
        ));
    }
    if info.days_left < 0 {
        return Err("the certificate has expired".into());
    }
    Ok(info)
}

/// The certificate in use, swapped when it is renewed.
#[derive(Debug)]
pub struct Reloadable(RwLock<Arc<rustls::sign::CertifiedKey>>);

impl Reloadable {
    pub fn load(cert: &Path, key: &Path) -> Result<Arc<Reloadable>, String> {
        let ck = certified_key(
            &std::fs::read(cert).map_err(|e| format!("{}: {e}", cert.display()))?,
            &std::fs::read(key).map_err(|e| format!("{}: {e}", key.display()))?,
        )?;
        Ok(Arc::new(Reloadable(RwLock::new(ck))))
    }

    pub fn reload(&self, cert: &Path, key: &Path) -> Result<(), String> {
        let ck = certified_key(
            &std::fs::read(cert).map_err(|e| e.to_string())?,
            &std::fs::read(key).map_err(|e| e.to_string())?,
        )?;
        *self.0.write().unwrap() = ck;
        Ok(())
    }

    pub fn server_config(self: &Arc<Self>) -> Arc<rustls::ServerConfig> {
        let mut c = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_no_client_auth()
        .with_cert_resolver(self.clone());
        c.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(c)
    }
}

impl rustls::server::ResolvesServerCert for Reloadable {
    fn resolve(
        &self,
        _: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.read().unwrap().clone())
    }
}

/// HTTP-01 answers being offered: token -> key authorization.
#[derive(Clone, Default)]
pub struct Challenges(Arc<RwLock<HashMap<String, String>>>);

impl Challenges {
    pub fn get(&self, token: &str) -> Option<String> {
        self.0.read().unwrap().get(token).cloned()
    }
    fn set(&self, token: String, value: String) {
        self.0.write().unwrap().insert(token, value);
    }
    fn clear(&self) {
        self.0.write().unwrap().clear();
    }
}

/// Where the ACME account and certificates are kept.
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn account(&self) -> PathBuf {
        self.dir.join("acme-account.json")
    }
}

/// Write a secret file: owner-only from the start, through a rename.
pub fn write_secret(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Get a certificate for `host` from the ACME CA at `directory`, answering the HTTP-01
/// challenge through `challenges` (served on port 80 by whoever holds it). Writes the chain and
/// key to `cert`/`key`.
pub async fn issue(
    directory: &str,
    email: &str,
    host: &str,
    store: &Store,
    challenges: &Challenges,
    cert: &Path,
    key: &Path,
) -> Result<CertInfo, String> {
    use instant_acme::{
        Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
        NewOrder, OrderStatus, RetryPolicy,
    };
    fn e(what: &'static str) -> impl Fn(instant_acme::Error) -> String {
        move |err| format!("{what}: {err}")
    }
    // one account per server, kept (Let's Encrypt limits new accounts)
    let saved: Option<AccountCredentials> = std::fs::read(store.account())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let account = match saved {
        Some(c) => Account::builder()
            .map_err(e("ACME client"))?
            .from_credentials(c)
            .await
            .map_err(e("ACME account"))?,
        None => {
            let contact = format!("mailto:{email}");
            let (a, creds) = Account::builder()
                .map_err(e("ACME client"))?
                .create(
                    &NewAccount {
                        contact: &[&contact],
                        terms_of_service_agreed: true,
                        only_return_existing: false,
                    },
                    directory.to_string(),
                    None,
                )
                .await
                .map_err(e("creating the ACME account"))?;
            write_secret(
                &store.account(),
                &serde_json::to_vec(&creds).map_err(|e| e.to_string())?,
            )?;
            a
        }
    };
    let ids = [Identifier::Dns(host.to_string())];
    let mut order = account
        .new_order(&NewOrder::new(&ids))
        .await
        .map_err(e("the certificate order"))?;
    {
        let mut auths = order.authorizations();
        while let Some(a) = auths.next().await {
            let mut a = a.map_err(e("authorization"))?;
            match a.status {
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Valid => continue,
                s => return Err(format!("the CA's authorization is {s:?}")),
            }
            let mut ch = a
                .challenge(ChallengeType::Http01)
                .ok_or("the CA offers no HTTP-01 challenge")?;
            challenges.set(
                ch.token.clone(),
                ch.key_authorization().as_str().to_string(),
            );
            ch.set_ready().await.map_err(e("answering the challenge"))?;
        }
    }
    let status = order
        .poll_ready(&RetryPolicy::new().timeout(std::time::Duration::from_secs(90)))
        .await
        .map_err(e("validation"));
    challenges.clear();
    let status = status?;
    if status != OrderStatus::Ready {
        return Err(format!("the CA could not validate {host} ({status:?}): it must reach http://{host}/.well-known/acme-challenge/ on port 80 of this server"));
    }
    let key_pem = order.finalize().await.map_err(e("finalizing"))?;
    let chain = order
        .poll_certificate(&RetryPolicy::new().timeout(std::time::Duration::from_secs(90)))
        .await
        .map_err(e("the certificate"))?;
    let info = cert_info(chain.as_bytes())?;
    write_secret(key, key_pem.as_bytes())?;
    write_secret(cert, chain.as_bytes())?;
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn self_signed(host: &str) -> (String, String) {
        let kp = rcgen::KeyPair::generate().unwrap();
        let c = rcgen::CertificateParams::new(vec![host.to_string()])
            .unwrap()
            .self_signed(&kp)
            .unwrap();
        (c.pem(), kp.serialize_pem())
    }

    #[test]
    fn pairs_are_checked() {
        let d = std::env::temp_dir().join(format!("taro-tls-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let (c, k) = self_signed("relay.acme.example");
        write_secret(&d.join("c.pem"), c.as_bytes()).unwrap();
        write_secret(&d.join("k.pem"), k.as_bytes()).unwrap();
        let info = check_pair(&d.join("c.pem"), &d.join("k.pem"), "relay.acme.example").unwrap();
        assert!(info.days_left > 300);
        assert!(check_pair(&d.join("c.pem"), &d.join("k.pem"), "other.example").is_err());
        let (_, other_key) = self_signed("relay.acme.example");
        write_secret(&d.join("k2.pem"), other_key.as_bytes()).unwrap();
        assert!(
            check_pair(&d.join("c.pem"), &d.join("k2.pem"), "relay.acme.example")
                .unwrap_err()
                .contains("does not belong")
        );
        let r = Reloadable::load(&d.join("c.pem"), &d.join("k.pem")).unwrap();
        let _ = r.server_config();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(d.join("k.pem"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn wildcards() {
        let i = CertInfo {
            names: vec!["*.acme.example".into()],
            issuer: String::new(),
            not_after: 0,
            days_left: 0,
        };
        assert!(
            covers(&i, "relay.acme.example")
                && !covers(&i, "a.b.acme.example")
                && !covers(&i, "acme.example")
        );
    }
}
