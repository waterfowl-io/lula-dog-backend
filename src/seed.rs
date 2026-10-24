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
use sqlx::SqlitePool;

use crate::extractor::{Extractor, StubExtractor};
use crate::opportunities::{self, NewOpportunity, Stage};
use crate::{paste, tasks};

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
