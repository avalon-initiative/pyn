use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Url;
use reqwest::redirect::Policy;

use super::addresses::is_blocked;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 16 * 1024;

/// One webhook POST: the receiver's URL, the headers to add and the JSON body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingRequest {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

/// Sends one request. `Ok` carries the HTTP status whatever it is; `Err` is a refusal or a transport failure,
/// described without the URL's path or query.
#[async_trait]
pub trait HttpSender: Send + Sync {
    async fn send(&self, request: OutgoingRequest) -> Result<u16, String>;
}

/// Sends over https (http only when allowed) to public addresses only, with no redirects and short timeouts.
#[derive(Debug, Clone, Default)]
pub struct SafeSender {
    allow_http: bool,
    allowed_hosts: Vec<String>,
    /// Lets tests reach a loopback listener; never set outside them.
    permit_private: bool,
}

impl SafeSender {
    /// A non-empty `allowed_hosts` limits deliveries to those hosts: `example.com` or `*.example.com`.
    pub fn new(allow_http: bool, allowed_hosts: Vec<String>) -> Self {
        Self {
            allow_http,
            allowed_hosts: allowed_hosts
                .into_iter()
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
            permit_private: false,
        }
    }

    fn host_allowed(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.allowed_hosts.is_empty()
            || self
                .allowed_hosts
                .iter()
                .any(|pattern| match pattern.strip_prefix("*.") {
                    Some(suffix) => host.strip_suffix(suffix).is_some_and(|p| p.ends_with('.')),
                    None => *pattern == host,
                })
    }

    /// The addresses to connect to: the host's public ones, or an error when it has none.
    async fn resolve(&self, url: &Url) -> Result<(Vec<SocketAddr>, Option<String>), String> {
        let port = url.port_or_known_default().ok_or("no port")?;
        let host = url.host_str().ok_or("no host")?;
        if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
            return self
                .public(vec![SocketAddr::new(ip, port)])
                .map(|a| (a, None));
        }
        let found: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| "the host name did not resolve".to_string())?
            .collect();
        self.public(found).map(|a| (a, Some(host.to_string())))
    }

    fn public(&self, found: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, String> {
        let allowed: Vec<_> = found
            .iter()
            .copied()
            .filter(|a| self.permit_private || !is_blocked(a.ip()))
            .collect();
        if allowed.is_empty() {
            return Err("the host resolves only to addresses webhooks may not use".into());
        }
        Ok(allowed)
    }
}

#[async_trait]
impl HttpSender for SafeSender {
    async fn send(&self, request: OutgoingRequest) -> Result<u16, String> {
        let url = Url::parse(&request.url).map_err(|_| "the url is not valid".to_string())?;
        match url.scheme() {
            "https" => {}
            "http" if self.allow_http => {}
            _ => return Err("only https urls are delivered to".into()),
        }
        let host = url.host_str().ok_or("the url has no host")?.to_string();
        if !self.host_allowed(&host) {
            return Err("the host is not on the operator's allow-list".into());
        }
        let (addrs, name) = self.resolve(&url).await?;
        let mut client = reqwest::Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .user_agent("pyn-webhooks");
        if let Some(name) = &name {
            client = client.resolve_to_addrs(name, &addrs);
        }
        let client = client.build().map_err(|e| e.without_url().to_string())?;
        let mut builder = client
            .post(url)
            .header("Content-Type", "application/json")
            .body(request.body);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        let mut response = builder.send().await.map_err(describe)?;
        let status = response.status().as_u16();
        let mut read = 0;
        while read < MAX_RESPONSE_BYTES {
            match response.chunk().await {
                Ok(Some(chunk)) => read += chunk.len(),
                _ => break,
            }
        }
        Ok(status)
    }
}

fn describe(e: reqwest::Error) -> String {
    let e = e.without_url();
    if e.is_timeout() {
        "the request timed out".into()
    } else if e.is_connect() {
        "could not connect".into()
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    fn request(url: &str) -> OutgoingRequest {
        OutgoingRequest {
            url: url.into(),
            headers: vec![("Pyn-Event", "x.y".into())],
            body: b"{}".to_vec(),
        }
    }

    #[test]
    fn the_allow_list_matches_exact_hosts_and_subdomains() {
        let s = SafeSender::new(
            false,
            vec!["Hooks.Example.com".into(), "*.corp.test".into()],
        );
        assert!(s.host_allowed("hooks.example.com"));
        assert!(s.host_allowed("a.corp.test"));
        assert!(!s.host_allowed("corp.test"));
        assert!(!s.host_allowed("evilcorp.test"));
        assert!(!s.host_allowed("other.example.com"));
        assert!(SafeSender::default().host_allowed("anything.test"));
    }

    #[tokio::test]
    async fn private_targets_and_plain_http_are_refused_before_connecting() {
        let s = SafeSender::default();
        for url in [
            "https://127.0.0.1/hook",
            "https://[::1]/hook",
            "https://169.254.169.254/latest/meta-data",
            "https://10.0.0.5:8443/hook",
            "https://localhost/hook",
            "http://8.8.8.8/hook",
            "ftp://8.8.8.8/hook",
        ] {
            assert!(s.send(request(url)).await.is_err(), "{url}");
        }
        let listed = SafeSender::new(false, vec!["only.example.com".into()]);
        let err = listed.send(request("https://8.8.8.8/")).await.unwrap_err();
        assert!(err.contains("allow-list"));
    }

    /// Serves one canned response on a loopback port.
    async fn serve(response: &'static str) -> (SocketAddr, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 1024];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") || !seen.ends_with(b"{}") {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buf[..n]);
            }
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&seen).into_owned()
        });
        (addr, task)
    }

    fn local_sender() -> SafeSender {
        SafeSender {
            allow_http: true,
            permit_private: true,
            ..SafeSender::default()
        }
    }

    #[tokio::test]
    async fn a_redirect_is_reported_and_not_followed() {
        let (addr, seen) = serve(
            "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\n\r\n",
        )
        .await;
        let status = local_sender()
            .send(request(&format!("http://{addr}/hook")))
            .await
            .unwrap();
        assert_eq!(status, 302);
        let request = seen.await.unwrap();
        assert!(request.starts_with("POST /hook"));
        assert!(request.to_ascii_lowercase().contains("pyn-event: x.y"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: application/json")
        );
    }

    #[tokio::test]
    async fn a_huge_response_is_read_only_up_to_the_cap() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000000\r\n\r\n")
                .await;
            let chunk = vec![b'x'; 8192];
            while socket.write_all(&chunk).await.is_ok() {}
        });
        let status = local_sender()
            .send(request(&format!("http://{addr}/")))
            .await
            .unwrap();
        assert_eq!(status, 200);
    }
}
