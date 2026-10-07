//! Mac IDs handed out by the relay. A Mac asks with its owner secret (random, kept on the Mac)
//! and, when it had one, the ID it wants back:
//!
//!   Mac   -> `{"claim_id":"<owner secret>","want":"123456789","key":"..."}`
//!   relay -> `ID 123456789`
//!
//! An ID belongs to the first owner that got it; asked for by another owner, a fresh one is
//! given instead, and an agent may only wait under a claimed ID with its owner's secret. So two
//! Macs on one relay never share an ID, and nobody can take over another Mac's ID there.
//! Kept in a JSON file (`RM_RELAY_IDS`, else `$STATE_DIRECTORY/ids.json` under systemd), else
//! only in memory.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// IDs one relay hands out at most (a full table refuses new owners).
pub const MAX_IDS: usize = 1_000_000;

#[derive(Debug, Deserialize)]
pub struct Claim {
    pub claim_id: String,
    #[serde(default)]
    pub want: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Table {
    /// id -> owner secret
    owners: HashMap<String, String>,
}

pub struct Ids {
    path: Option<PathBuf>,
    t: Table,
    by_owner: HashMap<String, String>,
    seed: u64,
}

pub fn valid_id(s: &str) -> bool {
    s.len() == 9 && s.bytes().all(|b| b.is_ascii_digit()) && !s.starts_with('0')
}

pub fn valid_owner(s: &str) -> bool {
    (32..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Where the table is kept: `RM_RELAY_IDS`, else systemd's state directory, else nowhere.
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("RM_RELAY_IDS")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("STATE_DIRECTORY").map(|d| PathBuf::from(d).join("ids.json")))
}

impl Ids {
    pub fn open(path: Option<PathBuf>) -> Ids {
        let t: Table = path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let by_owner = t.owners.iter().map(|(id, o)| (o.clone(), id.clone())).collect();
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) ^ ((std::process::id() as u64) << 32);
        Ids { path, t, by_owner, seed: seed | 1 }
    }

    pub fn len(&self) -> usize {
        self.t.owners.len()
    }

    pub fn is_empty(&self) -> bool {
        self.t.owners.is_empty()
    }

    /// The ID for `owner`: `want` when it is free or already this owner's, else the one this
    /// owner already has, else a new one. None: the table is full.
    pub fn claim(&mut self, owner: &str, want: Option<&str>) -> Option<String> {
        if let Some(w) = want.filter(|w| valid_id(w)) {
            match self.t.owners.get(w) {
                Some(o) if o == owner => return Some(w.to_string()),
                // free: it is this owner's now (an ID it had before moves there)
                None => {
                    if let Some(old) = self.by_owner.get(owner).cloned() {
                        self.t.owners.remove(&old);
                    }
                    return self.give(owner, w.to_string());
                }
                Some(_) => {}
            }
        }
        if let Some(id) = self.by_owner.get(owner) {
            return Some(id.clone());
        }
        if self.t.owners.len() >= MAX_IDS {
            return None;
        }
        loop {
            let id = (100_000_000 + self.next() % 900_000_000).to_string();
            if !self.t.owners.contains_key(&id) {
                return self.give(owner, id);
            }
        }
    }

    /// May an agent with `owner` (if any) wait under `id`? Unclaimed IDs are open to anyone.
    pub fn may_use(&self, id: &str, owner: Option<&str>) -> bool {
        match self.t.owners.get(id) {
            None => true,
            Some(o) => owner.is_some_and(|w| crate::constant_time_eq(o, w)),
        }
    }

    fn give(&mut self, owner: &str, id: String) -> Option<String> {
        self.t.owners.insert(id.clone(), owner.to_string());
        self.by_owner.insert(owner.to_string(), id.clone());
        self.save();
        Some(id)
    }

    fn next(&mut self) -> u64 {
        // xorshift64*: IDs need to be spread, not secret (the password protects the Mac)
        self.seed ^= self.seed >> 12;
        self.seed ^= self.seed << 25;
        self.seed ^= self.seed >> 27;
        self.seed.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn save(&self) {
        let Some(p) = &self.path else { return };
        let tmp = p.with_extension("tmp");
        let ok = serde_json::to_vec(&self.t).ok().is_some_and(|b| std::fs::write(&tmp, b).is_ok()) && std::fs::rename(&tmp, p).is_ok();
        if !ok {
            eprintln!("rm-relay: could not save the ID table to {}", p.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn ids_belong_to_their_owner() {
        let dir = std::env::temp_dir().join(format!("rm-ids-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ids.json");
        let _ = std::fs::remove_file(&path);
        let mut ids = Ids::open(Some(path.clone()));
        let a = ids.claim(A, None).unwrap();
        assert!(valid_id(&a), "{a}");
        assert_eq!(ids.claim(A, None).unwrap(), a, "same owner, same ID");
        assert_eq!(ids.claim(A, Some(&a)).unwrap(), a);
        let b = ids.claim(B, Some(&a)).unwrap();
        assert_ne!(b, a, "another owner never gets a taken ID");
        assert!(ids.may_use(&a, Some(A)) && !ids.may_use(&a, Some(B)) && !ids.may_use(&a, None));
        assert!(ids.may_use(if a == "999999999" || b == "999999999" { "999999998" } else { "999999999" }, None), "unclaimed IDs are open");
        // kept across restarts
        let mut again = Ids::open(Some(path.clone()));
        assert_eq!(again.claim(A, None).unwrap(), a);
        assert_eq!(again.claim(B, None).unwrap(), b);
        // a wanted free ID moves the owner there
        let c = again.claim(B, Some("987654321")).unwrap();
        assert_eq!(c, "987654321");
        assert!(again.may_use(&b, None), "the old one is free again");
        assert_eq!(again.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!valid_owner("short") && valid_owner(A));
        assert!(!valid_id("012345678") && !valid_id("12345678") && valid_id("123456789"));
    }
}
