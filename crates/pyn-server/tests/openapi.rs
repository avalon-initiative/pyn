use std::path::PathBuf;

use pyn_server::ApiDoc;
use utoipa::OpenApi;

fn path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/generated/openapi.json")
}

/// Fails when `docs/generated/openapi.json` is stale; `PYN_UPDATE_OPENAPI=1` rewrites it.
#[test]
fn published_openapi_document_is_current() {
    let current = ApiDoc::openapi().to_pretty_json().unwrap() + "\n";
    if std::env::var_os("PYN_UPDATE_OPENAPI").is_some() {
        std::fs::write(path(), &current).unwrap();
        return;
    }
    let published = std::fs::read_to_string(path()).unwrap_or_default();
    assert!(
        published == current,
        "docs/generated/openapi.json is stale: run `make openapi` and commit the result"
    );
}
