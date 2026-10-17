//! The provider adapter contract: how Taro Enterprise talks to whoever owns the Macs. One adapter
//! per provider; the rest of the system sees only this trait. Every call is bounded by a
//! timeout, every error says whether a retry can help, and nothing an adapter returns is shown to
//! a customer without going through `safe_diagnostics`.

pub mod mock;

use async_trait::async_trait;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub regions: Vec<String>,
    pub chips: Vec<String>,
    /// what the adapter can do beyond the required operations
    pub start_stop: bool,
    pub idempotency_keys: bool,
}

/// A Mac as the provider reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resource {
    pub external_id: String,
    pub display_name: String,
    pub model: String,
    pub chip: String,
    pub cpu_cores: i32,
    pub memory_gb: i32,
    pub storage_gb: i32,
    pub region: String,
    pub state: ResourceState,
}

/// The provider's own view of a Mac.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceState {
    Free,
    Reserved,
    Provisioning,
    InUse,
    Releasing,
    Maintenance,
    Offline,
}

impl ResourceState {
    pub fn key(&self) -> &'static str {
        match self {
            ResourceState::Free => "free",
            ResourceState::Reserved => "reserved",
            ResourceState::Provisioning => "provisioning",
            ResourceState::InUse => "in_use",
            ResourceState::Releasing => "releasing",
            ResourceState::Maintenance => "maintenance",
            ResourceState::Offline => "offline",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provisioning {
    /// still being prepared (poll again)
    Pending,
    /// ready to use
    Ready,
    Failed {
        retryable: bool,
        message: String,
    },
}

/// How a client reaches the Mac (Taro's ID, the relay, a password for this rental). Fetched live
/// when a connection grant is redeemed, never stored by Taro Enterprise.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Connection {
    pub taro_id: String,
    pub relay: Option<String>,
    pub password: String,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("taro_id", &self.taro_id)
            .field("relay", &self.relay)
            .field("password", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Healthy,
    Degraded,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// a timeout, a 5xx, a rate limit: trying again later can work
    Retryable,
    /// the request cannot succeed as it is
    Permanent,
    NotFound,
    /// no Mac free for it
    NoCapacity,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub kind: ErrorKind,
    /// safe to store and show to operators (no credentials, no internal addresses)
    pub message: String,
}

impl Error {
    pub fn retryable(m: impl Into<String>) -> Error {
        Error {
            kind: ErrorKind::Retryable,
            message: m.into(),
        }
    }
    pub fn permanent(m: impl Into<String>) -> Error {
        Error {
            kind: ErrorKind::Permanent,
            message: m.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The operations an adapter implements. `idempotency_key` is stable across retries of one
/// operation (the provisioning job's id), so a provider that supports keys never creates twice.
#[async_trait]
pub trait Provider: Send + Sync {
    fn kind(&self) -> &'static str;
    /// a mock or test double: the portal says so instead of presenting it as real capacity
    fn is_simulated(&self) -> bool;
    async fn list_capabilities(&self) -> Result<Capabilities>;
    async fn list_inventory(&self) -> Result<Vec<Resource>>;
    async fn get_resource(&self, external_id: &str) -> Result<Resource>;
    async fn reserve_resource(&self, external_id: &str, idempotency_key: &str) -> Result<()>;
    /// starts preparing the Mac; returns the provider's reference for polling
    async fn provision_resource(&self, external_id: &str, idempotency_key: &str) -> Result<String>;
    async fn get_provisioning_status(&self, external_ref: &str) -> Result<Provisioning>;
    async fn release_resource(&self, external_id: &str, idempotency_key: &str) -> Result<()>;
    async fn connection(&self, external_id: &str) -> Result<Connection>;
    async fn health_check(&self) -> Result<Health>;
    /// what operators may see about a Mac, already stripped of anything sensitive
    async fn safe_diagnostics(&self, external_id: &str) -> Result<serde_json::Value>;
}

/// Calls an adapter operation with a timeout: an adapter that hangs is a retryable failure.
pub async fn bounded<T>(
    what: &str,
    fut: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(std::time::Duration::from_secs(30), fut).await {
        Ok(r) => r,
        Err(_) => Err(Error::retryable(format!(
            "{what}: the provider did not answer in 30 s"
        ))),
    }
}
