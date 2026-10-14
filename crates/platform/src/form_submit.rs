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

    /// Submit HTML form pairs in the URL's query using HTTP GET, preserving an existing query.
    /// The reviewed destination is still the only destination: redirects stay disabled.
    pub fn get_html(&self, body: &[u8]) -> Result<u16, String> {
        let encoded = std::str::from_utf8(body).map_err(|_| "form data is not UTF-8".to_string())?;
        if body.len() > 8 * 1024 * 1024 {
            return Err("form data is larger than 8 MiB".into());
        }
        let mut url = url::Url::parse(&self.url).map_err(|e| format!("invalid form submission URL: {e}"))?;
        let query = match url.query() {
            Some(existing) if !existing.is_empty() => format!("{existing}&{encoded}"),
            _ => encoded.to_string(),
        };
        url.set_query(Some(&query));
        self.send(&url, None)
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
        let url = url::Url::parse(&self.url).map_err(|e| format!("invalid form submission URL: {e}"))?;
        self.send(&url, Some((body, content_type)))
    }

    fn send(&self, url: &url::Url, post: Option<(&[u8], &str)>) -> Result<u16, String> {
        let found = rustls_native_certs::load_native_certs();
        let certs: Vec<ureq::tls::Certificate<'static>> =
            found.certs.iter().map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned()).collect();
        if certs.is_empty() && url.scheme() == "https" {
            return Err("no trusted system certificates are available for form submission".into());
        }
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .max_redirects(0)
            .tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::new_with_certs(&certs)).build())
            .build()
            .new_agent();
        let response = match post {
            Some((body, content_type)) => agent.post(url.as_str()).header("Content-Type", content_type).send(body),
            None => agent.get(url.as_str()).call(),
        }
        .map_err(|e| format!("form submission failed: {e}"))?;
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
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn destination_requires_https_or_loopback() {
        assert!(FormDestination::new("https://forms.example/submit").is_ok());
        assert!(FormDestination::new("http://127.0.0.1:8080/submit").is_ok());
        for url in ["http://forms.example/submit", "file:///tmp/submit", "https://u:p@forms.example/submit", "https://forms.example/submit#FDF"] {
            assert!(FormDestination::new(url).is_err(), "{url}");
        }
    }

    #[test]
    fn get_html_appends_encoded_pairs_after_existing_query() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/submit?ticket=7", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let mut request = [0u8; 2048];
            let n = stream.read(&mut request).unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            String::from_utf8_lossy(&request[..n]).into_owned()
        });
        let status = FormDestination::new(&url).unwrap().get_html(b"Name=Ada+%26+Co&City=Madrid").unwrap();
        assert_eq!(status, 200);
        let request = worker.join().unwrap();
        assert!(request.starts_with("GET /submit?ticket=7&Name=Ada+%26+Co&City=Madrid HTTP/1.1\r\n"), "{request}");
    }
}
