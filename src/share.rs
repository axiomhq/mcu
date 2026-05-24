//! Build deep-link URLs for the Axiom web UI and open them in the system browser.
//!
//! Axiom's web UI accepts an `initForm` query-string parameter whose value is a
//! URL-encoded JSON object describing the query to pre-fill. The format is not
//! publicly documented but is used by `axiomhq/symphony`, `axiomhq/anton`, and
//! several other internal tools, so it is stable in practice.
//!
//! Example URL:
//! ```text
//! https://app.axiom.co/{org_id}/query?initForm=%7B%22apl%22%3A%22...%22%7D
//! ```
//!
//! Including `metricsDataset` in the JSON lands the user in the metrics
//! explorer tab; omitting it opens the APL query editor.

use std::io;
use std::process::{Command, Stdio};

/// Build a URL that opens the Axiom web UI with `query` pre-filled.
///
/// `deployment_url` is the API URL from `~/.axiom.toml` (e.g.
/// `https://api.axiom.co`); the host is rewritten to the matching app host
/// (`https://app.axiom.co`) by swapping the leading `api.` segment.
pub fn build_axiom_url(
    deployment_url: &str,
    org_id: &str,
    query: &str,
    dataset: Option<&str>,
) -> String {
    let host = app_host(deployment_url);
    let json = build_init_form_json(query, dataset);
    let encoded = urlencode(&json);
    format!("{host}/{org_id}/query?initForm={encoded}")
}

/// Spawn the OS's default URL opener. Returns as soon as the child is spawned;
/// we don't wait for it because the browser launch is fire-and-forget.
pub fn open_in_browser(url: &str) -> io::Result<()> {
    let mut cmd = opener_command();
    cmd.arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn opener_command() -> Command {
    Command::new("open")
}

#[cfg(target_os = "linux")]
fn opener_command() -> Command {
    Command::new("xdg-open")
}

#[cfg(target_os = "windows")]
fn opener_command() -> Command {
    // `cmd /C start "" <url>` — the empty string is `start`'s title argument,
    // required because `start "url"` interprets the URL as the window title.
    let mut c = Command::new("cmd");
    c.args(["/C", "start", ""]);
    c
}

fn app_host(deployment_url: &str) -> String {
    let trimmed = deployment_url.trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix("https://api.") {
        format!("https://app.{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://api.") {
        format!("http://app.{rest}")
    } else {
        trimmed.to_string()
    }
}

fn build_init_form_json(query: &str, dataset: Option<&str>) -> String {
    let apl = json_escape(query);
    match dataset {
        Some(ds) if !ds.is_empty() => {
            let ds = json_escape(ds);
            format!(r#"{{"apl":"{apl}","metricsDataset":"{ds}"}}"#)
        }
        _ => format!(r#"{{"apl":"{apl}"}}"#),
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_api_host_to_app() {
        assert_eq!(app_host("https://api.axiom.co"), "https://app.axiom.co");
        assert_eq!(app_host("https://api.axiom.co/"), "https://app.axiom.co");
        assert_eq!(
            app_host("http://api.example.test"),
            "http://app.example.test"
        );
    }

    #[test]
    fn leaves_non_api_host_unchanged() {
        assert_eq!(
            app_host("https://staging.example.com"),
            "https://staging.example.com"
        );
    }

    #[test]
    fn json_escapes_quotes_and_newlines() {
        assert_eq!(json_escape(r#"he said "hi""#), r#"he said \"hi\""#);
        assert_eq!(json_escape("a\nb\tc"), "a\\nb\\tc");
        assert_eq!(json_escape("a\\b"), "a\\\\b");
    }

    #[test]
    fn json_escapes_control_chars() {
        // 0x01 is below the printable range and not one of the named escapes.
        assert_eq!(json_escape("\x01"), "\\u0001");
    }

    #[test]
    fn urlencodes_reserved_chars() {
        assert_eq!(urlencode("{\"apl\":\"x\"}"), "%7B%22apl%22%3A%22x%22%7D");
        assert_eq!(urlencode("a b c"), "a%20b%20c");
    }

    #[test]
    fn builds_url_with_dataset() {
        let url = build_axiom_url(
            "https://api.axiom.co",
            "org-123",
            "`my-ds`:cpu_usage[1h..]",
            Some("my-ds"),
        );
        assert!(url.starts_with("https://app.axiom.co/org-123/query?initForm="));
        // Decoded payload should contain both fields.
        let payload = url.split("initForm=").nth(1).unwrap();
        let decoded = urldecode_for_test(payload);
        assert!(decoded.contains(r#""apl":""#));
        assert!(decoded.contains(r#""metricsDataset":"my-ds""#));
    }

    #[test]
    fn builds_url_without_dataset() {
        let url = build_axiom_url("https://api.axiom.co", "org", "foo", None);
        let payload = url.split("initForm=").nth(1).unwrap();
        let decoded = urldecode_for_test(payload);
        assert_eq!(decoded, r#"{"apl":"foo"}"#);
    }

    #[test]
    fn empty_dataset_is_treated_as_none() {
        let url = build_axiom_url("https://api.axiom.co", "org", "foo", Some(""));
        let payload = url.split("initForm=").nth(1).unwrap();
        let decoded = urldecode_for_test(payload);
        assert!(!decoded.contains("metricsDataset"));
    }

    fn urldecode_for_test(s: &str) -> String {
        let mut out = Vec::new();
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hi = (bytes[i + 1] as char).to_digit(16).unwrap() as u8;
                let lo = (bytes[i + 2] as char).to_digit(16).unwrap() as u8;
                out.push(hi * 16 + lo);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }
}
