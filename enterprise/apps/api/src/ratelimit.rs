//! Fixed-window rate limits per key (an IP, an email address), in memory. One server process is
//! the deployment model today (ADR-001); several would need a shared store (Planned).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Limiter {
    windows: Mutex<HashMap<(String, String), (Instant, u32)>>,
}

impl Limiter {
    /// True while `key` has made fewer than `limit` calls to `bucket` in the current `window`.
    pub fn allow(&self, bucket: &str, key: &str, limit: u32, window: Duration) -> bool {
        let now = Instant::now();
        let mut w = self.windows.lock().unwrap();
        if w.len() > 100_000 {
            w.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(3600));
        }
        let e = w
            .entry((bucket.to_string(), key.to_string()))
            .or_insert((now, 0));
        if now.duration_since(e.0) >= window {
            *e = (now, 0);
        }
        e.1 += 1;
        e.1 <= limit
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn limits_per_key_and_window() {
        let l = super::Limiter::default();
        let w = std::time::Duration::from_millis(80);
        assert!((0..3).all(|_| l.allow("login", "1.2.3.4", 3, w)));
        assert!(!l.allow("login", "1.2.3.4", 3, w));
        assert!(l.allow("login", "5.6.7.8", 3, w), "another key");
        std::thread::sleep(std::time::Duration::from_millis(90));
        assert!(l.allow("login", "1.2.3.4", 3, w), "a new window");
    }
}
