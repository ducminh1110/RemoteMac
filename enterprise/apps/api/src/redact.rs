//! Takes credentials out of text before it is logged or shown: API keys, tokens, signatures,
//! passwords in URLs, bearer headers.

/// Prefixes of credentials that must never appear in logs.
const PREFIXES: &[&str] = &[
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "rk_test_",
    "whsec_",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "xoxb-",
    "AKIA",
];

pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'outer: while !rest.is_empty() {
        for p in PREFIXES {
            if rest.starts_with(p) {
                out.push_str(p);
                out.push_str("[redacted]");
                let n = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                    .unwrap_or(rest.len());
                rest = &rest[n..];
                continue 'outer;
            }
        }
        // "Bearer <token>" and "://user:password@" in URLs
        if let Some(after) = rest.strip_prefix("Bearer ") {
            out.push_str("Bearer [redacted]");
            let n = after
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .unwrap_or(after.len());
            rest = &after[n..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("://") {
            if let Some(at) = after.find('@') {
                let creds = &after[..at];
                if creds.contains(':')
                    && !creds.contains('/')
                    && !creds.contains(char::is_whitespace)
                {
                    out.push_str("://[redacted]@");
                    rest = &after[at + 1..];
                    continue;
                }
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn credentials_are_taken_out() {
        assert_eq!(
            redact("key sk_test_abc123XYZ done"),
            "key sk_test_[redacted] done"
        );
        assert_eq!(
            redact("Authorization: Bearer eyJhbGci.x.y next"),
            "Authorization: Bearer [redacted] next"
        );
        assert_eq!(
            redact("postgres://taro:hunter2@db:5432/taro"),
            "postgres://[redacted]@db:5432/taro"
        );
        assert_eq!(
            redact("whsec_123 and github_pat_11ABC"),
            "whsec_[redacted] and github_pat_[redacted]"
        );
        assert_eq!(
            redact("plain text, nothing here: a@b.c"),
            "plain text, nothing here: a@b.c"
        );
        assert_eq!(redact("héllo"), "héllo");
    }
}
