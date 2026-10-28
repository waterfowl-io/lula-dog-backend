//! `actor_bench crash-child <scenario> <db> <args...>`: the process the
//! `crash` mode's parent spawns and `kill -9`s mid-transaction. Never
//! returns normally in the `single`/`mid_batch` scenarios -- it is killed
//! before its one command/batch call resolves.

use lula_dog_backend::actor::{ActorConfig, WorkspaceActor};
use serde_json::json;
use uuid::Uuid;

use crate::common::loop_batch_commands;

pub async fn run_child(args: &[String]) -> anyhow::Result<()> {
    let scenario = args.first().map(String::as_str).unwrap_or_default();
    let db = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("missing db path"))?;
    match scenario {
        "single" => single(db, args.get(2), args.get(3)).await,
        "mid_batch" => mid_batch(db, args.get(2), args.get(3)).await,
        "between_batches" => between_batches(db, args.get(2), args.get(3..).unwrap_or(&[])).await,
        other => Err(anyhow::anyhow!("unknown crash-child scenario: {other}")),
    }
}

fn parse_uuid(s: Option<&String>) -> anyhow::Result<Uuid> {
    let s = s.ok_or_else(|| anyhow::anyhow!("missing id argument"))?;
    Uuid::parse_str(s).map_err(|e| anyhow::anyhow!("bad uuid {s:?}: {e}"))
}

fn parse_i64(s: Option<&String>) -> anyhow::Result<i64> {
    let s = s.ok_or_else(|| anyhow::anyhow!("missing integer argument"))?;
    s.parse::<i64>()
        .map_err(|e| anyhow::anyhow!("bad integer {s:?}: {e}"))
}

/// One `lead.accept`, with `LULA_ACTOR_CRASH_DELAY_MS` set by the parent so
/// it pauses (after writing, before `COMMIT`) long enough to be killed.
async fn single(db: &str, id: Option<&String>, lead_id: Option<&String>) -> anyhow::Result<()> {
    let id = parse_uuid(id)?;
    let lead_id = parse_i64(lead_id)?;
    let handle = WorkspaceActor::spawn(db, ActorConfig::default()).await?;
    let _ = handle
        .send(id, "lead.accept", json!({"lead_id": lead_id}))
        .await;
    Ok(())
}

/// One 500-command `send_batch`, same crash-delay hook as `single`.
async fn mid_batch(
    db: &str,
    id: Option<&String>,
    opportunity_id: Option<&String>,
) -> anyhow::Result<()> {
    let id = parse_uuid(id)?;
    let opportunity_id = parse_i64(opportunity_id)?;
    let handle = WorkspaceActor::spawn(db, ActorConfig::default()).await?;
    let commands = loop_batch_commands(opportunity_id, 0, 500);
    let _ = handle.send_batch(id, commands).await;
    Ok(())
}

/// Commits several batches in a loop at normal speed (no crash delay) --
/// the parent kills this one at a random point between commits.
async fn between_batches(
    db: &str,
    opportunity_id: Option<&String>,
    ids: &[String],
) -> anyhow::Result<()> {
    let opportunity_id = parse_i64(opportunity_id)?;
    let handle = WorkspaceActor::spawn(db, ActorConfig::default()).await?;
    for (index, raw_id) in ids.iter().enumerate() {
        let id =
            Uuid::parse_str(raw_id).map_err(|e| anyhow::anyhow!("bad uuid {raw_id:?}: {e}"))?;
        let commands = loop_batch_commands(opportunity_id, index, 20);
        handle.send_batch(id, commands).await?;
        println!("COMMITTED {index}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
    Ok(())
}
