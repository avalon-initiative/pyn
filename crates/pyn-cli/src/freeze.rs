//! Owner freeze: a user or organization that keeps every byte and stays readable but cannot be written to.

use crate::transport::Method;
use anyhow::Result;
use pyn_proto as api;

use crate::client::Api;
use crate::time;

pub fn freeze(api: &Api, owner: &str, reason: String) -> Result<()> {
    let body = api::FreezeRequest { reason };
    let state: api::OwnerFreeze = api
        .send(
            api.request(Method::PUT, &format!("/v1/admin/owners/{owner}/freeze"))
                .json(&body),
        )?
        .json()?;
    println!("{}", describe(&state));
    Ok(())
}

pub fn unfreeze(api: &Api, owner: &str) -> Result<()> {
    let state: api::OwnerFreeze = api
        .send(api.request(Method::DELETE, &format!("/v1/admin/owners/{owner}/freeze")))?
        .json()?;
    println!("{}", describe(&state));
    Ok(())
}

pub fn show(api: &Api, owner: &str) -> Result<()> {
    let state: api::OwnerFreeze = api
        .send(api.get(&format!("/v1/owners/{owner}/freeze")))?
        .json()?;
    println!("{}", describe(&state));
    Ok(())
}

/// Prints the freeze line under an owner's summary; stays silent when the caller may not look or nothing is frozen.
pub fn note(api: &Api, owner: &str) {
    let Ok(response) = api.send(api.get(&format!("/v1/owners/{owner}/freeze"))) else {
        return;
    };
    if let Ok(state) = response.json::<api::OwnerFreeze>()
        && state.frozen
    {
        println!("{}", describe(&state));
    }
}

fn describe(state: &api::OwnerFreeze) -> String {
    match (state.frozen_at, &state.reason) {
        (Some(at), reason) => format!(
            "{} is frozen since {}: {}; reads continue, every write is refused",
            state.owner,
            time::local(at),
            reason.as_deref().unwrap_or("no reason given")
        ),
        _ => format!("{} is not frozen", state.owner),
    }
}
