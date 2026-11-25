//! Every `PYN_*` variable the server reads, and the ones other components own, so a stray one can be flagged.

/// Settings the server reads; a test proves every `"PYN_..."` literal in `src` is listed here or in `OTHER_COMPONENTS`.
pub const KNOWN: &[&str] = &[
    "PYN_ACCESS",
    "PYN_ACCESS_LOG",
    "PYN_ACCESS_LOG_CLIENT",
    "PYN_ACCOUNT_DELETION_WINDOW_DAYS",
    "PYN_ADDR",
    "PYN_ALLOWED_ORIGINS",
    "PYN_AUDIT_LOG",
    "PYN_BODY_IDLE_TIMEOUT_SECS",
    "PYN_BRANCH_STALE_DAYS",
    "PYN_CERT_CHECK_INTERVAL_SECS",
    "PYN_CONFIG",
    "PYN_CREATE_DATABASE",
    "PYN_DATABASE_URL",
    "PYN_DATA_DIR",
    "PYN_DEFAULT_MAX_ORG_MEMBERS",
    "PYN_DEFAULT_MAX_REPOSITORIES",
    "PYN_DEFAULT_MAX_STORAGE_BYTES",
    "PYN_DEV_AUTH",
    "PYN_DEV_AUTH_ALLOW_NON_LOOPBACK",
    "PYN_DEV_CLOCK_SPEED",
    "PYN_DISABLED_ACCOUNT_RETENTION_DAYS",
    "PYN_EMAIL_VERIFICATION",
    "PYN_INSTANCES",
    "PYN_LOG_FORMAT",
    "PYN_MAILER",
    "PYN_MAILER_LOG_ALLOW_NON_LOOPBACK",
    "PYN_MAX_CONCURRENT_EXPENSIVE",
    "PYN_MAX_CONCURRENT_MERGES",
    "PYN_MAX_FORKS_PER_OWNER",
    "PYN_MAX_FORKS_PER_REPO",
    "PYN_MAX_LOCKS_ALLOWED_PER_USER",
    "PYN_MAX_UNREFERENCED_UPLOAD_BYTES",
    "PYN_MAX_UPLOADS",
    "PYN_MAX_UPLOADS_PER_USER",
    "PYN_NEW_SECRET_KEY",
    "PYN_OIDC_CLIENT_ID",
    "PYN_OIDC_CLIENT_SECRET",
    "PYN_OIDC_CREATE_ACCOUNTS",
    "PYN_OIDC_ISSUER",
    "PYN_OIDC_NAME",
    "PYN_OIDC_REDIRECT_URL",
    "PYN_OIDC_USERNAME_CLAIM",
    "PYN_ORG_CREATION",
    "PYN_ORG_INVITATION_DAYS",
    "PYN_ORG_MEMBER_ADD",
    "PYN_PASSWORD_HASHES",
    "PYN_PASSWORD_SIGN_IN",
    "PYN_PUBLIC_URL",
    "PYN_RATE_MAIL_PER_EMAIL",
    "PYN_RATE_PEOPLE_CLIENT",
    "PYN_RATE_REGISTER_CLIENT",
    "PYN_RATE_SERVICE_CREDENTIAL",
    "PYN_RATE_SIGN_IN_ACCOUNT",
    "PYN_RATE_SIGN_IN_CLIENT",
    "PYN_REGISTRATION",
    "PYN_REQUEST_TIMEOUT_SECS",
    "PYN_REQUIRE_APPROVAL",
    "PYN_S3_ACCESS_KEY_ID",
    "PYN_S3_ACCESS_KEY_ID_FILE",
    "PYN_S3_BUCKET",
    "PYN_S3_ENDPOINT",
    "PYN_S3_MAX_RETRIES",
    "PYN_S3_PATH_STYLE",
    "PYN_S3_PREFIX",
    "PYN_S3_REGION",
    "PYN_S3_REQUEST_TIMEOUT_SECS",
    "PYN_S3_SECRET_ACCESS_KEY",
    "PYN_S3_SECRET_ACCESS_KEY_FILE",
    "PYN_S3_SPOOL_DIR",
    "PYN_S3_SPOOL_THRESHOLD",
    "PYN_SECRET_KEY",
    "PYN_SESSION_DAYS",
    "PYN_SESSION_REMEMBER_DAYS",
    "PYN_SETUP_TOKEN",
    "PYN_SHUTDOWN_DRAIN_DELAY_SECS",
    "PYN_SHUTDOWN_GRACE_SECS",
    "PYN_SMTP_FROM",
    "PYN_SMTP_HOST",
    "PYN_SMTP_PASSWORD",
    "PYN_SMTP_PORT",
    "PYN_SMTP_TLS",
    "PYN_SMTP_USER",
    "PYN_TRUSTED_PROXIES",
    "PYN_TRUST_FORWARDED_FOR",
    "PYN_UPDATE_CHECK",
    "PYN_WEBAUTHN_RP_ID",
    "PYN_WEBHOOKS_ALLOWED_HOSTS",
    "PYN_WEBHOOKS_ALLOW_HTTP",
    "PYN_WEBHOOKS_ALLOW_LOOPBACK",
    "PYN_WEBHOOKS_CONCURRENCY",
    "PYN_WEBHOOK_WORKER",
];

/// Variables owned by the CLI, the Compose files, the web container, the installer and the build. The Compose
/// `env_file: .env` hands the whole file to the server container, so these do reach it and must not be reported.
pub const OTHER_COMPONENTS: &[&str] = &[
    "PYN_API_URL",
    "PYN_CADDY_IMAGE",
    "PYN_CLI_CONFIG",
    "PYN_CONFIG_DIR",
    "PYN_DEBUG_SUBMODULE_CONFIG",
    "PYN_DIR",
    "PYN_EDITOR",
    "PYN_IMAGE",
    "PYN_PUBLISH",
    "PYN_REPO",
    "PYN_REVISION",
    "PYN_SERVER",
    "PYN_SERVER_PUBLISH",
    "PYN_SERVER_VERSION",
    "PYN_SITE",
    "PYN_TOKEN",
    "PYN_USER",
    "PYN_WEB_IMAGE",
    "PYN_WEB_PUBLISH",
    "PYN_WEB_PUBLISH_HTTP",
    "PYN_WEB_PUBLISH_TLS",
    "PYN_WEB_VERSION",
];

/// Prefixes of whole families owned elsewhere (the installer's `PYN_INSTALL_*`).
const OTHER_PREFIXES: &[&str] = &["PYN_INSTALL_"];

fn owned_elsewhere(name: &str) -> bool {
    OTHER_COMPONENTS.contains(&name) || OTHER_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Edit distance between two names, counting insertions, deletions and substitutions.
pub fn distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, &x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, &y) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(x != y))
                .min(above + 1)
                .min(row[j] + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

/// The nearest setting within an edit distance of 2.
pub fn suggestion(name: &str) -> Option<&'static str> {
    KNOWN
        .iter()
        .map(|known| (distance(name, known), *known))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, known)| known)
}

/// One line per `PYN_` name that is neither a setting nor owned by another component.
pub fn unknown_variables(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut unknown: Vec<String> = names
        .into_iter()
        .filter(|n| n.starts_with("PYN_") && !KNOWN.contains(&n.as_str()) && !owned_elsewhere(n))
        .collect();
    unknown.sort();
    unknown
        .into_iter()
        .map(|name| match suggestion(&name) {
            Some(near) => {
                format!("{name} is not a setting this server reads; did you mean {near}?")
            }
            None => format!("{name} is not a setting this server reads"),
        })
        .collect()
}

/// The unknown-variable lines for the current environment (names only, never values).
pub fn unknown_in_environment() -> Vec<String> {
    unknown_variables(std::env::vars_os().filter_map(|(name, _)| name.into_string().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }

    /// Names written as a whole string literal, the way the env getters take them.
    fn literals(text: &str) -> Vec<String> {
        let mut found = Vec::new();
        for part in text.split("\"PYN_").skip(1) {
            let end = part
                .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                .unwrap_or(part.len());
            if end > 0 && part[end..].starts_with('"') {
                found.push(format!("PYN_{}", &part[..end]));
            }
        }
        found
    }

    /// Every `PYN_` token in prose or config, skipping wildcards such as `PYN_S3_*`.
    fn tokens(text: &str) -> Vec<String> {
        let mut found = Vec::new();
        for part in text.split("PYN_").skip(1) {
            let end = part
                .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                .unwrap_or(part.len());
            if end > 0 && !part[..end].ends_with('_') {
                found.push(format!("PYN_{}", &part[..end]));
            }
        }
        found
    }

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn the_lists_are_sorted_and_disjoint() {
        for list in [KNOWN, OTHER_COMPONENTS] {
            assert!(list.windows(2).all(|w| w[0] < w[1]), "keep the list sorted");
        }
        assert!(KNOWN.iter().all(|n| !OTHER_COMPONENTS.contains(n)));
    }

    #[test]
    fn every_name_the_source_reads_is_registered_and_every_entry_is_read() {
        let mut files = Vec::new();
        walk(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let mut seen = std::collections::BTreeSet::new();
        for file in files.iter().filter(|f| !f.ends_with("known_settings.rs")) {
            seen.extend(literals(&std::fs::read_to_string(file).unwrap()));
        }
        assert!(seen.len() > 60, "the scan found only {}", seen.len());
        for name in &seen {
            assert!(
                KNOWN.contains(&name.as_str()) || OTHER_COMPONENTS.contains(&name.as_str()),
                "{name} is read in src but not listed in known_settings.rs"
            );
        }
        for name in KNOWN {
            assert!(
                seen.contains(*name),
                "{name} is listed but nothing reads it"
            );
        }
    }

    #[test]
    fn configuration_md_documents_exactly_the_known_settings() {
        let docs = std::fs::read_to_string(root().join("docs/configuration.md")).unwrap();
        let documented: std::collections::BTreeSet<String> = tokens(&docs).into_iter().collect();
        for name in &documented {
            assert!(
                KNOWN.contains(&name.as_str()) || OTHER_COMPONENTS.contains(&name.as_str()),
                "{name} is in docs/configuration.md but unknown to the server"
            );
        }
        for name in KNOWN {
            assert!(
                documented.contains(*name),
                "{name} is not in docs/configuration.md"
            );
        }
    }

    #[test]
    fn what_the_deploy_files_pass_is_a_setting_or_owned_elsewhere() {
        for file in [
            "deploy/.env.example",
            "deploy/docker-compose.yml",
            "deploy/docker-compose.server.yml",
            "deploy/docker-compose.web.yml",
            "docker-compose.yml",
        ] {
            let text = std::fs::read_to_string(root().join(file)).unwrap();
            for name in tokens(&text) {
                assert!(
                    KNOWN.contains(&name.as_str()) || owned_elsewhere(&name),
                    "{file} mentions {name}, which the server would report as unknown"
                );
            }
        }
    }

    #[test]
    fn the_distance_counts_edits() {
        assert_eq!(distance("PYN_ADDR", "PYN_ADDR"), 0);
        assert_eq!(distance("PYN_ADR", "PYN_ADDR"), 1);
        assert_eq!(distance("PYN_ADDER", "PYN_ADDR"), 1);
        assert_eq!(distance("PYN_ADDRESS", "PYN_ADDR"), 3);
        assert_eq!(distance("kitten", "sitting"), 3);
        assert_eq!(distance("", "abc"), 3);
    }

    #[test]
    fn a_near_miss_gets_a_suggestion_and_a_far_one_does_not() {
        assert_eq!(suggestion("PYN_PUBLIC_UR"), Some("PYN_PUBLIC_URL"));
        assert_eq!(suggestion("PYN_REGISTRTION"), Some("PYN_REGISTRATION"));
        assert_eq!(
            suggestion("PYN_TRUSTED_PROXIE"),
            Some("PYN_TRUSTED_PROXIES")
        );
        assert_eq!(suggestion("PYN_NOTHING_LIKE_IT"), None);
    }

    #[test]
    fn only_unknown_pyn_names_are_reported() {
        let names = [
            "PYN_PUBLIC_UR",
            "PYN_ADDR",
            "PYN_SERVER_VERSION",
            "PYN_INSTALL_COLOR",
            "PYN_ZZZZZZZZ",
            "HOME",
            "PYNX",
        ]
        .map(String::from);
        let lines = unknown_variables(names);
        assert_eq!(
            lines,
            [
                "PYN_PUBLIC_UR is not a setting this server reads; did you mean PYN_PUBLIC_URL?",
                "PYN_ZZZZZZZZ is not a setting this server reads",
            ]
        );
    }
}
