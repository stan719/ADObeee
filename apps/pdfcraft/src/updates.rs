//! Asks GitHub for the latest PdfCraft release (Help ▸ Check for updates, issue #28).

use std::time::Duration;

use pdfcraft_ui_egui::updates::Release;

const LATEST: &str = "https://api.github.com/repos/storytold/pdfcraft/releases/latest";

/// The latest release. The answer is untrusted: its size is capped, and only a page under
/// the canonical PdfCraft release and asset URLs are accepted. Packages are opened in the browser.
pub fn latest_release() -> Result<Release, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        // The only allowed request is the exact GitHub API endpoint above. Never follow a
        // redirect to another host, path, or scheme.
        .max_redirects(0)
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(os_roots()?).build())
        .build()
        .new_agent();
    let mut response = agent
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", concat!("PdfCraft/", env!("CARGO_PKG_VERSION")))
        // Read error statuses ourselves: a 403 from GitHub usually means its anonymous
        // rate limit ran out, and the retry time rides along in a response header.
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .map_err(|e| format!("couldn't reach GitHub ({e})"))?;
    let status = response.status().as_u16();
    if status == 403 {
        let reset = response.headers().get("x-ratelimit-reset").and_then(|v| v.to_str().ok());
        return Err(rate_limited(reset, now_unix()));
    }
    if !(200..300).contains(&status) {
        return Err(format!("couldn't reach GitHub (http status: {status})"));
    }
    let body = response.body_mut().with_config().limit(1 << 20).read_to_string().map_err(|e| format!("unreadable answer ({e})"))?;
    pdfcraft_ui_egui::updates::parse_release(&body)
}

/// The certificate authorities the operating system trusts.
fn os_roots() -> Result<ureq::tls::RootCerts, String> {
    let found = rustls_native_certs::load_native_certs();
    let certs: Vec<ureq::tls::Certificate<'static>> = found.certs.iter().map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned()).collect();
    if certs.is_empty() {
        return Err("no trusted certificates found on this system".into());
    }
    Ok(ureq::tls::RootCerts::new_with_certs(&certs))
}

/// The current Unix time in seconds (0 when the clock gives nothing usable).
fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// What to tell the user when GitHub answers 403: its anonymous quota (60 checks an hour,
/// shared by everyone behind the same address) ran out. `reset` is the `x-ratelimit-reset`
/// header, the Unix time the quota refills; `now` is the current Unix time. Anything
/// surprising (no header, not a number, already past) falls back to asking for patience.
fn rate_limited(reset: Option<&str>, now: u64) -> String {
    const FALLBACK: &str = "GitHub is limiting update checks for now (shared hourly quota); try again later";
    let wait = reset.and_then(|r| r.trim().parse::<u64>().ok()).and_then(|reset| reset.checked_sub(now)).filter(|&wait| wait > 0);
    match wait {
        Some(wait) => {
            let minutes = wait.div_ceil(60).max(1);
            let plural = if minutes == 1 { "" } else { "s" };
            format!("GitHub is limiting update checks for now (shared hourly quota); try again in about {minutes} minute{plural}")
        }
        None => FALLBACK.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response() -> &'static str {
        r#"{"tag_name":"v0.7.1","html_url":"https://github.com/storytold/pdfcraft/releases/tag/v0.7.1","body":"Fixes and improvements","draft":false,"prerelease":false,"assets":[{"name":"pdfcraft-0.7.1-linux-x86_64.AppImage","size":1234,"browser_download_url":"https://github.com/storytold/pdfcraft/releases/download/v0.7.1/pdfcraft-0.7.1-linux-x86_64.AppImage","digest":"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}]}"#
    }

    #[test]
    fn answers_are_read_and_only_our_release_pages_are_offered() {
        let release = pdfcraft_ui_egui::updates::parse_release(response()).unwrap();
        assert_eq!(release.version, "v0.7.1");
        assert_eq!(release.url, "https://github.com/storytold/pdfcraft/releases/tag/v0.7.1");
        assert_eq!(release.notes, "Fixes and improvements");
        assert_eq!(release.assets.len(), 1);
        assert_eq!(release.assets[0].name, "pdfcraft-0.7.1-linux-x86_64.AppImage");
        assert_eq!(release.assets[0].digest.as_deref(), Some("sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"));
    }

    #[test]
    fn github_error_and_invalid_release_fixtures_are_rejected_without_network() {
        for body in [
            r#"{"message":"Not Found","documentation_url":"https://docs.github.com"}"#,
            r#"{"message":"API rate limit exceeded"}"#,
            "<html>upstream error</html>",
            r#"{"tag_name":"v0.7.1","html_url":"https://example.com/release","body":"Notes","draft":false,"prerelease":false,"assets":[]}"#,
        ] {
            assert!(pdfcraft_ui_egui::updates::parse_release(body).is_err(), "{body}");
        }
    }

    #[test]
    fn rate_limit_messages_name_the_wait() {
        let limited = "GitHub is limiting update checks for now (shared hourly quota); try again in about 20 minutes";
        assert_eq!(rate_limited(Some("1800001200"), 1800000000), limited);
        // Rounds up: 61 seconds out reads as 2 minutes.
        assert_eq!(
            rate_limited(Some("1800000061"), 1800000000),
            "GitHub is limiting update checks for now (shared hourly quota); try again in about 2 minutes"
        );
        assert_eq!(
            rate_limited(Some("1800000060"), 1800000000),
            "GitHub is limiting update checks for now (shared hourly quota); try again in about 1 minute"
        );
        // No header, not a number, or already past: ask for patience instead of a time.
        let later = "GitHub is limiting update checks for now (shared hourly quota); try again later";
        for bad in [None, Some(""), Some("soon"), Some("1799999999"), Some("1800000000")] {
            assert_eq!(rate_limited(bad, 1800000000), later);
        }
    }
}
