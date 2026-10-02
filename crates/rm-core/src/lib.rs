//! Provider-agnostic session logic: provisioning state machine, the
//! `ComputeProvider` abstraction and the server-side application allowlist.

pub mod apps;
pub mod provider;
pub mod state;

pub use apps::{AppDescriptor, AppRegistry, LaunchError, ValidatedLaunch};
pub use provider::{ComputeProvider, ProviderError, SessionHandle};
pub use state::{Event, Failure, SessionState, StateError};
