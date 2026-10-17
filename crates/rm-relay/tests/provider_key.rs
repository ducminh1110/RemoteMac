//! A provider's relay key set at start (a test binary of its own: the setting is once per
//! process), and what a join then presents to a relay.
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

const ACME: &str = "acme-provider-key-0123456789";

#[test]
fn the_providers_key_is_sent_and_an_own_relay_gets_its_own() {
    // (the environment would win over the built-in key: not set for this test)
    std::env::remove_var("RM_RELAY_KEY");
    rm_relay::set_key_override(None);
    assert!(!rm_relay::has_provider_key());
    assert!(rm_relay::set_builtin_key("").is_err());
    rm_relay::set_builtin_key(ACME).unwrap();
    assert!(rm_relay::has_provider_key());
    assert_eq!(rm_relay::env_key().as_deref(), Some(ACME));
    rm_relay::set_builtin_key(ACME).unwrap();
    assert!(rm_relay::set_builtin_key("another-key-0123456789").is_err());

    // what the relay is shown: the provider's key
    let join_key = || {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut line = String::new();
            BufReader::new(s.try_clone().unwrap()).read_line(&mut line).unwrap();
            let _ = (&s).write_all(b"ERR test\n");
            let j: serde_json::Value = serde_json::from_str(&line).unwrap();
            j["key"].as_str().map(String::from)
        });
        let _ = rm_relay::join(&addr, "rm-123456789", rm_relay::Role::Client, "0123456789abcdef-token");
        t.join().unwrap()
    };
    assert_eq!(join_key().as_deref(), Some(ACME));
    // a relay of one's own with its key: that key; with none: no key at all
    rm_relay::set_key_override(Some("own-relay-key-0123456789".into()));
    assert_eq!(join_key().as_deref(), Some("own-relay-key-0123456789"));
    rm_relay::set_key_override(Some(String::new()));
    assert_eq!(join_key(), None);
    rm_relay::set_key_override(None);
    assert_eq!(join_key().as_deref(), Some(ACME));
}
