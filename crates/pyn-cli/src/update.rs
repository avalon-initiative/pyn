//! `pyn version --check`: asks the signed-in server whether a newer release exists.

use anyhow::Result;
use pyn_proto as api;

use crate::client::{Api, ApiError};
use crate::json_out;

/// One line for the status; what it says when the server cannot tell is plain too.
fn line(s: &api::UpdateStatus) -> String {
    if !s.enabled {
        return "the server's update check is off; an operator turns it on with PYN_UPDATE_CHECK=on".into();
    }
    match (&s.latest, s.checked_at) {
        (Some(latest), _) => {
            let kind = if s.prerelease == Some(true) {
                "pre-release"
            } else {
                "stable release"
            };
            let url = s.url.as_deref().unwrap_or_default();
            format!(
                "server {} is behind: {latest} is available ({kind}); release notes: {url}",
                s.current
            )
        }
        (None, Some(_)) => format!("server {} is up to date", s.current),
        (None, None) => format!(
            "server {}: the update check has not completed yet; try again later",
            s.current
        ),
    }
}

pub fn check(api: &Api) -> Result<()> {
    let reply = api.send(api.get("/v1/admin/update"));
    let status = match reply {
        Ok(r) => r.json::<api::UpdateStatus>()?,
        Err(e) => {
            let refusal = match e.downcast_ref::<ApiError>() {
                Some(a) if a.code == "server_admin_required" => {
                    "only a server administrator can see update information"
                }
                Some(a) if a.status == 404 => "this server does not offer an update check",
                _ => return Err(e),
            };
            if json_out::on() {
                return Err(e);
            }
            println!("{refusal}");
            return Ok(());
        }
    };
    if json_out::on() {
        json_out::print_as("update", &status);
    } else {
        println!("{}", line(&status));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(latest: Option<&str>, pre: bool, checked: bool) -> api::UpdateStatus {
        api::UpdateStatus {
            enabled: true,
            current: "0.1.4".into(),
            latest: latest.map(str::to_string),
            prerelease: latest.map(|_| pre),
            url: latest.map(|v| format!("https://example.test/releases/v{v}")),
            checked_at: checked.then(chrono::Utc::now),
        }
    }

    #[test]
    fn a_newer_release_names_its_kind_and_link() {
        let text = line(&status(Some("0.1.5-alpha"), true, true));
        assert!(text.contains("0.1.5-alpha") && text.contains("pre-release"));
        assert!(text.contains("https://example.test/releases/v0.1.5-alpha"));
        assert!(line(&status(Some("0.2.0"), false, true)).contains("stable release"));
    }

    #[test]
    fn up_to_date_and_not_yet_known_differ() {
        assert!(line(&status(None, false, true)).contains("up to date"));
        assert!(line(&status(None, false, false)).contains("not completed"));
    }
}
