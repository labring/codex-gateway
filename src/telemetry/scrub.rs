//! Best-effort secret redaction for text exported to Langfuse.
//!
//! The agent works inside a sandbox whose environment contains credentials
//! (e.g. `GITHUB_TOKEN`); command output echoing one of them must not leak
//! into telemetry. This scrubs well-known token shapes.

/// Prefixes that mark the start of a credential. A match must sit on a word
/// boundary and be followed by a long-enough token tail to be redacted.
const SECRET_PREFIXES: &[&str] = &[
    "github_pat_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "glpat-",
    "sk-",
    "pk-lf-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxr-",
    "xoxs-",
];

const BEARER_PREFIX: &str = "Bearer ";
const MIN_TAIL: usize = 8;
const REDACTED: &str = "[REDACTED]";

/// Maximum length for input/output payloads exported to Langfuse.
pub const MAX_TEXT: usize = 8 * 1024;

pub fn scrub(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut index = 0;

    while index < text.len() {
        let rest = &text[index..];
        if let Some(found) = match_secret(rest, boundary_before(text, index)) {
            result.push_str(&rest[..found.prefix_len]);
            result.push_str(REDACTED);
            index += found.prefix_len + found.tail_len;
            continue;
        }

        let ch = rest.chars().next().expect("non-empty rest");
        result.push(ch);
        index += ch.len_utf8();
    }

    result
}

/// Scrub and cap a payload for export.
pub fn scrub_and_truncate(text: &str) -> String {
    let scrubbed = scrub(text);
    if scrubbed.chars().count() <= MAX_TEXT {
        return scrubbed;
    }
    let truncated: String = scrubbed.chars().take(MAX_TEXT).collect();
    format!("{truncated}… [truncated]")
}

struct SecretMatch {
    prefix_len: usize,
    tail_len: usize,
}

fn match_secret(rest: &str, at_boundary: bool) -> Option<SecretMatch> {
    if let Some(tail_text) = rest.strip_prefix(BEARER_PREFIX) {
        let tail = token_tail_len(tail_text);
        if tail >= MIN_TAIL {
            return Some(SecretMatch {
                prefix_len: BEARER_PREFIX.len(),
                tail_len: tail,
            });
        }
    }

    if !at_boundary {
        return None;
    }
    for prefix in SECRET_PREFIXES {
        if let Some(tail_text) = rest.strip_prefix(prefix) {
            let tail = token_tail_len(tail_text);
            if tail >= MIN_TAIL {
                return Some(SecretMatch {
                    prefix_len: prefix.len(),
                    tail_len: tail,
                });
            }
        }
    }
    None
}

fn token_tail_len(rest: &str) -> usize {
    rest.bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        .count()
}

fn boundary_before(text: &str, index: usize) -> bool {
    text[..index]
        .chars()
        .next_back()
        .is_none_or(|ch| !ch.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_github_tokens() {
        let input = "cloning https://x-access-token:ghp_abcdef1234567890@github.com/o/r";
        let output = scrub(input);
        assert_eq!(
            output,
            "cloning https://x-access-token:ghp_[REDACTED]@github.com/o/r"
        );
    }

    #[test]
    fn redacts_bearer_and_env_assignment() {
        assert_eq!(
            scrub("Authorization: Bearer abc123def456xyz"),
            "Authorization: Bearer [REDACTED]"
        );
        assert_eq!(
            scrub("GITHUB_TOKEN=github_pat_11ABCDEF0123456789"),
            "GITHUB_TOKEN=github_pat_[REDACTED]"
        );
    }

    #[test]
    fn keeps_non_secret_text() {
        assert_eq!(scrub("run task-12345678 now"), "run task-12345678 now");
        assert_eq!(scrub("short sk-1 stays"), "short sk-1 stays");
        assert_eq!(scrub("no secrets here"), "no secrets here");
    }

    #[test]
    fn requires_word_boundary_for_prefixes() {
        assert_eq!(scrub("crisk-detection-9000x"), "crisk-detection-9000x");
        assert_eq!(scrub("use sk-abcdef12345678"), "use sk-[REDACTED]");
    }

    #[test]
    fn truncates_long_payloads() {
        let long = "a".repeat(MAX_TEXT + 100);
        let output = scrub_and_truncate(&long);
        assert!(output.ends_with("… [truncated]"));
        assert!(output.chars().count() < MAX_TEXT + 20);
    }
}
