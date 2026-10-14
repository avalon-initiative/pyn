//! The HTTP transport: the server's API over blocking HTTP(S).

use anyhow::Result;
use pyn_proto as api;
use reqwest::blocking::Client;

use crate::transport::{Auth, Call, Reply, Transport};

pub struct HttpTransport {
    http: Client,
    base: String,
}

impl HttpTransport {
    pub fn new(base: String) -> Self {
        Self {
            http: Client::new(),
            base,
        }
    }
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
            Auth::Token(token) => req.bearer_auth(token),
            Auth::DevUser(user) => req.header(api::DEV_USER_HEADER, user),
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
