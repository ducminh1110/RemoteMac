//! Taro Enterprise Setup Wizard.
//!
//! A local web wizard served on 127.0.0.1 that walks an operator through bootstrapping a
//! Taro Enterprise deployment: preflight, deployment settings, branding, domain verification,
//! Stripe billing, a private GitHub repository generated from the deployment template, the
//! release build, and post-deployment verification.

pub mod bootstrap;
pub mod branding;
pub mod config;
pub mod domain;
pub mod github;
pub mod platform;
pub mod preflight;
pub mod redact;
pub mod release;
pub mod remember;
pub mod sealed;
pub mod secret;
pub mod server;
pub mod state;
pub mod stripe;
pub mod summary;
pub mod util;
pub mod validate;
pub mod verify;
