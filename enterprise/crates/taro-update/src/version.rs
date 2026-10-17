//! Versions as `major.minor.patch[-pre]` (a leading `v` or `enterprise-v` allowed).

use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub n: [u64; 3],
    pub pre: Option<String>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix("enterprise-").unwrap_or(s);
        let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p.to_string())),
            Some(_) => return None,
            None => (s, None),
        };
        let mut n = [0u64; 3];
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        for (i, p) in parts.iter().enumerate() {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || p.len() > 9 {
                return None;
            }
            n[i] = p.parse().ok()?;
        }
        Some(Version { n, pre })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        self.n.cmp(&o.n).then_with(|| match (&self.pre, &o.pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => a.cmp(b),
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.n[0], self.n[1], self.n[2])?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        Ok(())
    }
}

/// a < b (both must parse).
pub fn lt(a: &str, b: &str) -> Option<bool> {
    Some(Version::parse(a)? < Version::parse(b)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn order() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("0.2.0") > v("0.1.9"));
        assert!(v("enterprise-v0.2.0") == v("0.2.0"));
        assert!(v("1.0.0-beta.1") < v("1.0.0"));
        assert!(
            Version::parse("1.0").is_none()
                && Version::parse("1.0.x").is_none()
                && Version::parse("1.0.0-").is_none()
        );
    }
}
