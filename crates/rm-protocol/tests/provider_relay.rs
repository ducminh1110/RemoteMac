//! A provider's relay set at start (a test binary of its own: the setting is once per process).
use rm_protocol::session::{compiled_relay, default_relay, provider_relay, set_builtin_relay};

#[test]
fn the_providers_relay_is_the_built_in_one_from_then_on() {
    assert_eq!(provider_relay(), None);
    assert_eq!(default_relay(), compiled_relay());
    assert!(set_builtin_relay("  ").is_err(), "an empty relay is refused");
    set_builtin_relay("relay.acme.example:7471").unwrap();
    assert_eq!(default_relay(), Some("relay.acme.example:7471"));
    assert_eq!(provider_relay(), Some("relay.acme.example:7471"));
    // the same again is fine; another one is refused and changes nothing
    set_builtin_relay("relay.acme.example:7471").unwrap();
    assert!(set_builtin_relay("remotemac.mooo.com:7470").is_err());
    assert_eq!(default_relay(), Some("relay.acme.example:7471"));
}
