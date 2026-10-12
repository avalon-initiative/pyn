//! The stranded-version report (docs/concepts/pyn-toml.md): the shared versions branches keep of a path that became
//! exclusive.

use pyn_proto as api;

use crate::table;
use crate::time;

/// Prints `versions` under `heading`; nothing when there are none.
pub fn print(heading: &str, versions: &[api::StrandedVersion]) {
    if versions.is_empty() {
        return;
    }
    println!("{heading}");
    let rows: Vec<Vec<String>> = versions
        .iter()
        .map(|v| {
            vec![
                v.branch.clone(),
                v.branch_owner.clone(),
                format!("r{}", v.revision),
                v.author.clone(),
                time::local(v.created_at),
                v.path.clone(),
            ]
        })
        .collect();
    println!(
        "{}",
        table::render(
            &["BRANCH", "OWNER", "REVISION", "BY", "WHEN", "PATH"],
            &rows
        )
    );
    println!(
        "To keep a stranded version, its owner takes the lock on the path and checks it in as the new head."
    );
}
