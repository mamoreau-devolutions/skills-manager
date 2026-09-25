//! Minimal blocking HTTP client standing in for the global `fetch`.

use std::io::Read;
use std::time::Duration;

pub struct Response {
    pub status: u16,
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_lowercase() == lower)
            .map(|(_, v)| v.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Option<serde_json::Value> {
        // `Response.json()` decodes as UTF-8, which drops a leading BOM.
        let body = self
            .body
            .strip_prefix(b"\xEF\xBB\xBF")
            .unwrap_or(&self.body);
        serde_json::from_slice(body).ok()
    }
}

pub struct Request<'a> {
    url: &'a str,
    headers: Vec<(String, String)>,
    timeout: Duration,
    max_bytes: Option<u64>,
}

/// Error raised when a response body exceeds `max_bytes`.
#[derive(Debug)]
pub struct TooLarge;

pub fn get(url: &str) -> Request<'_> {
    Request {
        url,
        headers: Vec::new(),
        timeout: Duration::from_secs(300),
        max_bytes: None,
    }
}

impl<'a> Request<'a> {
    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.to_string(), v.to_string()));
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    pub fn max_bytes(mut self, n: u64) -> Self {
        self.max_bytes = Some(n);
        self
    }

    /// Perform the request. Transport failures are `Err`; any HTTP status is `Ok`.
    pub fn send(self) -> Result<Response, String> {
        let agent = ureq::AgentBuilder::new()
            .timeout(self.timeout)
            .redirects(20)
            .user_agent(concat!("skills-cli/", env!("CARGO_PKG_VERSION")))
            .build();
        let mut req = agent.get(self.url);
        for (k, v) in &self.headers {
            req = req.set(k, v);
        }
        let resp = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(e.to_string()),
        };
        let status = resp.status();
        let headers: Vec<(String, String)> = resp
            .headers_names()
            .iter()
            .filter_map(|n| resp.header(n).map(|v| (n.clone(), v.to_string())))
            .collect();
        // Error statuses are reported before size limits (the TS download checks
        // `response.ok` first); for size-limited requests their bodies are skipped.
        let max_bytes = if (200..300).contains(&status) {
            self.max_bytes
        } else {
            None
        };
        if let (Some(max), Some(len)) = (
            max_bytes,
            resp.header("content-length")
                .and_then(|l| l.parse::<u64>().ok()),
        ) {
            if len > max {
                return Err(format!("too-large:{}", max));
            }
        }
        let mut body = Vec::new();
        let reader = resp.into_reader();
        let limit = match (max_bytes, self.max_bytes) {
            (Some(m), _) => m + 1,
            // Size-limited request that failed: the caller only needs the status.
            (None, Some(_)) => 0,
            (None, None) => u64::MAX,
        };
        reader
            .take(limit)
            .read_to_end(&mut body)
            .map_err(|e| e.to_string())?;
        if let Some(max) = max_bytes {
            if body.len() as u64 > max {
                return Err(format!("too-large:{}", max));
            }
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}
