//! `cargo run -- seed`: fill an empty database with clearly fake
//! packaging-sales data. Every company, person and address is invented and
//! uses the reserved `.example` domain.
//!
//! Dates are relative to now, so the stale deals stay stale and the fresh
//! ones fresh whenever it runs. Timeline entries go through the real paste
//! flow with the stub extractor, so the seed also leaves pending proposals,
//! auto-filled fields and suggested tasks behind exactly as the API would.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use crate::app_definition::AppDefinitionDoc;
use crate::companies::{self, NewCompany};
use crate::config;
use crate::control::app_definitions;
use crate::control::saved_queries::{self, NewSavedQuery};
use crate::control::workspaces;
use crate::control::{ControlPool, profiles};
use crate::extractor::{Extractor, StubExtractor};
use crate::leads;
use crate::opportunities::{self, NewOpportunity, Stage};
use crate::target_profiles;
use crate::{app_definition, paste, tasks};

const DEV_USER_EMAIL: &str = "jack@lula.dog.example";
const JACKS_WORKSPACE_NAME: &str = "Jack's Packaging";

pub struct SeedSummary {
    pub opportunities: usize,
    pub leads: usize,
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

/// Fill an empty workspace database with the example deals/leads, and
/// upgrade an already-seeded one.
///
/// Re-running this against a developer's existing `DATA_DIR` is the normal
/// case (not a fresh database), so each piece is seeded independently and
/// idempotently rather than refusing outright:
/// - the example deals/timeline only ever seed once (an empty
///   `opportunities` table), since they're hand-tuned to read as fresh
///   relative to "now" and re-inserting them on top of real data would be
///   nonsensical;
/// - the example leads/companies backfill whenever `lead_candidates` is
///   empty, so a database seeded before leads existed picks them up on the
///   next `cargo run -- seed` without duplicating on further runs.
pub async fn run(pool: &SqlitePool, lead_min_fit_score: i64) -> anyhow::Result<SeedSummary> {
    let mut conn = pool.acquire().await?;
    target_profiles::seed_default(&mut conn, lead_min_fit_score).await?;
    let profile = target_profiles::first(&mut conn)
        .await?
        .ok_or_else(|| anyhow::anyhow!("just seeded a default target profile"))?;

    let existing_opportunities: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM opportunities")
        .fetch_one(&mut *conn)
        .await?;
    let opportunities = if existing_opportunities > 0 {
        tracing::info!(
            existing_opportunities,
            "opportunities already seeded; skipping the example deal/timeline seed \
             (delete the workspace database to re-seed deals from scratch)"
        );
        0
    } else {
        seed_opportunities(pool, &mut conn).await?
    };

    let existing_leads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lead_candidates")
        .fetch_one(&mut *conn)
        .await?;
    let leads = if existing_leads > 0 {
        tracing::info!(
            existing_leads,
            "lead_candidates already seeded; skipping example leads"
        );
        0
    } else {
        seed_leads(&mut conn, profile.id).await?
    };

    Ok(SeedSummary {
        opportunities,
        leads,
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "seed data reads best as one top-to-bottom list of deals"
)]
async fn seed_opportunities(
    pool: &SqlitePool,
    conn: &mut SqliteConnection,
) -> anyhow::Result<usize> {
    let extractor: Arc<dyn Extractor> = Arc::new(StubExtractor);
    let mut count = 0;

    // 1. Corrugated mailer boxes, proposal out, touched 3 days ago.
    let northwind = opportunities::insert(
        conn,
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
        conn,
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
        conn,
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
        conn,
        ridgeline.id,
        "Send GMA 48x40 heat-treated pallet pricing",
        Some(&date(days_ago(3))),
        None,
    )
    .await?;

    // 4. Repeat order of RSC boxes, won a month ago (won deals are never stale).
    opportunities::insert(
        conn,
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
        conn,
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
        conn,
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
        conn,
        prairie.id,
        "Confirm repeat order: 10,000 12x15.5 poly mailers",
        Some(&date(days_ago(-5))),
        None,
    )
    .await?;

    // 7. Lost to another supplier.
    let coastal = opportunities::insert(
        conn,
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
        conn,
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

    Ok(count)
}

/// The lead queue: a handful of clearly-fake companies (`.example` domains,
/// same convention as the example opportunities) queued as lead candidates
/// so `/leads` isn't empty out of the box.
async fn seed_leads(conn: &mut SqliteConnection, profile_id: i64) -> anyhow::Result<usize> {
    let lead_defs: &[(&str, &str, &str, i64, &str)] = &[
        (
            "Bluebird Coffee Roasters",
            "bluebirdcoffee.example",
            "Food & beverage",
            86,
            "Ships physical goods, similar size to existing roaster accounts, no packaging vendor on file.",
        ),
        (
            "Fernhill Candle Co.",
            "fernhillcandle.example",
            "Manufacturing",
            78,
            "Growing DTC brand, currently hand-packing -- good fit for retail boxes.",
        ),
        (
            "Northside Print Shop",
            "northsideprint.example",
            "Distribution",
            61,
            "Ships physical goods but low order volume so far; worth a qualifying call.",
        ),
        (
            "Harbor & Oak Furniture",
            "harborandoak.example",
            "Manufacturing",
            54,
            "Bulk/industrial packaging, lower margin fit, but a large potential volume.",
        ),
    ];
    let mut lead_count = 0;
    for (name, domain, category, fit_score, fit_reason) in lead_defs {
        let company = companies::upsert(
            conn,
            NewCompany {
                name: (*name).to_owned(),
                jurisdiction: "IL".to_owned(),
                website: Some(format!("https://{domain}")),
                category: Some((*category).to_owned()),
                ..NewCompany::default()
            },
        )
        .await?;
        if leads::queue_candidate(conn, company.id, profile_id, *fit_score, Some(fit_reason))
            .await?
            .is_some()
        {
            lead_count += 1;
        }
    }

    Ok(lead_count)
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

/// Create (or converge) the saved queries and the "Sales" app definition
/// for a workspace. Safe to call repeatedly against a workspace that
/// already has them: saved queries are upserted by key, and the sales app
/// gets a new published version whenever `sales_app_definition()` differs
/// from what's currently published -- so re-running `cargo run -- seed`
/// against an existing dev database picks up schema/definition changes
/// without touching the workspace's SQLite data.
pub async fn seed_control_plane(control: &ControlPool, workspace_id: Uuid) -> anyhow::Result<()> {
    for query in sales_saved_queries() {
        saved_queries::upsert(control, workspace_id, query).await?;
    }
    let doc = sales_app_definition();
    app_definition::validate(control, workspace_id, &doc).await?;
    let generated = serde_json::to_value(&doc)?;
    match app_definitions::find_by_key(control, workspace_id, "sales").await? {
        None => {
            let app = app_definitions::create(control, workspace_id, "sales", "Sales").await?;
            let version = app_definitions::create_version(control, app.id, generated, None).await?;
            app_definitions::publish(control, workspace_id, app.id, version.id).await?;
        }
        Some(app) => {
            let (_, published) =
                app_definitions::fetch_published_or_latest(control, workspace_id, app.id).await?;
            if published.definition != generated {
                let version =
                    app_definitions::create_version(control, app.id, generated, None).await?;
                app_definitions::publish(control, workspace_id, app.id, version.id).await?;
            }
        }
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

#[expect(
    clippy::too_many_lines,
    reason = "a literal list of saved queries reads best as one top-to-bottom vec"
)]
fn sales_saved_queries() -> Vec<NewSavedQuery> {
    let no_params = json!([]);
    let opportunity_id_param = json!([{"name": "opportunity_id", "type": "number"}]);
    vec![
        NewSavedQuery {
            key: "pipeline_by_stage".to_owned(),
            name: "Pipeline by stage".to_owned(),
            description: Some(
                "Every deal grouped by stage, for the kanban board, with each card's open \
                 task count, staleness and pending-suggestion count."
                    .to_owned(),
            ),
            sql_text: "SELECT o.id, o.company, o.stage, o.value_cents, o.next_step, \
                       o.last_touch_at, \
                       (SELECT COUNT(*) FROM tasks t WHERE t.opportunity_id = o.id \
                        AND t.done = 0) AS open_task_count, \
                       (o.stage NOT IN ('won', 'lost') \
                        AND (COALESCE(o.last_touch_at, o.created_at) < datetime('now', '-14 days') \
                             OR EXISTS (SELECT 1 FROM tasks t WHERE t.opportunity_id = o.id \
                                        AND t.done = 0 AND t.due_at IS NOT NULL \
                                        AND date(t.due_at) < date('now')))) AS stale, \
                       (SELECT COUNT(*) FROM field_changes fc WHERE fc.opportunity_id = o.id \
                        AND fc.status = 'proposed') AS pending_proposal_count \
                       FROM opportunities o ORDER BY o.stage, o.updated_at DESC"
                .to_owned(),
            params: no_params.clone(),
        },
        NewSavedQuery {
            key: "pipeline_open_value".to_owned(),
            name: "Open pipeline value".to_owned(),
            description: Some("Total value and count of deals not yet won or lost.".to_owned()),
            sql_text: "SELECT COALESCE(SUM(value_cents), 0) AS value, COUNT(*) AS deal_count \
                       FROM opportunities WHERE stage NOT IN ('won', 'lost')"
                .to_owned(),
            params: no_params.clone(),
        },
        NewSavedQuery {
            key: "pipeline_won_this_month".to_owned(),
            name: "Won this month".to_owned(),
            description: Some(
                "Total value and count of deals won since the start of this month.".to_owned(),
            ),
            sql_text: "SELECT COALESCE(SUM(value_cents), 0) AS value, COUNT(*) AS deal_count \
                       FROM opportunities \
                       WHERE stage = 'won' AND updated_at >= date('now', 'start of month')"
                .to_owned(),
            params: no_params.clone(),
        },
        NewSavedQuery {
            key: "pipeline_suggestions_waiting".to_owned(),
            name: "Suggestions waiting".to_owned(),
            description: Some(
                "Count of pending field-change proposals, and how many deals they're on."
                    .to_owned(),
            ),
            sql_text: "SELECT COUNT(*) AS value, COUNT(DISTINCT opportunity_id) AS deal_count \
                       FROM field_changes WHERE status = 'proposed'"
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
                       c.name AS company_name, c.website, c.phone, c.category \
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

/// A component's named `actions` map, e.g. `{"accept": ..., "reject": ...}`.
fn actions_map(
    entries: Vec<(&str, app_definition::ActionBinding)>,
) -> std::collections::BTreeMap<String, app_definition::ActionBinding> {
    entries
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
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

    let badge_colors = json!({
        "lead": {"bg": "#F8F1E9", "text": "#6E6966", "dot": "#968D86"},
        "qualified": {"bg": "#E3EEFB", "text": "#2A5DA6", "dot": "#3F7FD6"},
        "proposal": {"bg": "#FCF0D6", "text": "#8A5E0F", "dot": "#E5A228"},
        "won": {"bg": "#E3F3EA", "text": "#1F7650", "dot": "#2E9E6A"},
        "lost": {"bg": "#F8F1E9", "text": "#6E6966", "dot": "#968D86"},
    });

    AppDefinitionDoc {
        version: 1,
        brand: Some(app_definition::Brand {
            title: "Jack's Sales".to_owned(),
            mark: Some("lula".to_owned()),
            chat: Some(app_definition::ChatBrand {
                status: Some("Building Jack's Sales".to_owned()),
                placeholder: Some("Ask Lula to change this app…".to_owned()),
            }),
        }),
        nav: vec![
            app_definition::NavItem {
                label: "Pipeline".to_owned(),
                path: "/".to_owned(),
            },
            app_definition::NavItem {
                label: "Leads".to_owned(),
                path: "/leads".to_owned(),
            },
        ],
        pages: vec![
            Page {
                id: "pipeline".to_owned(),
                path: "/".to_owned(),
                title: "Pipeline".to_owned(),
                layout: LayoutNode::Column {
                    children: vec![
                        LayoutNode::Row {
                            children: vec![LayoutNode::Component {
                                component_id: "new_deal_form".to_owned(),
                            }],
                        },
                        LayoutNode::Row {
                            children: vec![
                                LayoutNode::Component {
                                    component_id: "metric_open_value".to_owned(),
                                },
                                LayoutNode::Component {
                                    component_id: "metric_won_this_month".to_owned(),
                                },
                                LayoutNode::Component {
                                    component_id: "stale_metric".to_owned(),
                                },
                                LayoutNode::Component {
                                    component_id: "metric_suggestions_waiting".to_owned(),
                                },
                            ],
                        },
                        LayoutNode::Component {
                            component_id: "pipeline_kanban".to_owned(),
                        },
                        LayoutNode::Row {
                            children: vec![
                                LayoutNode::Column {
                                    children: vec![LayoutNode::Component {
                                        component_id: "deals_table".to_owned(),
                                    }],
                                },
                                LayoutNode::Column {
                                    children: vec![LayoutNode::Component {
                                        component_id: "stale_alerts".to_owned(),
                                    }],
                                },
                            ],
                        },
                        LayoutNode::Section {
                            title: Some("Pipeline value by stage".to_owned()),
                            children: vec![LayoutNode::Component {
                                component_id: "pipeline_value_chart".to_owned(),
                            }],
                        },
                    ],
                },
                components: vec![
                    Component {
                        id: "new_deal_form".to_owned(),
                        kind: ComponentKind::Form,
                        title: Some("New deal".to_owned()),
                        query: None,
                        action: Some(action_binding("opportunity.create")),
                        actions: actions_map(vec![]),
                        props: props(&[
                            ("trigger_label", json!("New deal")),
                            ("button_label", json!("Add deal")),
                            (
                                "fields",
                                json!([
                                    {"name": "company", "type": "text", "label": "Company", "required": true},
                                    {"name": "contact_name", "type": "text", "label": "Contact"},
                                    {"name": "contact_email", "type": "text", "label": "Email"},
                                    {"name": "value_cents", "type": "currency", "label": "Value"},
                                    {"name": "next_step", "type": "text", "label": "Next step"},
                                ]),
                            ),
                        ]),
                    },
                    Component {
                        id: "metric_open_value".to_owned(),
                        kind: ComponentKind::MetricCard,
                        title: Some("Open pipeline".to_owned()),
                        query: Some(query_binding("pipeline_open_value")),
                        action: None,
                        actions: actions_map(vec![]),
                        props: props(&[
                            ("value_field", json!("value")),
                            ("format", json!("currency_cents")),
                            ("caption_field", json!("deal_count")),
                            ("caption_suffix", json!(" deal")),
                        ]),
                    },
                    Component {
                        id: "metric_won_this_month".to_owned(),
                        kind: ComponentKind::MetricCard,
                        title: Some("Won this month".to_owned()),
                        query: Some(query_binding("pipeline_won_this_month")),
                        action: None,
                        actions: actions_map(vec![]),
                        props: props(&[
                            ("value_field", json!("value")),
                            ("format", json!("currency_cents")),
                            ("caption_field", json!("deal_count")),
                            ("caption_suffix", json!(" deal")),
                        ]),
                    },
                    Component {
                        id: "stale_metric".to_owned(),
                        kind: ComponentKind::MetricCard,
                        title: Some("Needs a touch".to_owned()),
                        query: Some(query_binding("stale_deals_count")),
                        action: None,
                        actions: actions_map(vec![]),
                        props: props(&[
                            ("value_field", json!("value")),
                            ("caption", json!("no touch in 14+ days")),
                            ("accent", json!(true)),
                        ]),
                    },
                    Component {
                        id: "metric_suggestions_waiting".to_owned(),
                        kind: ComponentKind::MetricCard,
                        title: Some("Suggestions waiting".to_owned()),
                        query: Some(query_binding("pipeline_suggestions_waiting")),
                        action: None,
                        actions: actions_map(vec![]),
                        props: props(&[
                            ("value_field", json!("value")),
                            ("caption_field", json!("deal_count")),
                            ("caption_suffix", json!(" deal")),
                            ("accent", json!(true)),
                        ]),
                    },
                    Component {
                        id: "pipeline_kanban".to_owned(),
                        kind: ComponentKind::Kanban,
                        title: Some("Pipeline".to_owned()),
                        query: Some(query_binding("pipeline_by_stage")),
                        action: Some(action_binding("opportunity.update_stage")),
                        actions: actions_map(vec![]),
                        props: props(&[
                            ("group_field", json!("stage")),
                            ("card_title_field", json!("company")),
                            ("add_label", json!("Add deal")),
                            (
                                "columns",
                                json!([
                                    {"key": "lead", "label": "Lead", "color": "#968D86", "addable": true},
                                    {"key": "qualified", "label": "Qualified", "color": "#3F7FD6"},
                                    {"key": "proposal", "label": "Proposal", "color": "#E5A228"},
                                    {"key": "won", "label": "Won", "color": "#2E9E6A"},
                                    {"key": "lost", "label": "Lost", "color": "#C2B8AE"},
                                ]),
                            ),
                            (
                                "card",
                                json!({
                                    "title_field": "company",
                                    "value_field": "value_cents",
                                    "value_format": "currency_cents",
                                    "next_step_field": "next_step",
                                    "age_field": "last_touch_at",
                                    "open_tasks_field": "open_task_count",
                                    "stale_field": "stale",
                                    "suggestions_field": "pending_proposal_count",
                                    "link": {"page": "deal_detail", "param": "opportunity_id", "param_field": "id"},
                                }),
                            ),
                        ]),
                    },
                    Component {
                        id: "deals_table".to_owned(),
                        kind: ComponentKind::Table,
                        title: Some("Deals".to_owned()),
                        query: Some(query_binding("deals_table")),
                        action: None,
                        actions: actions_map(vec![]),
                        props: serde_json::Map::new(),
                    },
                    Component {
                        id: "stale_alerts".to_owned(),
                        kind: ComponentKind::List,
                        title: Some("Needs attention".to_owned()),
                        query: Some(query_binding("stale_deals")),
                        action: None,
                        actions: actions_map(vec![]),
                        props: props(&[("item_title_field", json!("company"))]),
                    },
                    Component {
                        id: "pipeline_value_chart".to_owned(),
                        kind: ComponentKind::Chart,
                        title: None,
                        query: Some(query_binding("pipeline_value_by_stage")),
                        action: None,
                        actions: actions_map(vec![]),
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
                        LayoutNode::Row {
                            children: vec![
                                LayoutNode::Column {
                                    children: vec![
                                        LayoutNode::Component {
                                            component_id: "deal_details_panel".to_owned(),
                                        },
                                        LayoutNode::Component {
                                            component_id: "deal_tasks".to_owned(),
                                        },
                                    ],
                                },
                                LayoutNode::Column {
                                    children: vec![
                                        LayoutNode::Component {
                                            component_id: "paste_form".to_owned(),
                                        },
                                        LayoutNode::Component {
                                            component_id: "deal_proposals".to_owned(),
                                        },
                                        LayoutNode::Component {
                                            component_id: "deal_timeline".to_owned(),
                                        },
                                    ],
                                },
                            ],
                        },
                    ],
                },
                components: vec![
                    Component {
                        id: "deal_detail_view".to_owned(),
                        kind: ComponentKind::DetailView,
                        title: None,
                        query: Some(query_binding_with(
                            "deal_detail",
                            &[("opportunity_id", "$route.opportunity_id")],
                        )),
                        action: None,
                        actions: actions_map(vec![]),
                        props: props(&[
                            ("display", json!("header")),
                            ("back_link", json!({"label": "Pipeline", "path": "/"})),
                            ("title_field", json!("company")),
                            ("badge_field", json!("stage")),
                            ("badge_format", json!("stage_pill")),
                            ("badge_colors", badge_colors),
                            (
                                "meta",
                                json!([
                                    {"icon": "user", "field": "contact_name"},
                                    {"icon": "clock", "field": "last_touch_at", "format": "relative_date", "prefix": "Last touch "},
                                ]),
                            ),
                            (
                                "actions",
                                json!([
                                    {
                                        "label": "Move to Won",
                                        "icon": "check",
                                        "variant": "primary",
                                        "action": {"action": "opportunity.update_stage", "params": {"stage": "won"}},
                                    },
                                ]),
                            ),
                        ]),
                    },
                    Component {
                        id: "deal_details_panel".to_owned(),
                        kind: ComponentKind::DetailView,
                        title: Some("Details".to_owned()),
                        query: Some(query_binding_with(
                            "deal_detail",
                            &[("opportunity_id", "$route.opportunity_id")],
                        )),
                        action: None,
                        actions: actions_map(vec![]),
                        props: props(&[
                            ("display", json!("panel")),
                            (
                                "fields",
                                json!([
                                    {"key": "value_cents", "label": "Value", "format": "currency_cents"},
                                    {"key": "stage", "label": "Stage", "format": "stage_pill"},
                                    {"key": "next_step", "label": "Next step"},
                                    {"key": "contact_name", "label": "Contact"},
                                    {"key": "contact_email", "label": "Email"},
                                    {"key": "last_touch_at", "label": "Last touch", "format": "relative_date"},
                                ]),
                            ),
                        ]),
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
                        actions: actions_map(vec![]),
                        props: props(&[
                            ("variant", json!("timeline")),
                            (
                                "item",
                                json!({"raw_field": "raw_text", "summary_field": "summary", "date_field": "occurred_at"}),
                            ),
                        ]),
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
                        actions: actions_map(vec![]),
                        props: props(&[
                            ("fields", json!([{"name": "text", "type": "textarea"}])),
                            (
                                "placeholder",
                                json!("Paste an email thread, call notes, or Slack message…"),
                            ),
                            ("button_label", json!("Add to deal")),
                        ]),
                    },
                    Component {
                        id: "deal_proposals".to_owned(),
                        kind: ComponentKind::List,
                        title: Some("Lula suggests {count} changes".to_owned()),
                        query: Some(query_binding_with(
                            "deal_proposals",
                            &[("opportunity_id", "$route.opportunity_id")],
                        )),
                        action: None,
                        actions: actions_map(vec![
                            ("accept", action_binding("proposal.accept")),
                            ("reject", action_binding("proposal.reject")),
                        ]),
                        props: props(&[
                            ("variant", json!("proposals")),
                            ("hidden_when_empty", json!(true)),
                            (
                                "item",
                                json!({"field_field": "field", "old_field": "old_value", "new_field": "new_value"}),
                            ),
                        ]),
                    },
                    Component {
                        id: "deal_tasks".to_owned(),
                        kind: ComponentKind::List,
                        title: Some("To-dos".to_owned()),
                        query: Some(query_binding_with(
                            "deal_tasks",
                            &[("opportunity_id", "$route.opportunity_id")],
                        )),
                        action: None,
                        actions: actions_map(vec![
                            ("complete", action_binding("task.complete")),
                            (
                                "create",
                                action_binding_with(
                                    "task.create",
                                    &[("opportunity_id", "$route.opportunity_id")],
                                ),
                            ),
                        ]),
                        props: props(&[
                            ("variant", json!("tasks")),
                            ("add_label", json!("Add")),
                            (
                                "item",
                                json!({
                                    "title_field": "title",
                                    "done_field": "done",
                                    "due_field": "due_at",
                                    "source_field": "source_entry_id",
                                }),
                            ),
                        ]),
                    },
                ],
            },
            Page {
                id: "lead_queue".to_owned(),
                path: "/leads".to_owned(),
                title: "Leads".to_owned(),
                layout: LayoutNode::Column {
                    children: vec![LayoutNode::Component {
                        component_id: "lead_queue_list".to_owned(),
                    }],
                },
                components: vec![Component {
                    id: "lead_queue_list".to_owned(),
                    kind: ComponentKind::List,
                    title: Some("Lead queue".to_owned()),
                    query: Some(query_binding("lead_queue")),
                    action: None,
                    actions: actions_map(vec![
                        ("accept", action_binding("lead.accept")),
                        ("reject", action_binding("lead.reject")),
                        ("snooze", action_binding("lead.snooze")),
                    ]),
                    props: props(&[
                        ("variant", json!("leads")),
                        (
                            "item",
                            json!({
                                "title_field": "company_name",
                                "score_field": "fit_score",
                                "reason_field": "fit_reason",
                                "category_field": "category",
                            }),
                        ),
                    ]),
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
