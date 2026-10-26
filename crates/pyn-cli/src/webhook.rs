//! Webhook subscription administration. Signing secrets are shown once, on create and rotate.

use crate::transport::Method;
use anyhow::{Result, bail};
use clap::Subcommand;
use pyn_proto as api;

use crate::client::Api;
use crate::{table, time};

#[derive(Subcommand)]
pub enum WebhookCommand {
    /// Create a subscription. Its signing secret is printed once on stdout.
    Create {
        name: String,
        /// Where deliveries are sent: https only, no user name or password.
        #[arg(long)]
        url: String,
        /// Event types to deliver, comma-separated or repeated; every type when left out.
        #[arg(long = "event", value_delimiter = ',')]
        events: Vec<String>,
        /// Create it turned off.
        #[arg(long)]
        disabled: bool,
    },
    /// List subscriptions.
    List,
    /// Show one subscription and its active secrets (never their values).
    Show { id: i64 },
    /// Change a subscription; fields you leave out stay as they are.
    Update {
        id: i64,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        url: Option<String>,
        /// Replace the event types, comma-separated or repeated.
        #[arg(long = "event", value_delimiter = ',', conflicts_with = "all_events")]
        events: Vec<String>,
        /// Deliver every event type.
        #[arg(long)]
        all_events: bool,
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        #[arg(long)]
        disable: bool,
    },
    /// Delete a subscription with its secrets and deliveries.
    Delete { id: i64 },
    /// Add a second active secret, printed once on stdout. The old one keeps signing until you retire it.
    Rotate { id: i64 },
    /// Stop signing with one active secret, once receivers use the new one.
    Retire { id: i64, secret_id: i64 },
    /// List a subscription's deliveries, newest event first.
    Deliveries {
        id: i64,
        /// Only this status: pending, delivered or failed.
        #[arg(long)]
        status: Option<String>,
        /// Page size (server default 50, at most 200).
        #[arg(long)]
        limit: Option<u32>,
        /// Continue after a page: the event id printed at its end.
        #[arg(long)]
        before: Option<i64>,
    },
    /// Queue a delivery again now. It keeps its delivery id, so receivers can dedupe.
    Redeliver { id: i64, delivery_id: String },
}

pub fn run(api: &Api, cmd: WebhookCommand) -> Result<()> {
    match cmd {
        WebhookCommand::Create {
            name,
            url,
            events,
            disabled,
        } => {
            let body = api::CreateWebhookRequest {
                name,
                url,
                event_types: events,
                enabled: !disabled,
            };
            let made: api::CreatedWebhook = api
                .send(api.request(Method::POST, "/v1/admin/webhooks").json(&body))?
                .json()?;
            println!("{}", made.secret);
            eprintln!(
                "webhook {} created; this is the only time the signing secret is shown",
                made.webhook.id
            );
        }
        WebhookCommand::List => {
            let all: Vec<api::Webhook> = api.send(api.get("/v1/admin/webhooks"))?.json()?;
            let rows: Vec<Vec<String>> = all
                .into_iter()
                .map(|w| {
                    vec![
                        w.id.to_string(),
                        if w.enabled { "on" } else { "off" }.to_string(),
                        w.secrets.len().to_string(),
                        events_text(&w.event_types),
                        w.name,
                        w.url,
                    ]
                })
                .collect();
            table::show(
                &["ID", "STATE", "SECRETS", "EVENTS", "NAME", "URL"],
                &rows,
                "no webhooks",
            );
        }
        WebhookCommand::Show { id } => {
            let w: api::Webhook = api
                .send(api.get(&format!("/v1/admin/webhooks/{id}")))?
                .json()?;
            println!("id:      {}", w.id);
            println!("name:    {}", w.name);
            println!("url:     {}", w.url);
            println!("state:   {}", if w.enabled { "on" } else { "off" });
            println!("events:  {}", events_text(&w.event_types));
            println!("created: {} by {}", time::local(w.created_at), w.created_by);
            println!("updated: {}", time::local(w.updated_at));
            for s in &w.secrets {
                println!(
                    "secret {}: created {}{}",
                    s.id,
                    time::local(s.created_at),
                    if s.encrypted { ", encrypted" } else { "" }
                );
            }
        }
        WebhookCommand::Update {
            id,
            name,
            url,
            events,
            all_events,
            enable,
            disable,
        } => {
            let body = api::UpdateWebhookRequest {
                name,
                url,
                event_types: (all_events || !events.is_empty()).then_some(events),
                enabled: match (enable, disable) {
                    (true, _) => Some(true),
                    (_, true) => Some(false),
                    _ => None,
                },
            };
            let w: api::Webhook = api
                .send(
                    api.request(Method::PATCH, &format!("/v1/admin/webhooks/{id}"))
                        .json(&body),
                )?
                .json()?;
            println!("updated webhook {}", w.id);
        }
        WebhookCommand::Delete { id } => {
            api.send(api.request(Method::DELETE, &format!("/v1/admin/webhooks/{id}")))?;
            println!("deleted webhook {id}");
        }
        WebhookCommand::Rotate { id } => {
            let made: api::RotatedWebhookSecret = api
                .send(api.request(Method::POST, &format!("/v1/admin/webhooks/{id}/secrets")))?
                .json()?;
            println!("{}", made.secret);
            eprintln!(
                "secret {} added to webhook {id}; this is the only time it is shown. Deliveries carry a signature for \
                 each active secret: once receivers use the new one, run `pyn admin webhook retire {id} <old secret id>`",
                made.secret_id
            );
        }
        WebhookCommand::Retire { id, secret_id } => {
            if secret_id <= 0 {
                bail!("secret id must be positive");
            }
            api.send(api.request(
                Method::DELETE,
                &format!("/v1/admin/webhooks/{id}/secrets/{secret_id}"),
            ))?;
            println!("retired secret {secret_id} of webhook {id}");
        }
        WebhookCommand::Deliveries {
            id,
            status,
            limit,
            before,
        } => {
            let mut req = api.get(&format!("/v1/admin/webhooks/{id}/deliveries"));
            if let Some(s) = &status {
                req = req.query(&[("status", s)]);
            }
            if let Some(l) = limit {
                req = req.query(&[("limit", l)]);
            }
            if let Some(b) = before {
                req = req.query(&[("before", b)]);
            }
            let page: api::WebhookDeliveryPage = api.send(req)?.json()?;
            let rows: Vec<Vec<String>> = page
                .deliveries
                .into_iter()
                .map(|d| {
                    vec![
                        d.event_id.to_string(),
                        status_text(d.status).to_string(),
                        d.attempts.to_string(),
                        match d.status {
                            api::WebhookDeliveryStatus::Pending => time::local(d.next_attempt_at),
                            _ => "-".to_string(),
                        },
                        d.last_status_code.map_or("-".into(), |c| c.to_string()),
                        d.id,
                        d.last_error.unwrap_or_default(),
                    ]
                })
                .collect();
            table::show(
                &[
                    "EVENT", "STATUS", "ATTEMPTS", "NEXT", "CODE", "DELIVERY", "ERROR",
                ],
                &rows,
                "no deliveries",
            );
            if let Some(next) = page.next_before {
                eprintln!("more: pass --before {next}");
            }
        }
        WebhookCommand::Redeliver { id, delivery_id } => {
            let d: api::WebhookDelivery = api
                .send(api.request(
                    Method::POST,
                    &format!("/v1/admin/webhooks/{id}/deliveries/{delivery_id}/redeliver"),
                ))?
                .json()?;
            println!("delivery {} of webhook {id} queued again", d.id);
        }
    }
    Ok(())
}

fn events_text(types: &[String]) -> String {
    if types.is_empty() {
        "all".to_string()
    } else {
        types.join(",")
    }
}

fn status_text(s: api::WebhookDeliveryStatus) -> &'static str {
    match s {
        api::WebhookDeliveryStatus::Pending => "pending",
        api::WebhookDeliveryStatus::Delivered => "delivered",
        api::WebhookDeliveryStatus::Failed => "failed",
    }
}
