//! Native form submission. The caller must obtain explicit user consent (or be an explicit
//! headless tool call) before posting. Redirects are disabled so form data reaches only
//! the reviewed destination.

use std::time::Duration;

pub struct FormDestination {
    url: String,
}

impl FormDestination {
    pub fn new(value: &str) -> Result<Self, String> {
        let url = url::Url::parse(value).map_err(|e| format!("invalid form submission URL: {e}"))?;
        let local_http = url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1" | "[::1]"));
        if (url.scheme() != "https" && !local_http) || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err("form submission requires HTTPS (or HTTP on localhost), without credentials or a fragment".into());
        }
        Ok(Self { url: value.to_string() })
    }

    /// POST reviewed URL-encoded values. No redirect is followed and no response body is read.
    pub fn post_html(&self, body: &[u8]) -> Result<u16, String> {
        if body.len() > 8 * 1024 * 1024 {
            return Err("form data is larger than 8 MiB".into());
        }
        self.post(body, "application/x-www-form-urlencoded; charset=UTF-8")
    }

    /// POST a snapshot of the entire current PDF, including unsaved edits.
    pub fn post_pdf(&self, body: &[u8]) -> Result<u16, String> {
        self.post(body, "application/pdf")
    }

    pub fn post_fdf(&self, body: &[u8]) -> Result<u16, String> {
        self.post(body, "application/vnd.fdf")
    }

    pub fn post_xfdf(&self, body: &[u8]) -> Result<u16, String> {
        self.post(body, "application/vnd.adobe.xfdf")
    }

    fn post(&self, body: &[u8], content_type: &str) -> Result<u16, String> {
        let found = rustls_native_certs::load_native_certs();
        let certs: Vec<ureq::tls::Certificate<'static>> =
            found.certs.iter().map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned()).collect();
        if certs.is_empty() && self.url.starts_with("https:") {
            return Err("no trusted system certificates are available for form submission".into());
        }
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .max_redirects(0)
            .tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::new_with_certs(&certs)).build())
            .build()
            .new_agent();
        let response = agent.post(&self.url).header("Content-Type", content_type).send(body).map_err(|e| format!("form submission failed: {e}"))?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(format!("form server returned HTTP {status}"));
        }
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_requires_https_or_loopback() {
        assert!(FormDestination::new("https://forms.example/submit").is_ok());
        assert!(FormDestination::new("http://127.0.0.1:8080/submit").is_ok());
        for url in ["http://forms.example/submit", "file:///tmp/submit", "https://u:p@forms.example/submit", "https://forms.example/submit#FDF"] {
            assert!(FormDestination::new(url).is_err(), "{url}");
        }
    }
}
