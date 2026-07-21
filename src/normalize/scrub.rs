//! Fixed-pattern secret scrub over indexed text only (D-08).
//!
//! A closed pattern set - provider-prefixed keys (OpenAI `sk-`, GitHub
//! `ghp_`/`github_pat_`, AWS `AKIA`, Slack `xox*`, Stripe `sk_live_`/`sk_test_`/
//! `rk_live_`, Google `AIza`, GitLab `glpat-`, npm `npm_`), JWTs, PEM
//! private-key blocks, connection strings with embedded credentials,
//! `Authorization:` header values, and secret-named `KEY=VALUE` / `"key": value`
//! assignments (including quoted JSON keys, `pass`/`pwd`/`credential`, and any
//! compound `*_KEY`/`*-key`) - each replaced with the fixed marker `[REDACTED]`.
//! The assignment value must be quoted or a token of adequate length so ordinary
//! prose (`the big secret: it was a lie`) is left byte-identical. Applied to the
//! free-text indexed fields (Event `text` on indexed events, Artifact `content`)
//! only; NOT to `Mention.entity` (structural identifiers acceptance criterion 1
//! requires to equal the tool inputs), NOT to skeleton bodies (already blanked),
//! and NEVER to the archive (verbatim). The `regex` crate matches in guaranteed
//! linear time, so these patterns carry no catastrophic-backtracking (ReDoS)
//! exposure over hostile transcript text.
//!
//! Entropy-based detection is deferred to a later hardening pass (D-08): a lone
//! high-entropy token with no known prefix and no secret-named key (e.g. a bare
//! `deadbeefcafebabe0123456789abcdef01234567`) still leaks by design here.

use std::sync::OnceLock;

use regex::Regex;

const REDACTED: &str = "[REDACTED]";

/// Compiled pattern set with its replacement template. `[REDACTED]` alone
/// redacts the whole match; `${1}[REDACTED]` keeps a structural prefix (a header
/// name or assignment key) and redacts only the value.
fn patterns() -> &'static [(Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            vec![
                // PEM private-key block (any key type), whole block.
                (
                    Regex::new(
                        r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
                    )
                    .unwrap(),
                    REDACTED,
                ),
                // JWTs (three base64url segments).
                (
                    Regex::new(r"eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+").unwrap(),
                    REDACTED,
                ),
                // Provider-prefixed API keys / tokens.
                (
                    Regex::new(
                        r"(sk-[A-Za-z0-9]{16,}|[sr]k_(?:live|test)_[A-Za-z0-9]{10,}|gh[opsu]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|glpat-[A-Za-z0-9_-]{18,}|npm_[A-Za-z0-9]{20,}|AKIA[A-Z0-9]{16}|AIza[A-Za-z0-9_-]{20,}|xox[baprs]-[A-Za-z0-9-]{10,})",
                    )
                    .unwrap(),
                    REDACTED,
                ),
                // Connection strings with embedded credentials (scheme://user:pass@host).
                (
                    Regex::new(r"[a-zA-Z][a-zA-Z0-9+.\-]*://[^\s:/@]+:[^\s:/@]+@[^\s/]+").unwrap(),
                    REDACTED,
                ),
                // Authorization header values (keep the header name).
                (
                    Regex::new(
                        r"(?i)(authorization\s*:\s*)(?:bearer\s+|basic\s+)?[A-Za-z0-9._+/=\-]+",
                    )
                    .unwrap(),
                    "${1}[REDACTED]",
                ),
                // Secret-named KEY=VALUE / KEY: VALUE / "key": value assignments
                // (keep the key, quotes and separator; redact the value). The key
                // may be quoted (JSON fields) and covers pass/pwd/credential plus
                // any compound `*_KEY`/`*-key`. The value must be quoted or a
                // token of adequate length, so `secret: it was a lie` and other
                // short-word prose after a colon are left intact.
                (
                    Regex::new(
                        r#"(?i)(["']?\b\w*(?:secret|token|pass|pwd|credential|apikey|api[_-]key|access[_-]?key|private[_-]?key|[_-]key|auth[_-]?token)\w*\b["']?\s*[=:]\s*)("[^"]*"|'[^']*'|[A-Za-z0-9][A-Za-z0-9._/+=\-]{11,})"#,
                    )
                    .unwrap(),
                    "${1}[REDACTED]",
                ),
            ]
        })
        .as_slice()
}

/// Redact every fixed-pattern secret in `text`, returning the scrubbed copy.
pub fn scrub(text: &str) -> String {
    let mut out = text.to_string();
    for (re, replacement) in patterns() {
        out = re.replace_all(&out, *replacement).into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_provider_token() {
        let out = scrub("here is sk-ABCDEFGHIJKLMNOPqrstuvwx a key");
        assert!(!out.contains("sk-ABCDEFGHIJKLMNOPqrstuvwx"), "token gone: {out}");
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redacts_pem_private_key_block() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIBODESECRETLINES\n-----END RSA PRIVATE KEY-----";
        let out = scrub(&format!("key:\n{pem}\nend"));
        assert!(!out.contains("MIIBODESECRETLINES"), "pem body gone: {out}");
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redacts_connection_string_credentials() {
        let out = scrub("db at postgres://admin:hunter2@db.example.com/prod now");
        assert!(!out.contains("hunter2"), "password gone: {out}");
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redacts_authorization_header() {
        let out = scrub("Authorization: Bearer abc123def456ghi789");
        assert!(!out.contains("abc123def456ghi789"), "value gone: {out}");
        assert!(out.contains(REDACTED));
        assert!(out.to_lowercase().contains("authorization"), "header name kept");
    }

    #[test]
    fn redacts_secret_assignment_keeps_key() {
        let out = scrub("API_KEY=supersecretvalue0123456789");
        assert!(!out.contains("supersecretvalue0123456789"), "value gone: {out}");
        assert!(out.contains("API_KEY="), "key name kept: {out}");
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn leaves_ordinary_prose_unchanged() {
        let prose = "The quick brown fox jumps over the lazy dog near the river.";
        assert_eq!(scrub(prose), prose);
    }

    /// Assert `secret` is gone from the scrubbed text and the marker is present.
    fn assert_redacted(secret: &str) {
        let input = format!("prefix {secret} suffix");
        let out = scrub(&input);
        assert!(!out.contains(secret), "expected `{secret}` redacted, got: {out}");
        assert!(out.contains(REDACTED), "expected marker present, got: {out}");
    }

    #[test]
    fn redacts_additional_provider_prefixes() {
        // Stripe live/test/restricted, Google, GitLab, npm.
        assert_redacted("sk_live_51H8xYzABCDEFGHIJKLMNOPqr");
        assert_redacted("AIzaSyD-1234567890abcdefghijklmnopqrs_tu");
        assert_redacted("glpat-ABCDEFGHIJ1234567890");
        assert_redacted("npm_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789");
    }

    #[test]
    fn redacts_broader_secret_named_assignments() {
        // pass / credential / compound *_KEY keys the old set missed.
        assert_redacted("DB_PASS=hunter2secretpw");
        assert_redacted("MY_CREDENTIAL=abc123deadbeefcafe");
        assert_redacted("SIGNING_KEY=deadbeefcafe1234567");

        // `password:` with a token-shaped value.
        let out = scrub("password: hunter2secretvalue");
        assert!(!out.contains("hunter2secretvalue"), "value gone: {out}");
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redacts_quoted_json_key_assignment() {
        let out = scrub(r#"{"access_token": "ya29.a0AfrealTOKENvalue1234567890"}"#);
        assert!(
            !out.contains("ya29.a0AfrealTOKENvalue1234567890"),
            "quoted JSON token gone: {out}"
        );
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn leaves_secret_word_prose_intact() {
        // A secret-word followed by a colon in ordinary prose is not an
        // assignment: the short-word value must not trip the scrub.
        let prose = "the big secret: it was a lie";
        assert_eq!(scrub(prose), prose, "prose after `secret:` must be byte-identical");
    }

    #[test]
    fn leaves_code_reference_key_intact() {
        // Best-effort hard case: a code reference where the key names a secret
        // but the value is an expression, not a token. Comes out clean here.
        let code = r#"access_key = os.environ["AWS_ACCESS_KEY"]"#;
        assert_eq!(scrub(code), code, "code reference left intact: {}", scrub(code));
    }
}
