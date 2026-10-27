//! The Taro Enterprise relay server: the relay (rm-relay) run as a service, its first-run setup
//! (Wizard 2), TLS, health checks and Enterprise updates of its core.

pub mod checks;
pub mod config;
pub mod portal;
pub mod serve;
pub mod setup;
pub mod system;
pub mod tls;
pub mod update;
pub mod web;
