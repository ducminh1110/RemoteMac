//! The mock provider: a handful of pretend Macs held in memory, for development, tests and demos
//! before a real provider is chosen. It behaves like a real one where it matters: provisioning
//! takes time and is polled, it can run out of capacity, Macs whose id starts with `mock-fail`
//! fail permanently and `mock-flaky` fail once then succeed, and it forgets everything on restart
//! (which reconciliation then notices). The portal labels it "Demo environment".

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::*;

struct Mac {
    r: Resource,
    reserved_by: Option<String>,
    provisioning: Option<(String, Instant)>,
    password: Option<String>,
    flaky_failed: bool,
}

pub struct Mock {
    macs: Mutex<HashMap<String, Mac>>,
    /// how long provisioning takes
    pub delay: Duration,
    pub relay: Option<String>,
}

impl Mock {
    /// `n` Macs in two regions (eu-west and us-east), M4 and M4 Pro.
    pub fn new(n: usize, delay: Duration) -> Mock {
        let mut macs = HashMap::new();
        for i in 1..=n {
            let pro = i % 3 == 0;
            let id = format!("mock-{i:03}");
            let r = Resource {
                external_id: id.clone(),
                display_name: format!("Mac mini {i:03}"),
                model: if pro {
                    "Mac mini (M4 Pro, 2024)"
                } else {
                    "Mac mini (M4, 2024)"
                }
                .into(),
                chip: if pro { "M4 Pro" } else { "M4" }.into(),
                cpu_cores: if pro { 14 } else { 10 },
                memory_gb: if pro { 48 } else { 24 },
                storage_gb: if pro { 1024 } else { 512 },
                region: if i % 2 == 0 { "us-east" } else { "eu-west" }.into(),
                state: ResourceState::Free,
            };
            macs.insert(
                id,
                Mac {
                    r,
                    reserved_by: None,
                    provisioning: None,
                    password: None,
                    flaky_failed: false,
                },
            );
        }
        Mock {
            macs: Mutex::new(macs),
            delay,
            relay: None,
        }
    }

    /// Add a Mac with a chosen id (tests: `mock-fail-1`, `mock-flaky-1`).
    pub fn add(&self, id: &str, chip: &str, region: &str) {
        let r = Resource {
            external_id: id.into(),
            display_name: id.into(),
            model: format!("Mac mini ({chip})"),
            chip: chip.into(),
            cpu_cores: 10,
            memory_gb: 24,
            storage_gb: 512,
            region: region.into(),
            state: ResourceState::Free,
        };
        self.macs.lock().unwrap().insert(
            id.into(),
            Mac {
                r,
                reserved_by: None,
                provisioning: None,
                password: None,
                flaky_failed: false,
            },
        );
    }

    /// Take a Mac out of service as a real provider might (tests and demos of incidents).
    pub fn set_offline(&self, id: &str) {
        if let Some(m) = self.macs.lock().unwrap().get_mut(id) {
            m.r.state = ResourceState::Offline;
        }
    }

    fn with<T>(&self, id: &str, f: impl FnOnce(&mut Mac) -> Result<T>) -> Result<T> {
        let mut m = self.macs.lock().unwrap();
        let mac = m.get_mut(id).ok_or(Error {
            kind: ErrorKind::NotFound,
            message: format!("no Mac {id}"),
        })?;
        f(mac)
    }
}

/// A 9-digit Taro ID for a mock Mac (900 000 000 + its number).
fn taro_id(external_id: &str) -> String {
    let n: u64 = external_id
        .bytes()
        .fold(0u64, |h, b| h.wrapping_mul(131).wrapping_add(b as u64))
        % 99_999_999;
    format!("9{n:08}")
}

#[async_trait]
impl Provider for Mock {
    fn kind(&self) -> &'static str {
        "mock"
    }

    fn is_simulated(&self) -> bool {
        true
    }

    async fn list_capabilities(&self) -> Result<Capabilities> {
        let m = self.macs.lock().unwrap();
        let mut regions: Vec<String> = m.values().map(|m| m.r.region.clone()).collect();
        let mut chips: Vec<String> = m.values().map(|m| m.r.chip.clone()).collect();
        regions.sort();
        regions.dedup();
        chips.sort();
        chips.dedup();
        Ok(Capabilities {
            regions,
            chips,
            start_stop: false,
            idempotency_keys: true,
        })
    }

    async fn list_inventory(&self) -> Result<Vec<Resource>> {
        let mut v: Vec<Resource> = self
            .macs
            .lock()
            .unwrap()
            .values()
            .map(|m| m.r.clone())
            .collect();
        v.sort_by(|a, b| a.external_id.cmp(&b.external_id));
        Ok(v)
    }

    async fn get_resource(&self, external_id: &str) -> Result<Resource> {
        self.with(external_id, |m| Ok(m.r.clone()))
    }

    async fn reserve_resource(&self, external_id: &str, key: &str) -> Result<()> {
        self.with(external_id, |m| match (&m.reserved_by, m.r.state) {
            (Some(k), _) if k == key => Ok(()),
            (None, ResourceState::Free) => {
                m.reserved_by = Some(key.into());
                m.r.state = ResourceState::Reserved;
                Ok(())
            }
            (_, ResourceState::Offline | ResourceState::Maintenance) => {
                Err(Error::retryable(format!("{external_id} is not in service")))
            }
            _ => Err(Error {
                kind: ErrorKind::NoCapacity,
                message: format!("{external_id} is taken"),
            }),
        })
    }

    async fn provision_resource(&self, external_id: &str, key: &str) -> Result<String> {
        let delay = self.delay;
        self.with(external_id, |m| {
            if m.reserved_by.as_deref() != Some(key) {
                return Err(Error::permanent(format!(
                    "{external_id} is not reserved for this request"
                )));
            }
            // the same key again: the same provisioning, not a second one
            if let Some((r, _)) = &m.provisioning {
                return Ok(r.clone());
            }
            // unique per Mac: two Macs provisioned under one key must never share a reference
            let r = format!("prov-{external_id}-{key}");
            m.provisioning = Some((r.clone(), Instant::now() + delay));
            m.r.state = ResourceState::Provisioning;
            Ok(r)
        })
    }

    async fn get_provisioning_status(&self, external_ref: &str) -> Result<Provisioning> {
        let mut all = self.macs.lock().unwrap();
        let Some(m) = all.values_mut().find(|m| {
            m.provisioning
                .as_ref()
                .is_some_and(|(r, _)| r == external_ref)
        }) else {
            return Err(Error {
                kind: ErrorKind::NotFound,
                message: format!("no provisioning {external_ref}"),
            });
        };
        let ready_at = m.provisioning.as_ref().unwrap().1;
        if Instant::now() < ready_at {
            return Ok(Provisioning::Pending);
        }
        if m.r.external_id.starts_with("mock-fail") {
            return Ok(Provisioning::Failed {
                retryable: false,
                message: "the Mac did not pass its health check (simulated)".into(),
            });
        }
        if m.r.external_id.starts_with("mock-flaky") && !m.flaky_failed {
            m.flaky_failed = true;
            m.provisioning = None;
            return Ok(Provisioning::Failed {
                retryable: true,
                message: "the provider timed out (simulated, once)".into(),
            });
        }
        m.r.state = ResourceState::InUse;
        if m.password.is_none() {
            m.password = Some(crate::crypto::token(12));
        }
        Ok(Provisioning::Ready)
    }

    async fn release_resource(&self, external_id: &str, _key: &str) -> Result<()> {
        self.with(external_id, |m| {
            m.reserved_by = None;
            m.provisioning = None;
            m.password = None;
            if m.r.state != ResourceState::Offline {
                m.r.state = ResourceState::Free;
            }
            Ok(())
        })
    }

    async fn connection(&self, external_id: &str) -> Result<Connection> {
        let relay = self.relay.clone();
        self.with(external_id, |m| match (&m.password, m.r.state) {
            (Some(p), ResourceState::InUse) => Ok(Connection {
                taro_id: taro_id(external_id),
                relay,
                password: p.clone(),
            }),
            _ => Err(Error::permanent(format!(
                "{external_id} is not ready to connect"
            ))),
        })
    }

    async fn health_check(&self) -> Result<Health> {
        let m = self.macs.lock().unwrap();
        let offline = m
            .values()
            .filter(|m| m.r.state == ResourceState::Offline)
            .count();
        Ok(if offline == 0 {
            Health::Healthy
        } else if offline < m.len() {
            Health::Degraded
        } else {
            Health::Down
        })
    }

    async fn safe_diagnostics(&self, external_id: &str) -> Result<serde_json::Value> {
        self.with(external_id, |m| Ok(serde_json::json!({ "provider": "mock", "state": m.r.state.key(), "region": m.r.region, "simulated": true })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reserve_provision_connect_release() {
        let p = Mock::new(2, Duration::from_millis(30));
        let inv = p.list_inventory().await.unwrap();
        assert_eq!(inv.len(), 2);
        let id = &inv[0].external_id;
        p.reserve_resource(id, "job-1").await.unwrap();
        p.reserve_resource(id, "job-1").await.unwrap(); // the same key again: fine
        assert_eq!(
            p.reserve_resource(id, "job-2").await.unwrap_err().kind,
            ErrorKind::NoCapacity
        );
        let r = p.provision_resource(id, "job-1").await.unwrap();
        assert_eq!(
            p.provision_resource(id, "job-1").await.unwrap(),
            r,
            "idempotent"
        );
        assert_eq!(
            p.get_provisioning_status(&r).await.unwrap(),
            Provisioning::Pending
        );
        assert!(p.connection(id).await.is_err(), "not before it is ready");
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(
            p.get_provisioning_status(&r).await.unwrap(),
            Provisioning::Ready
        );
        let c = p.connection(id).await.unwrap();
        assert_eq!(c.taro_id.len(), 9);
        assert!(
            !format!("{c:?}").contains(&c.password),
            "the password never prints"
        );
        p.release_resource(id, "job-1").await.unwrap();
        assert_eq!(p.get_resource(id).await.unwrap().state, ResourceState::Free);
    }

    #[tokio::test]
    async fn failures_as_configured() {
        let p = Mock::new(0, Duration::ZERO);
        p.add("mock-fail-1", "M4", "eu-west");
        p.add("mock-flaky-1", "M4", "eu-west");
        p.reserve_resource("mock-fail-1", "k").await.unwrap();
        let r = p.provision_resource("mock-fail-1", "k").await.unwrap();
        assert!(matches!(
            p.get_provisioning_status(&r).await.unwrap(),
            Provisioning::Failed {
                retryable: false,
                ..
            }
        ));
        p.reserve_resource("mock-flaky-1", "k").await.unwrap();
        let r = p.provision_resource("mock-flaky-1", "k").await.unwrap();
        assert!(matches!(
            p.get_provisioning_status(&r).await.unwrap(),
            Provisioning::Failed {
                retryable: true,
                ..
            }
        ));
        let r = p.provision_resource("mock-flaky-1", "k").await.unwrap();
        assert_eq!(
            p.get_provisioning_status(&r).await.unwrap(),
            Provisioning::Ready,
            "the retry works"
        );
        p.set_offline("mock-fail-1");
        assert_eq!(p.health_check().await.unwrap(), Health::Degraded);
    }
}
