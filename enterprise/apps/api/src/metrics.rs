//! Counters and gauges in Prometheus' text format (`GET /metrics` on the ops listener). Gauges
//! that describe the database are read when scraped, so they are never stale.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Default)]
pub struct Metrics {
    counters: Mutex<BTreeMap<String, &'static AtomicU64>>,
    latency: Mutex<BTreeMap<String, (u64, f64)>>,
}

impl Metrics {
    fn counter(&self, key: String) -> &'static AtomicU64 {
        let mut m = self.counters.lock().unwrap();
        m.entry(key)
            .or_insert_with(|| Box::leak(Box::new(AtomicU64::new(0))))
    }

    /// `name{labels}` += 1
    pub fn inc(&self, name: &str, labels: &[(&str, &str)]) {
        self.counter(key(name, labels))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// A duration observed (count and sum, as a Prometheus summary without quantiles).
    pub fn observe(&self, name: &str, labels: &[(&str, &str)], seconds: f64) {
        let mut m = self.latency.lock().unwrap();
        let e = m.entry(key(name, labels)).or_insert((0, 0.0));
        e.0 += 1;
        e.1 += seconds;
    }

    pub fn render(&self, gauges: &[(String, f64)]) -> String {
        let mut out = String::new();
        for (k, v) in self.counters.lock().unwrap().iter() {
            out.push_str(&format!("{k} {}\n", v.load(Ordering::Relaxed)));
        }
        for (k, (n, sum)) in self.latency.lock().unwrap().iter() {
            let (name, labels) = k
                .split_once('{')
                .map(|(a, b)| (a.to_string(), format!("{{{b}")))
                .unwrap_or((k.clone(), String::new()));
            out.push_str(&format!(
                "{name}_count{labels} {n}\n{name}_sum{labels} {sum:.6}\n"
            ));
        }
        for (k, v) in gauges {
            out.push_str(&format!("{k} {v}\n"));
        }
        out
    }
}

fn key(name: &str, labels: &[(&str, &str)]) -> String {
    if labels.is_empty() {
        return name.to_string();
    }
    let l: Vec<String> = labels
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    format!("{name}{{{}}}", l.join(","))
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders_prometheus_text() {
        let m = super::Metrics::default();
        m.inc(
            "taro_logins_total",
            &[("method", "email"), ("result", "success")],
        );
        m.inc(
            "taro_logins_total",
            &[("method", "email"), ("result", "success")],
        );
        m.observe(
            "taro_http_request_seconds",
            &[("route", "/api/v1/me")],
            0.25,
        );
        let t = m.render(&[("taro_rentals{status=\"ready\"}".into(), 3.0)]);
        assert!(
            t.contains("taro_logins_total{method=\"email\",result=\"success\"} 2"),
            "{t}"
        );
        assert!(
            t.contains("taro_http_request_seconds_count{route=\"/api/v1/me\"} 1"),
            "{t}"
        );
        assert!(t.contains("taro_rentals{status=\"ready\"} 3"), "{t}");
    }
}
