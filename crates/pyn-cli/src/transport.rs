//! The wire a pyn server is reached over: a request in, a status, headers and a streamed body out.

use std::io::{Read, Write};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use reqwest::Method;

/// Who a request speaks as.
#[derive(Clone, Debug, Default)]
pub enum Auth {
    #[default]
    Anonymous,
    Token(String),
    /// A development identity, accepted only by servers that allow it.
    DevUser(String),
}

/// One request to the server's API, independent of how it travels.
pub struct Call {
    pub method: Method,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub auth: Auth,
    encode_error: Option<String>,
}

impl Call {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
            auth: Auth::Anonymous,
            encode_error: None,
        }
    }

    /// Appends query pairs from a list of `(key, value)` tuples.
    pub fn query<T: Serialize>(mut self, pairs: &T) -> Self {
        match serde_json::to_value(pairs) {
            Ok(serde_json::Value::Array(items)) => {
                for item in items {
                    match item {
                        serde_json::Value::Array(kv) if kv.len() == 2 => {
                            self.query.push((scalar(&kv[0]), scalar(&kv[1])));
                        }
                        _ => self.encode_error = Some("query pairs must be (key, value)".into()),
                    }
                }
            }
            Ok(_) => self.encode_error = Some("query must be a list of pairs".into()),
            Err(e) => self.encode_error = Some(e.to_string()),
        }
        self
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_ascii_lowercase(), value.into()));
        self
    }

    pub fn body(mut self, bytes: Vec<u8>) -> Self {
        self.body = Some(bytes);
        self
    }

    pub fn json<T: Serialize>(mut self, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(bytes) => {
                self.body = Some(bytes);
                self.headers
                    .push(("content-type".into(), "application/json".into()));
            }
            Err(e) => self.encode_error = Some(e.to_string()),
        }
        self
    }

    pub fn bearer_auth(mut self, token: &str) -> Self {
        self.auth = Auth::Token(token.to_string());
        self
    }

    /// The first encoding failure, reported when the call is sent.
    pub fn check(&self) -> Result<()> {
        match &self.encode_error {
            Some(e) => Err(anyhow!("{e}")),
            None => Ok(()),
        }
    }
}

fn scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A server's answer: status, headers (names lowercase) and a body read on demand.
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    body: Box<dyn Read + Send>,
}

impl Reply {
    pub fn new(status: u16, headers: Vec<(String, String)>, body: Box<dyn Read + Send>) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn bytes(mut self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.body
            .read_to_end(&mut out)
            .context("reading the response body")?;
        Ok(out)
    }

    pub fn json<T: DeserializeOwned>(self) -> Result<T> {
        let bytes = self.bytes()?;
        serde_json::from_slice(&bytes).context("error decoding response body")
    }

    pub fn copy_to(&mut self, out: &mut dyn Write) -> Result<u64> {
        Ok(std::io::copy(&mut self.body, out)?)
    }
}

/// Carries a [`Call`] to a pyn server and returns its [`Reply`], whatever the status.
pub trait Transport: Send + Sync {
    fn send(&self, call: Call) -> Result<Reply>;
}
