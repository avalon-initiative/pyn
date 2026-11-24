//! `pyn tag --server`: the repository's immutable tags on the pyn server, shared by everyone who can read it.

use anyhow::{Result, bail};
use globset::Glob;
use pyn_proto as api;

use crate::backend::Backend;
use crate::branch;
use crate::client::Api;
use crate::git_home::TagArgs;
use crate::snapshot::short;
use crate::transport::Method;
use crate::workspace::Workspace;

pub fn run(backend: &dyn Backend, ws: Option<&Workspace>, args: TagArgs) -> Result<()> {
    let api = backend.pyn_server("tag")?;
    if args.force {
        bail!(
            "a server tag never moves; delete it with `pyn tag --server -d <name>` and make it again"
        );
    }
    if args.delete {
        return delete(api, &args.names);
    }
    if args.list || args.names.is_empty() {
        return list(api, &args.names);
    }
    let (name, target) = match args.names.as_slice() {
        [name] => (name, None),
        [name, target] => (name, Some(target.as_str())),
        _ => bail!("too many arguments to create a tag; the usage is `pyn tag <name> [<commit>]`"),
    };
    let target = match target {
        Some(spec) => branch::start_point(backend, api, spec)?,
        None => branch::name(ws),
    };
    let message = (!args.message.is_empty()).then(|| args.message.join("\n\n"));
    let body = api::CreateTagRequest {
        name: name.clone(),
        target: Some(target),
        message,
    };
    let made: api::Tag = api
        .send(
            api.request(Method::POST, &api.repo_route("/tags")?)
                .json(&body),
        )?
        .json()?;
    println!("tagged {} at {}", made.name, short(&made.commit));
    Ok(())
}

fn list(api: &Api, patterns: &[String]) -> Result<()> {
    let matchers: Vec<_> = patterns
        .iter()
        .map(|p| Glob::new(p).map(|g| g.compile_matcher()))
        .collect::<std::result::Result<_, _>>()?;
    let tags: Vec<api::Tag> = api.send(api.get(&api.repo_route("/tags")?))?.json()?;
    for t in tags {
        if matchers.is_empty() || matchers.iter().any(|m| m.is_match(&t.name)) {
            println!("{}", t.name);
        }
    }
    Ok(())
}

fn delete(api: &Api, names: &[String]) -> Result<()> {
    for name in names {
        let tag: api::Tag = api
            .send(api.get(&api.repo_route(&format!("/tags/{name}"))?))?
            .json()?;
        api.send(api.request(Method::DELETE, &api.repo_route(&format!("/tags/{name}"))?))?;
        println!("Deleted tag '{name}' (was {})", short(&tag.commit));
    }
    Ok(())
}
