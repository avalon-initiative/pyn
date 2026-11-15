//! `--json`: versioned machine-readable output for the commands an integration calls (docs/cli-json.md).

use std::sync::atomic::{AtomicBool, Ordering};

use pyn_proto as api;
use serde::Serialize;
use serde_json::{Value, json};

use crate::client::ApiError;

/// Bumped only for a change that breaks an existing field.
pub const VERSION: u32 = 1;

static ON: AtomicBool = AtomicBool::new(false);

pub fn set() {
    ON.store(true, Ordering::Relaxed);
}

pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// The lock that made a `lock` fail, attached to the error when `--json` is on.
#[derive(Debug)]
pub struct HeldBy(pub api::Lock);

impl std::fmt::Display for HeldBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "locked by {}", self.0.owner)
    }
}

/// Prints `fields` (a JSON object) with the version added.
pub fn print(fields: Value) {
    let mut doc = json!({ "version": VERSION });
    if let (Some(doc), Value::Object(fields)) = (doc.as_object_mut(), fields) {
        doc.extend(fields);
    }
    println!("{doc}");
}

/// Prints `{"<key>": value}` with the version added.
pub fn print_as<T: Serialize>(key: &str, value: &T) {
    print(json!({ key: value }));
}

/// The error document: the server's code and status when there is one, else the code `error`.
pub fn error_doc(e: &anyhow::Error, message: &str) -> Value {
    let mut err = match e.downcast_ref::<ApiError>() {
        Some(a) => json!({ "code": a.code, "status": a.status, "message": a.message }),
        None => json!({ "code": "error", "message": message }),
    };
    if let Some(held) = e.downcast_ref::<HeldBy>() {
        err["lock"] = json!(held.0);
    }
    json!({ "version": VERSION, "error": err })
}

pub fn print_error(e: &anyhow::Error, message: &str) {
    println!("{}", error_doc(e, message));
}
