//! `cargo run -- seed`: fill an empty database with clearly fake
//! packaging-sales data. Every company, person and address is invented and
//! uses the reserved `.example` domain.
//!
//! Dates are relative to now, so the stale deals stay stale and the fresh
//! ones fresh whenever it runs. Timeline entries go through the real paste
//! flow with the stub extractor, so the seed also leaves pending proposals,
//! auto-filled fields and suggested tasks behind exactly as the API would.

use std::sync::Arc;

use anyhow::bail;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::app_definition::AppDefinitionDoc;
use crate::config;
use crate::control::app_definitions;
use crate::control::saved_queries::{self, NewSavedQuery};
use crate::control::workspaces;
use crate::control::{ControlPool, profiles};
use crate::extractor::{Extractor, StubExtractor};
use crate::opportunities::{self, NewOpportunity, Stage};
use crate::{app_definition, paste, tasks};

const DEV_USER_EMAIL: &str = "jack@lula.dog.example";
const JACKS_WORKSPACE_NAME: &str = "Jack's Packaging";

pub struct SeedSummary {
    pub opportunities: usize,
}

fn days_ago(days: i64) -> DateTime<Utc> {
    Utc::now() - Duration::days(days)
}

fn ts(dt: DateTime<Utc>) -> String {
    crate::time::format(&dt)
}

fn date(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d").to_string()
}

fn opp(company: &str, name: &str, email: &str, stage: Stage) -> NewOpportunity {
    NewOpportunity {
        company: company.to_owned(),
        contact_name: Some(name.to_owned()),
        contact_email: Some(email.to_owned()),
        value_cents: None,
        stage: Some(stage),
        next_step: None,
        last_touch_at: None,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "seed data reads best as one top-to-bottom list of deals"
)]
pub async fn run(pool: &SqlitePool) -> anyhow::Result<SeedSummary> {
    let mut conn = pool.acquire().await?;
    let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM opportunities")
        .fetch_one(&mut *conn)
        .await?;
    if existing > 0 {
        bail!(
            "database already has {existing} opportunities; seed only runs on an empty database \
             (delete the database file to start over)"
        );
    }
    let extractor: Arc<dyn Extractor> = Arc::new(StubExtractor);
    let mut count = 0;

    // 1. Corrugated mailer boxes, proposal out, touched 3 days ago.
    let northwind = opportunities::insert(
        &mut conn,
        NewOpportunity {
            value_cents: Some(840_000),
            ..opp(
                "Northwind Coffee Roasters",
                "Priya Raman",
                "priya@northwind-coffee.example",
                Stage::Proposal,
            )
        },
    )
    .await?;
    count += 1;
    paste::run(
        pool,
        &extractor,
        northwind.id,
        &format!(
            "From: Priya Raman <priya@northwind-coffee.example>\n\
             Date: {}\n\
             Subject: Re: Mailer box quote for 12oz bags\n\n\
             Thanks for the quote on the kraft E-flute mailers (9x6x3), 6,000 units.\n\
             Our ops lead wants to see a printed sample before we sign.\n\
             Next step: Ship two printed samples\n\
             TODO: Ship printed mailer samples by {}\n",
            days_ago(3).to_rfc2822(),
            date(days_ago(-4)),
        ),
    )
    .await?;

    // 2. Poly mailers, qualified, last touch 20 days ago: STALE.
    let bluebird = opportunities::insert(
        &mut conn,
        NewOpportunity {
            value_cents: Some(375_000),
            ..opp(
                "Bluebird Apparel",
                "Marcus Lee",
                "marcus@bluebird-apparel.example",
                Stage::Qualified,
            )
        },
    )
    .await?;
    count += 1;
    paste::run(
        pool,
        &extractor,
        bluebird.id,
        &format!(
            "From: Marcus Lee <marcus@bluebird-apparel.example>\n\
             Date: {}\n\
             Subject: 10x13 poly mailers\n\n\
             We go through about 25,000 10x13 poly mailers a quarter. Current supplier\n\
             is late on every order. Send me what you can do on 2.5 mil white.\n",
            days_ago(20).to_rfc2822(),
        ),
    )
    .await?;

    // 3. Pallets, lead, touched recently but with an overdue task: STALE.
    let ridgeline = opportunities::insert(
        &mut conn,
        NewOpportunity {
            last_touch_at: Some(ts(days_ago(5))),
            next_step: Some("Confirm pallet spec and monthly volume".to_owned()),
            ..opp(
                "Ridgeline Hardware Supply",
                "Tom Becker",
                "tbecker@ridgeline-hardware.example",
                Stage::Lead,
            )
        },
    )
    .await?;
    count += 1;
    tasks::insert(
        &mut conn,
        ridgeline.id,
        "Send GMA 48x40 heat-treated pallet pricing",
        Some(&date(days_ago(3))),
        None,
    )
    .await?;

    // 4. Repeat order of RSC boxes, won a month ago (won deals are never stale).
    opportunities::insert(
        &mut conn,
        NewOpportunity {
            value_cents: Some(298_000),
            last_touch_at: Some(ts(days_ago(30))),
            next_step: Some("Check in on the next reorder in November".to_owned()),
            ..opp(
                "Harbor Candle Co.",
                "Elena Park",
                "elena@harborcandle.example",
                Stage::Won,
            )
        },
    )
    .await?;
    count += 1;

    // 5. Double-wall boxes, proposal; the latest email states a new total, so
    //    the paste leaves a pending value_cents proposal.
    let summit = opportunities::insert(
        &mut conn,
        NewOpportunity {
            value_cents: Some(1_450_000),
            ..opp(
                "Summit Outdoor Gear",
                "Jordan Diaz",
                "jordan@summit-outdoor.example",
                Stage::Proposal,
            )
        },
    )
    .await?;
    count += 1;
    paste::run(
        pool,
        &extractor,
        summit.id,
        &format!(
            "From: Jordan Diaz <jordan@summit-outdoor.example>\n\
             Date: {}\n\
             Subject: Re: Double-wall 24x18x18 proposal\n\n\
             We bumped the order to 4,000 boxes, so the total on our side is $15,200.00.\n\
             Can you confirm the ship date before we cut the PO?\n\
             Next step: Confirm ship date with the plant\n\
             TODO: Confirm ship date for 4,000 double-wall boxes by {}\n",
            days_ago(1).to_rfc2822(),
            date(days_ago(-2)),
        ),
    )
    .await?;

    // 6. Repeat poly mailer order, qualified, touched 8 days ago.
    let prairie = opportunities::insert(
        &mut conn,
        NewOpportunity {
            value_cents: Some(190_000),
            last_touch_at: Some(ts(days_ago(8))),
            next_step: Some("Send reorder confirmation".to_owned()),
            ..opp(
                "Prairie Pet Supply",
                "Sam Okafor",
                "sam@prairiepet.example",
                Stage::Qualified,
            )
        },
    )
    .await?;
    count += 1;
    tasks::insert(
        &mut conn,
        prairie.id,
        "Confirm repeat order: 10,000 12x15.5 poly mailers",
        Some(&date(days_ago(-5))),
        None,
    )
    .await?;

    // 7. Lost to another supplier.
    let coastal = opportunities::insert(
        &mut conn,
        NewOpportunity {
            value_cents: Some(450_000),
            ..opp(
                "Coastal Ceramics Studio",
                "Ava Nguyen",
                "ava@coastalceramics.example",
                Stage::Lost,
            )
        },
    )
    .await?;
    count += 1;
    paste::run(
        pool,
        &extractor,
        coastal.id,
        &format!(
            "From: Ava Nguyen <ava@coastalceramics.example>\n\
             Date: {}\n\
             Subject: Re: Molded pulp inserts\n\n\
             Thanks for your time. We went with another supplier for the inserts and boxes.\n",
            days_ago(40).to_rfc2822(),
        ),
    )
    .await?;

    // 8. Brand-new lead, no touch yet.
    opportunities::insert(
        &mut conn,
        NewOpportunity {
            next_step: Some("Intro call about 12-pack carriers and pallets".to_owned()),
            ..opp(
                "Granite State Brewing",
                "Riley Morgan",
                "riley@granitebrewing.example",
                Stage::Lead,
            )
        },
    )
    .await?;
    count += 1;

    Ok(SeedSummary {
        opportunities: count,
    })
}

/// Find or create the dev user's profile and their workspace ("Jack's
/// Packaging"), returning the workspace id. Idempotent: `AUTH_MODE=dev`'s
/// default user (`config::DEFAULT_DEV_USER_ID`) ends up owning exactly one
/// workspace by this name, so a locally-running server (`AUTH_MODE=dev`,
/// no `X-Dev-User` override) sees it at `GET /me` with no extra setup.
pub async fn ensure_dev_workspace(control: &ControlPool) -> anyhow::Result<Uuid> {
    let profile =
        profiles::upsert_from_jwt(control, config::DEFAULT_DEV_USER_ID, DEV_USER_EMAIL).await?;
    if let Some(existing) = workspaces::find_by_name(control, JACKS_WORKSPACE_NAME).await? {
        return Ok(existing.id);
    }
    let workspace = workspaces::create(control, JACKS_WORKSPACE_NAME, profile.id).await?;
    Ok(workspace.id)
}

/// Create the saved queries and the "Sales" app definition for a workspace,
/// if they don't already exist.
pub async fn seed_control_plane(control: &ControlPool, workspace_id: Uuid) -> anyhow::Result<()> {
    for query in sales_saved_queries() {
        if saved_queries::find_by_key(control, workspace_id, &query.key)
            .await?
            .is_none()
        {
            saved_queries::create(control, workspace_id, query).await?;
        }
    }
    if app_definitions::find_by_key(control, workspace_id, "sales")
        .await?
        .is_none()
    {
        let doc = sales_app_definition();
        app_definition::validate(control, workspace_id, &doc).await?;
        let app = app_definitions::create(control, workspace_id, "sales", "Sales").await?;
        let version =
            app_definitions::create_version(control, app.id, serde_json::to_value(&doc)?, None)
                .await?;
        app_definitions::publish(control, workspace_id, app.id, version.id).await?;
    }
    Ok(())
}

/// Derived-table SQL computing the same `stale` flag as
/// `opportunities::list_views`'s `VIEW_SQL`, duplicated here (rather than
/// shared) because it is deliberately a plain, inspectable saved query --
/// every chart exposes its SQL via the `</>` button.
const STALE_DEALS_INNER_SQL: &str = "\
    SELECT o.*, \
           (o.stage NOT IN ('won', 'lost') \
            AND (COALESCE(o.last_touch_at, o.created_at) < datetime('now', '-14 days') \
                 OR EXISTS (SELECT 1 FROM tasks t WHERE t.opportunity_id = o.id AND t.done = 0 \
                            AND t.due_at IS NOT NULL AND date(t.due_at) < date('now')))) AS stale \
    FROM opportunities o";

fn sales_saved_queries() -> Vec<NewSavedQuery> {
    let no_params = json!([]);
    let opportunity_id_param = json!([{"name": "opportunity_id", "type": "number"}]);
    vec![
        NewSavedQuery {
            key: "pipeline_by_stage".to_owned(),
            name: "Pipeline by stage".to_owned(),
            description: Some("Open deals grouped by stage, for the kanban board.".to_owned()),
            sql_text: "SELECT id, company, stage, value_cents, next_step, last_touch_at \
                       FROM opportunities WHERE stage NOT IN ('won', 'lost') \
                       ORDER BY stage, updated_at DESC"
                .to_owned(),
            params: no_params.clone(),
        },
        NewSavedQuery {
            key: "deals_table".to_owned(),
            name: "All deals".to_owned(),
            description: Some("Every deal, most recently updated first.".to_owned()),
            sql_text: "SELECT id, company, stage, value_cents, contact_name, contact_email, \
                       next_step, last_touch_at, updated_at FROM opportunities \
                       ORDER BY updated_at DESC"
                .to_owned(),
            params: no_params.clone(),
        },
        NewSavedQuery {
            key: "stale_deals".to_owned(),
            name: "Stale deals".to_owned(),
            description: Some(
                "Open deals untouched in 14+ days, or with an overdue task.".to_owned(),
            ),
            sql_text: format!(
                "SELECT * FROM ({STALE_DEALS_INNER_SQL}) WHERE stale = 1 \
                 ORDER BY COALESCE(last_touch_at, created_at) ASC"
            ),
            params: no_params.clone(),
        },
        NewSavedQuery {
            key: "stale_deals_count".to_owned(),
            name: "Stale deal count".to_owned(),
            description: Some("Count of stale deals, for the metric card.".to_owned()),
            sql_text: format!("SELECT COUNT(*) AS value FROM ({STALE_DEALS_INNER_SQL}) WHERE stale = 1"),
            params: no_params.clone(),
        },
        NewSavedQuery {
            key: "pipeline_value_by_stage".to_owned(),
            name: "Pipeline value by stage".to_owned(),
            description: Some("Open deal count and value, grouped by stage.".to_owned()),
            sql_text: "SELECT stage, COUNT(*) AS deal_count, COALESCE(SUM(value_cents), 0) AS total_value_cents \
                       FROM opportunities WHERE stage NOT IN ('won', 'lost') GROUP BY stage"
                .to_owned(),
            params: no_params.clone(),
        },
        NewSavedQuery {
            key: "deal_detail".to_owned(),
            name: "Deal detail".to_owned(),
            description: Some("One deal by id.".to_owned()),
            sql_text: "SELECT * FROM opportunities WHERE id = :opportunity_id".to_owned(),
            params: opportunity_id_param.clone(),
        },
        NewSavedQuery {
            key: "deal_timeline".to_owned(),
            name: "Deal timeline".to_owned(),
            description: Some("A deal's timeline entries, newest first.".to_owned()),
            sql_text: "SELECT * FROM timeline_entries WHERE opportunity_id = :opportunity_id \
                       ORDER BY created_at DESC"
                .to_owned(),
            params: opportunity_id_param.clone(),
        },
        NewSavedQuery {
            key: "deal_proposals".to_owned(),
            name: "Deal proposals".to_owned(),
            description: Some("A deal's pending field-change proposals.".to_owned()),
            sql_text: "SELECT * FROM field_changes WHERE opportunity_id = :opportunity_id \
                       AND status = 'proposed' ORDER BY created_at"
                .to_owned(),
            params: opportunity_id_param.clone(),
        },
        NewSavedQuery {
            key: "deal_tasks".to_owned(),
            name: "Deal tasks".to_owned(),
            description: Some("A deal's tasks, open first.".to_owned()),
            sql_text: "SELECT * FROM tasks WHERE opportunity_id = :opportunity_id \
                       ORDER BY done, due_at IS NULL, due_at"
                .to_owned(),
            params: opportunity_id_param,
        },
        NewSavedQuery {
            key: "lead_queue".to_owned(),
            name: "Lead queue".to_owned(),
            description: Some("New (or un-snoozed) leads, highest fit score first.".to_owned()),
            sql_text: "SELECT lc.id, lc.fit_score, lc.fit_reason, lc.status, lc.snoozed_until, \
                       c.name AS company_name, c.website, c.phone \
                       FROM lead_candidates lc JOIN companies c ON c.id = lc.company_id \
                       WHERE lc.status = 'new' \
                          OR (lc.status = 'snoozed' \
                              AND (lc.snoozed_until IS NULL OR lc.snoozed_until <= datetime('now'))) \
                       ORDER BY lc.fit_score DESC"
                .to_owned(),
            params: no_params,
        },
    ]
}

fn query_binding(key: &str) -> app_definition::QueryBinding {
    app_definition::QueryBinding {
        query: key.to_owned(),
        params: std::collections::BTreeMap::new(),
    }
}

fn query_binding_with(key: &str, params: &[(&str, &str)]) -> app_definition::QueryBinding {
    app_definition::QueryBinding {
        query: key.to_owned(),
        params: params
            .iter()
            .map(|(k, v)| ((*k).to_owned(), serde_json::Value::String((*v).to_owned())))
            .collect(),
    }
}

fn action_binding(key: &str) -> app_definition::ActionBinding {
    app_definition::ActionBinding {
        action: key.to_owned(),
        params: std::collections::BTreeMap::new(),
    }
}

fn action_binding_with(key: &str, params: &[(&str, &str)]) -> app_definition::ActionBinding {
    app_definition::ActionBinding {
        action: key.to_owned(),
        params: params
            .iter()
            .map(|(k, v)| ((*k).to_owned(), serde_json::Value::String((*v).to_owned())))
            .collect(),
    }
}

fn props(entries: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// The dashboard Jack gets from `cargo run -- seed`: a pipeline kanban by
/// stage, a deals table, a stale-deals metric card and alert list, a
/// pipeline-value chart, a deal detail page (fields, timeline, a paste
/// form, proposals, tasks) and a lead queue. Committed as a fixture too at
/// `fixtures/apps/sales.json`, since the frontend reuses it; a test checks
/// they match.
#[expect(
    clippy::too_many_lines,
    reason = "a literal app definition reads best as one top-to-bottom struct literal"
)]
pub fn sales_app_definition() -> AppDefinitionDoc {
    use app_definition::{Component, ComponentKind, LayoutNode, Page};

    AppDefinitionDoc {
        version: 1,
        pages: vec![
            Page {
                id: "pipeline".to_owned(),
                path: "/".to_owned(),
                title: "Pipeline".to_owned(),
                layout: LayoutNode::Column {
                    children: vec![
                        LayoutNode::Component {
                            component_id: "pipeline_kanban".to_owned(),
                        },
                        LayoutNode::Row {
                            children: vec![
                                LayoutNode::Component {
                                    component_id: "deals_table".to_owned(),
                                },
                                LayoutNode::Column {
                                    children: vec![
                                        LayoutNode::Component {
                                            component_id: "stale_metric".to_owned(),
                                        },
                                        LayoutNode::Component {
                                            component_id: "stale_alerts".to_owned(),
                                        },
                                    ],
                                },
                            ],
                        },
                        LayoutNode::Component {
                            component_id: "pipeline_value_chart".to_owned(),
                        },
                    ],
                },
                components: vec![
                    Component {
                        id: "pipeline_kanban".to_owned(),
                        kind: ComponentKind::Kanban,
                        title: Some("Pipeline".to_owned()),
                        query: Some(query_binding("pipeline_by_stage")),
                        action: Some(action_binding("opportunity.update_stage")),
                        props: props(&[
                            ("group_field", json!("stage")),
                            ("card_title_field", json!("company")),
                        ]),
                    },
                    Component {
                        id: "deals_table".to_owned(),
                        kind: ComponentKind::Table,
                        title: Some("Deals".to_owned()),
                        query: Some(query_binding("deals_table")),
                        action: None,
                        props: serde_json::Map::new(),
                    },
                    Component {
                        id: "stale_metric".to_owned(),
                        kind: ComponentKind::MetricCard,
                        title: Some("Stale deals".to_owned()),
                        query: Some(query_binding("stale_deals_count")),
                        action: None,
                        props: props(&[("value_field", json!("value"))]),
                    },
                    Component {
                        id: "stale_alerts".to_owned(),
                        kind: ComponentKind::List,
                        title: Some("Needs attention".to_owned()),
                        query: Some(query_binding("stale_deals")),
                        action: None,
                        props: props(&[("item_title_field", json!("company"))]),
                    },
                    Component {
                        id: "pipeline_value_chart".to_owned(),
                        kind: ComponentKind::Chart,
                        title: Some("Pipeline value by stage".to_owned()),
                        query: Some(query_binding("pipeline_value_by_stage")),
                        action: None,
                        props: props(&[
                            ("chart_type", json!("bar")),
                            ("encoding", json!({"x": "stage", "y": "total_value_cents"})),
                        ]),
                    },
                ],
            },
            Page {
                id: "deal_detail".to_owned(),
                path: "/deals/:opportunity_id".to_owned(),
                title: "Deal".to_owned(),
                layout: LayoutNode::Column {
                    children: vec![
                        LayoutNode::Component {
                            component_id: "deal_detail_view".to_owned(),
                        },
                        LayoutNode::Component {
                            component_id: "deal_timeline".to_owned(),
                        },
                        LayoutNode::Component {
                            component_id: "paste_form".to_owned(),
                        },
                        LayoutNode::Component {
                            component_id: "deal_proposals".to_owned(),
                        },
                        LayoutNode::Component {
                            component_id: "deal_tasks".to_owned(),
                        },
                    ],
                },
                components: vec![
                    Component {
                        id: "deal_detail_view".to_owned(),
                        kind: ComponentKind::DetailView,
                        title: Some("Deal".to_owned()),
                        query: Some(query_binding_with(
                            "deal_detail",
                            &[("opportunity_id", "$route.opportunity_id")],
                        )),
                        action: None,
                        props: props(&[(
                            "fields",
                            json!([
                                "company",
                                "contact_name",
                                "contact_email",
                                "value_cents",
                                "stage",
                                "next_step",
                                "last_touch_at"
                            ]),
                        )]),
                    },
                    Component {
                        id: "deal_timeline".to_owned(),
                        kind: ComponentKind::List,
                        title: Some("Timeline".to_owned()),
                        query: Some(query_binding_with(
                            "deal_timeline",
                            &[("opportunity_id", "$route.opportunity_id")],
                        )),
                        action: None,
                        props: props(&[("item_title_field", json!("summary"))]),
                    },
                    Component {
                        id: "paste_form".to_owned(),
                        kind: ComponentKind::Form,
                        title: Some("Paste an email or notes".to_owned()),
                        query: None,
                        action: Some(action_binding_with(
                            "paste.submit",
                            &[("opportunity_id", "$route.opportunity_id")],
                        )),
                        props: props(&[("fields", json!([{"name": "text", "type": "textarea"}]))]),
                    },
                    Component {
                        id: "deal_proposals".to_owned(),
                        kind: ComponentKind::List,
                        title: Some("Proposed changes".to_owned()),
                        query: Some(query_binding_with(
                            "deal_proposals",
                            &[("opportunity_id", "$route.opportunity_id")],
                        )),
                        action: Some(action_binding("proposal.accept")),
                        props: props(&[("item_title_field", json!("field"))]),
                    },
                    Component {
                        id: "deal_tasks".to_owned(),
                        kind: ComponentKind::List,
                        title: Some("Tasks".to_owned()),
                        query: Some(query_binding_with(
                            "deal_tasks",
                            &[("opportunity_id", "$route.opportunity_id")],
                        )),
                        action: Some(action_binding("task.complete")),
                        props: props(&[("item_title_field", json!("title"))]),
                    },
                ],
            },
            Page {
                id: "lead_queue".to_owned(),
                path: "/leads".to_owned(),
                title: "Lead queue".to_owned(),
                layout: LayoutNode::Component {
                    component_id: "lead_queue_table".to_owned(),
                },
                components: vec![Component {
                    id: "lead_queue_table".to_owned(),
                    kind: ComponentKind::Table,
                    title: Some("Lead queue".to_owned()),
                    query: Some(query_binding("lead_queue")),
                    action: Some(action_binding("lead.accept")),
                    props: props(&[(
                        "columns",
                        json!(["company_name", "fit_score", "website", "phone"]),
                    )]),
                }],
            },
        ],
    }
}

#[cfg(test)]
mod control_plane_tests {
    use super::*;

    #[test]
    fn committed_sales_fixture_matches_generated() -> anyhow::Result<()> {
        let generated = serde_json::to_string_pretty(&sales_app_definition())? + "\n";
        let committed = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/apps/sales.json"
        ))?;
        assert_eq!(
            generated, committed,
            "fixtures/apps/sales.json is stale; regenerate it from `seed::sales_app_definition()`"
        );
        Ok(())
    }
}
