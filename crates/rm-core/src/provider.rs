use rm_protocol::CapabilityReport;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("provisioning failed: {0}")]
    Provision(String),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("provider error: {0}")]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHandle {
    pub session_id: String,
    /// Provider-specific run identifier (e.g. a GitHub Actions run id).
    pub provider_ref: String,
    /// Seconds until the backing VM is expected to disappear, if known.
    pub max_lifetime_secs: Option<u64>,
}

/// Backends (GitHub Actions, personal Mac, cloud Mac, ...) implement this.
/// Nothing here may leak into the video/input protocol.
pub trait ComputeProvider {
    fn authenticate(&mut self) -> Result<(), ProviderError>;
    fn create_session(&mut self, session_id: &str) -> Result<SessionHandle, ProviderError>;
    fn wait_until_ready(&mut self, handle: &SessionHandle) -> Result<(), ProviderError>;
    fn terminate_session(&mut self, handle: &SessionHandle) -> Result<(), ProviderError>;
    fn get_capabilities(&self, handle: &SessionHandle) -> Result<CapabilityReport, ProviderError>;
}
