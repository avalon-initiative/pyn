//! The HTTP transport: the server's API over blocking HTTP(S).

use std::time::Duration;

use anyhow::Result;
use pyn_proto as api;
use reqwest::Url;
use reqwest::blocking::Client;
use reqwest::header::HeaderValue;
use reqwest::redirect::Policy;

use crate::transport::{Auth, Call, Reply, Transport};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Limits the wait for a response and each read of a body, not the whole transfer, so large downloads proceed.
const STALL_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_REDIRECTS: usize = 10;

pub struct HttpTransport {
    http: Client,
    base: String,
}

/// Whether a redirect keeps to the same host (and may only upgrade to https), so credentials never travel elsewhere.
fn stays_on_host(from: &Url, to: &Url) -> bool {
    from.host_str() == to.host_str()
        && ((from.scheme() == to.scheme()
            && from.port_or_known_default() == to.port_or_known_default())
            || (from.scheme() == "http" && to.scheme() == "https"))
}

fn redirects() -> Policy {
    Policy::custom(|attempt| {
        let same = attempt
            .previous()
            .last()
            .is_some_and(|from| stays_on_host(from, attempt.url()));
        if attempt.previous().len() >= MAX_REDIRECTS || !same {
            attempt.stop()
        } else {
            attempt.follow()
        }
    })
}

impl HttpTransport {
    pub fn new(base: String) -> Self {
        let http = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(STALL_TIMEOUT)
            .redirect(redirects())
            .build()
            .expect("the HTTP client builds with its default TLS settings");
        Self { http, base }
    }
}

fn sensitive(value: &str) -> Result<HeaderValue> {
    let mut v = HeaderValue::from_str(value)?;
    v.set_sensitive(true);
    Ok(v)
}

impl Transport for HttpTransport {
    fn send(&self, call: Call) -> Result<Reply> {
        call.check()?;
        let mut req = self
            .http
            .request(call.method, format!("{}{}", self.base, call.path));
        if !call.query.is_empty() {
            req = req.query(&call.query);
        }
        for (name, value) in call.headers {
            req = req.header(name, value);
        }
        req = match call.auth {
            Auth::Token(token) => req.header(
                reqwest::header::AUTHORIZATION,
                sensitive(&format!("Bearer {token}"))?,
            ),
            Auth::DevUser(user) => req.header(api::DEV_USER_HEADER, sensitive(&user)?),
            Auth::Anonymous => req,
        };
        if let Some(body) = call.body {
            req = req.body(body);
        }
        let resp = req.send()?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();
        Ok(Reply::new(status, headers, Box::new(resp)))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn redirects_stay_on_the_host() {
        let cases = [
            ("http://a.example/x", "http://a.example/y", true),
            ("http://a.example/x", "https://a.example/y", true),
            ("https://a.example/x", "http://a.example/y", false),
            ("http://a.example/x", "http://b.example/y", false),
            ("http://a.example:80/x", "http://a.example:8080/y", false),
            ("http://127.0.0.1:1/x", "http://localhost:1/y", false),
        ];
        for (from, to, want) in cases {
            assert_eq!(stays_on_host(&url(from), &url(to)), want, "{from} {to}");
        }
    }

    fn serve(reply: String, hits: Arc<AtomicUsize>, seen: Arc<std::sync::Mutex<String>>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                *seen.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                hits.fetch_add(1, Ordering::SeqCst);
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        port
    }

    #[test]
    fn credentials_are_not_sent_to_another_host_after_a_redirect() {
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(std::sync::Mutex::new(String::new()));
        let other = serve(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".to_owned(),
            hits.clone(),
            seen.clone(),
        );
        let first_hits = Arc::new(AtomicUsize::new(0));
        let first = serve(
            format!(
                "HTTP/1.1 302 Found\r\nLocation: http://localhost:{other}/steal\r\nContent-Length: 0\r\n\r\n"
            ),
            first_hits.clone(),
            Arc::new(std::sync::Mutex::new(String::new())),
        );
        let http = HttpTransport::new(format!("http://127.0.0.1:{first}"));
        let call = Call::new(reqwest::Method::GET, "/v1/me").bearer_auth("secret-token");
        let reply = http.send(call).unwrap();
        assert_eq!(reply.status, 302);
        assert_eq!(first_hits.load(Ordering::SeqCst), 1);
        assert_eq!(hits.load(Ordering::SeqCst), 0, "{}", seen.lock().unwrap());
    }
}
