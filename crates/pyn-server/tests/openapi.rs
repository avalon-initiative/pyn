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

fn spec() -> serde_json::Value {
    serde_json::to_value(ApiDoc::openapi()).unwrap()
}

#[test]
fn every_path_parameter_is_declared() {
    let spec = spec();
    let mut missing = Vec::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let declared: Vec<&str> = op["parameters"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|p| p["in"] == "path")
                .map(|p| p["name"].as_str().unwrap())
                .collect();
            for name in path
                .split('{')
                .skip(1)
                .map(|s| s.split('}').next().unwrap())
            {
                if !declared.contains(&name) {
                    missing.push(format!("{method} {path}: {name}"));
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "undeclared path parameters: {missing:#?}"
    );
}

fn enum_values(spec: &serde_json::Value, schema: &str, field: &str) -> Vec<String> {
    let mut prop = &spec["components"]["schemas"][schema]["properties"][field];
    if let Some(one_of) = prop["oneOf"].as_array() {
        prop = one_of.iter().find(|v| v["type"] != "null").unwrap();
    }
    let target = match prop["$ref"].as_str() {
        Some(r) => &spec["components"]["schemas"][r.rsplit('/').next().unwrap()],
        None => prop,
    };
    let values = target["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("{schema}.{field} is not an enum"));
    values
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn closed_sets_are_enums() {
    let spec = spec();
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "AccountInfo",
            "status",
            &[
                "pending_verification",
                "pending_approval",
                "active",
                "disabled",
            ],
        ),
        (
            "Registered",
            "status",
            &[
                "pending_verification",
                "pending_approval",
                "active",
                "disabled",
            ],
        ),
        (
            "RegistrationInfo",
            "registration",
            &["open", "invite", "closed"],
        ),
        ("SetupStatus", "registration", &["open", "invite", "closed"]),
        (
            "SetupRequest",
            "registration",
            &["open", "invite", "closed"],
        ),
        ("OrgMember", "role", &["owner", "member"]),
        ("OrgInfo", "role", &["owner", "member"]),
        ("AddOrgMemberRequest", "role", &["owner", "member"]),
        ("SetOrgRoleRequest", "role", &["owner", "member"]),
        ("CreationRuleInfo", "effect", &["allow", "deny"]),
        ("CreationRuleInfo", "kind", &["team", "user", "role"]),
        ("CreationRuleInfo", "scope", &["public", "private", "both"]),
        (
            "SetCreationRuleRequest",
            "scope",
            &["public", "private", "both"],
        ),
        (
            "RepoPolicyInfo",
            "member_creation",
            &["none", "private", "both"],
        ),
        (
            "SetRepoPolicyRequest",
            "member_creation",
            &["none", "private", "both"],
        ),
        ("Member", "source", &["direct", "team", "org_owner"]),
    ];
    for (schema, field, want) in cases {
        assert_eq!(&enum_values(&spec, schema, field), want, "{schema}.{field}");
    }
}

#[test]
fn nullable_cursors_are_required() {
    let spec = spec();
    for (schema, field) in [
        ("AuditPage", "next_before"),
        ("HistoryPage", "next_cursor"),
        ("FilePage", "next_after"),
    ] {
        let required = spec["components"]["schemas"][schema]["required"]
            .as_array()
            .unwrap();
        assert!(required.iter().any(|r| r == field), "{schema}.{field}");
    }
}

#[test]
fn operation_ids_are_unique() {
    let spec = spec();
    let mut seen = std::collections::HashMap::new();
    let mut dupes = Vec::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let id = op["operationId"].as_str().unwrap().to_string();
            if let Some(first) = seen.insert(id.clone(), format!("{method} {path}")) {
                dupes.push(format!("{id}: {first} and {method} {path}"));
            }
        }
    }
    assert!(dupes.is_empty(), "duplicate operationIds: {dupes:#?}");
}
