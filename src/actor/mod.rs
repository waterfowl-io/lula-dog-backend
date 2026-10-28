//! The workspace actor (`actor-design.md`): one tokio task per workspace
//! owning the only write `SqliteConnection` for that workspace's file.
//! Commands are today's `crate::actions::dispatch_in` logic, applied inside
//! `BEGIN IMMEDIATE ... COMMIT` together with an idempotency row
//! (`_lula_commands`) and a change-feed row (`_lula_outbox`), so a crash
//! leaves either everything or nothing.
//!
//! This is a local, single-node spike: no Postgres lease, no forwarding, no
//! Litestream wiring. See `actor-design.md` §§0, 1, 4, 5, 8 for the full
//! design and what's deliberately out of scope here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sqlx::{Connection, FromRow, SqliteConnection};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use uuid::Uuid;

use crate::actions::{self, Undo};
use crate::error::{AppError, AppResult};

/// Mailbox and batching tuning. `Default` matches the design doc's
/// defaults: a 256-deep mailbox, group commit off, a 10-minute idle sleep.
#[derive(Debug, Clone)]
pub struct ActorConfig {
    pub mailbox_capacity: usize,
    pub idle_timeout: Duration,
    /// When true, the actor drains up to `group_commit_max` queued
    /// interactive commands and applies them in one transaction (one
    /// `SAVEPOINT` per command), replying to each only after the outer
    /// `COMMIT`.
    pub group_commit: bool,
    pub group_commit_max: usize,
}

impl Default for ActorConfig {
    fn default() -> Self {
        Self {
            mailbox_capacity: 256,
            idle_timeout: Duration::from_secs(600),
            group_commit: false,
            group_commit_max: 32,
        }
    }
}

/// What a command (or batch) returns to its caller.
#[derive(Debug, Clone)]
pub struct Reply {
    pub result: Value,
    pub undo: Option<Undo>,
    /// `true` when this id had already been applied; `result`/`undo` are
    /// the stored reply from the first application, replayed verbatim.
    pub deduped: bool,
    /// The `_lula_outbox.seq` this command's row landed at, or `-1` for a
    /// dedupe hit (no new outbox row is written on replay).
    pub outbox_seq: i64,
}

struct CmdMsg {
    id: Uuid,
    action: String,
    args: Value,
    reply: oneshot::Sender<AppResult<Reply>>,
}

struct BatchMsg {
    id: Uuid,
    commands: Vec<(String, Value)>,
    reply: oneshot::Sender<AppResult<Reply>>,
}

/// A cloneable reference to a running actor's mailbox. Sending on a handle
/// whose actor has gone to sleep (idle eviction, or a crash) fails fast
/// with a 503-style error; `ActorRegistry` is what actually re-spawns.
#[derive(Clone)]
pub struct WorkspaceHandle {
    interactive_tx: mpsc::Sender<CmdMsg>,
    batch_tx: mpsc::Sender<BatchMsg>,
    outbox: broadcast::Sender<i64>,
}

fn actor_unavailable() -> AppError {
    AppError::ServiceUnavailable("workspace actor is not running".to_owned())
}

impl WorkspaceHandle {
    /// Send one interactive command and wait for its reply.
    pub async fn send(&self, id: Uuid, action: impl Into<String>, args: Value) -> AppResult<Reply> {
        let (reply, rx) = oneshot::channel();
        let msg = CmdMsg {
            id,
            action: action.into(),
            args,
            reply,
        };
        self.interactive_tx
            .send(msg)
            .await
            .map_err(|_| actor_unavailable())?;
        rx.await.map_err(|_| actor_unavailable())?
    }

    /// Send a batch (one transaction, one dedupe row) and wait for its
    /// reply. Goes through the low-priority lane: the actor always drains
    /// interactive commands first.
    pub async fn send_batch(&self, id: Uuid, commands: Vec<(String, Value)>) -> AppResult<Reply> {
        let (reply, rx) = oneshot::channel();
        let msg = BatchMsg {
            id,
            commands,
            reply,
        };
        self.batch_tx
            .send(msg)
            .await
            .map_err(|_| actor_unavailable())?;
        rx.await.map_err(|_| actor_unavailable())?
    }

    /// `true` once the actor has evicted itself (idle timeout) or exited.
    pub fn is_closed(&self) -> bool {
        self.interactive_tx.is_closed()
    }

    /// `true` when `self` and `other` are handles to the same running
    /// actor task -- used to confirm a wake race spawned exactly one.
    pub fn same_actor(&self, other: &Self) -> bool {
        self.interactive_tx.same_channel(&other.interactive_tx)
    }

    /// Subscribe to the outbox's committed sequence numbers, newest first
    /// from the point of subscribing (no backlog replay -- a real `/events`
    /// endpoint would also read `_lula_outbox` for `seq > since`).
    pub fn subscribe(&self) -> broadcast::Receiver<i64> {
        self.outbox.subscribe()
    }
}

#[derive(FromRow)]
struct CommandRow {
    action: String,
    args_hash: String,
    reply_json: String,
    undo_json: Option<String>,
}

/// Not cryptographic -- just enough to reject a replayed id whose args
/// changed. `serde_json::Value`'s default map is a `BTreeMap` (the
/// `preserve_order` feature is off), so the same logical args always print
/// the same string.
fn hash_args(action: &str, args: &Value) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    action.hash(&mut hasher);
    args.to_string().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Bench/crash-test hook: when set, sleep this many milliseconds after a
/// command (or batch) has been applied but before its transaction commits,
/// so a `kill -9` lands mid-transaction. Prints `IN_TX` first so a parent
/// process driving the crash test knows the window has opened.
async fn maybe_crash_delay() {
    let Some(ms) = std::env::var("LULA_ACTOR_CRASH_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    else {
        return;
    };
    println!("IN_TX");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

async fn fetch_command(conn: &mut SqliteConnection, id: &str) -> AppResult<Option<CommandRow>> {
    Ok(sqlx::query_as::<_, CommandRow>(
        "SELECT action, args_hash, reply_json, undo_json FROM _lula_commands WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?)
}

/// Apply one command on an already-open transaction: dedupe by id, else
/// `actions::dispatch_in` plus the `_lula_commands`/`_lula_outbox` rows.
/// No `BEGIN`/`COMMIT` here -- the caller owns the transaction (a whole
/// `BEGIN IMMEDIATE ... COMMIT` for a lone command, or a `SAVEPOINT` inside
/// a group commit).
async fn apply_cmd_body(
    conn: &mut SqliteConnection,
    id: Uuid,
    action: &str,
    args: &Value,
) -> AppResult<Reply> {
    let id_str = id.to_string();
    let args_hash = hash_args(action, args);
    if let Some(existing) = fetch_command(conn, &id_str).await? {
        if existing.action != action || existing.args_hash != args_hash {
            return Err(AppError::Conflict(format!(
                "command {id} was already applied with different action/args"
            )));
        }
        let result: Value = serde_json::from_str(&existing.reply_json)
            .map_err(|e| AppError::Internal(format!("corrupt stored reply for {id}: {e}")))?;
        let undo = existing
            .undo_json
            .as_deref()
            .map(serde_json::from_str::<Undo>)
            .transpose()
            .map_err(|e| AppError::Internal(format!("corrupt stored undo for {id}: {e}")))?;
        return Ok(Reply {
            result,
            undo,
            deduped: true,
            outbox_seq: -1,
        });
    }
    let outcome = actions::dispatch_in(conn, action, args).await?;
    maybe_crash_delay().await;
    let reply_json = serde_json::to_string(&outcome.result)
        .map_err(|e| AppError::Internal(format!("serializing reply: {e}")))?;
    let undo_json = outcome
        .undo
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| AppError::Internal(format!("serializing undo: {e}")))?;
    sqlx::query(
        "INSERT INTO _lula_commands (id, action, args_hash, reply_json, undo_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(&id_str)
    .bind(action)
    .bind(&args_hash)
    .bind(&reply_json)
    .bind(&undo_json)
    .execute(&mut *conn)
    .await?;
    let seq: i64 = sqlx::query_scalar(
        "INSERT INTO _lula_outbox (kind, payload) VALUES (?1, ?2) RETURNING seq",
    )
    .bind(action)
    .bind(&reply_json)
    .fetch_one(conn)
    .await?;
    Ok(Reply {
        result: outcome.result,
        undo: outcome.undo,
        deduped: false,
        outbox_seq: seq,
    })
}

/// Apply a whole batch on an already-open transaction: dedupe by batch id,
/// else every command in order (any error aborts the batch, see
/// `handle_batch`), then one `_lula_commands` row and one `_lula_outbox`
/// row for the batch as a whole.
async fn apply_batch_body(
    conn: &mut SqliteConnection,
    id: Uuid,
    commands: &[(String, Value)],
) -> AppResult<Reply> {
    let id_str = id.to_string();
    let canon = serde_json::to_value(commands)
        .map_err(|e| AppError::Internal(format!("serializing batch args: {e}")))?;
    let args_hash = hash_args("batch", &canon);
    if let Some(existing) = fetch_command(conn, &id_str).await? {
        if existing.action != "batch" || existing.args_hash != args_hash {
            return Err(AppError::Conflict(format!(
                "batch {id} was already applied with different commands"
            )));
        }
        let result: Value = serde_json::from_str(&existing.reply_json)
            .map_err(|e| AppError::Internal(format!("corrupt stored reply for {id}: {e}")))?;
        return Ok(Reply {
            result,
            undo: None,
            deduped: true,
            outbox_seq: -1,
        });
    }
    let mut results = Vec::with_capacity(commands.len());
    for (action, args) in commands {
        let outcome = actions::dispatch_in(conn, action, args).await?;
        results.push(outcome.result);
    }
    maybe_crash_delay().await;
    let reply_json = serde_json::to_string(&results)
        .map_err(|e| AppError::Internal(format!("serializing batch reply: {e}")))?;
    sqlx::query(
        "INSERT INTO _lula_commands (id, action, args_hash, reply_json, undo_json)
         VALUES (?1, 'batch', ?2, ?3, NULL)",
    )
    .bind(&id_str)
    .bind(&args_hash)
    .bind(&reply_json)
    .execute(&mut *conn)
    .await?;
    let seq: i64 = sqlx::query_scalar(
        "INSERT INTO _lula_outbox (kind, payload) VALUES ('batch', ?1) RETURNING seq",
    )
    .bind(&reply_json)
    .fetch_one(conn)
    .await?;
    Ok(Reply {
        result: Value::Array(results),
        undo: None,
        deduped: false,
        outbox_seq: seq,
    })
}

/// One command, its own `BEGIN IMMEDIATE ... COMMIT`.
async fn handle_cmd(
    conn: &mut SqliteConnection,
    outbox_tx: &broadcast::Sender<i64>,
    id: Uuid,
    action: &str,
    args: &Value,
) -> AppResult<Reply> {
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
    match apply_cmd_body(conn, id, action, args).await {
        Ok(reply) => {
            sqlx::query("COMMIT").execute(&mut *conn).await?;
            if reply.outbox_seq >= 0 {
                let _ = outbox_tx.send(reply.outbox_seq);
            }
            Ok(reply)
        }
        Err(e) => {
            let _ = sqlx::query("ROLLBACK").execute(conn).await;
            Err(e)
        }
    }
}

/// One batch, its own `BEGIN IMMEDIATE ... COMMIT`.
async fn handle_batch(
    conn: &mut SqliteConnection,
    outbox_tx: &broadcast::Sender<i64>,
    id: Uuid,
    commands: &[(String, Value)],
) -> AppResult<Reply> {
    sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
    match apply_batch_body(conn, id, commands).await {
        Ok(reply) => {
            sqlx::query("COMMIT").execute(&mut *conn).await?;
            if reply.outbox_seq >= 0 {
                let _ = outbox_tx.send(reply.outbox_seq);
            }
            Ok(reply)
        }
        Err(e) => {
            let _ = sqlx::query("ROLLBACK").execute(conn).await;
            Err(e)
        }
    }
}

/// A group of interactive commands applied in one outer transaction, each
/// in its own `SAVEPOINT` so one failing command only loses its own change;
/// every reply is held back until the outer `COMMIT` actually lands.
async fn handle_group(
    conn: &mut SqliteConnection,
    outbox_tx: &broadcast::Sender<i64>,
    group: Vec<CmdMsg>,
) {
    if let Err(e) = sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await {
        let msg = e.to_string();
        for cmd in group {
            let _ = cmd
                .reply
                .send(Err(AppError::Database(sqlx::Error::Protocol(msg.clone()))));
        }
        return;
    }
    let mut seqs = Vec::new();
    let mut outcomes = Vec::with_capacity(group.len());
    for cmd in group {
        if let Err(e) = sqlx::query("SAVEPOINT cmd").execute(&mut *conn).await {
            outcomes.push((cmd.reply, Err(AppError::from(e))));
            continue;
        }
        match apply_cmd_body(conn, cmd.id, &cmd.action, &cmd.args).await {
            Ok(reply) => {
                let _ = sqlx::query("RELEASE SAVEPOINT cmd")
                    .execute(&mut *conn)
                    .await;
                if reply.outbox_seq >= 0 {
                    seqs.push(reply.outbox_seq);
                }
                outcomes.push((cmd.reply, Ok(reply)));
            }
            Err(e) => {
                let _ = sqlx::query("ROLLBACK TO SAVEPOINT cmd")
                    .execute(&mut *conn)
                    .await;
                let _ = sqlx::query("RELEASE SAVEPOINT cmd")
                    .execute(&mut *conn)
                    .await;
                outcomes.push((cmd.reply, Err(e)));
            }
        }
    }
    match sqlx::query("COMMIT").execute(&mut *conn).await {
        Ok(_) => {
            for seq in seqs {
                let _ = outbox_tx.send(seq);
            }
            for (reply, result) in outcomes {
                let _ = reply.send(result);
            }
        }
        Err(e) => {
            let msg = format!("group commit failed: {e}");
            for (reply, _) in outcomes {
                let _ = reply.send(Err(AppError::Internal(msg.clone())));
            }
        }
    }
}

enum Next {
    Interactive(CmdMsg),
    Batch(BatchMsg),
    /// Timer fired, or every handle has been dropped -- either way, wind
    /// down the same way (checkpoint and exit).
    Idle,
}

async fn next_msg(
    interactive_rx: &mut mpsc::Receiver<CmdMsg>,
    batch_rx: &mut mpsc::Receiver<BatchMsg>,
    idle_timeout: Duration,
) -> Next {
    tokio::select! {
        biased;
        cmd = interactive_rx.recv() => cmd.map_or(Next::Idle, Next::Interactive),
        batch = batch_rx.recv() => batch.map_or(Next::Idle, Next::Batch),
        () = tokio::time::sleep(idle_timeout) => Next::Idle,
    }
}

async fn run_loop(
    mut conn: SqliteConnection,
    mut interactive_rx: mpsc::Receiver<CmdMsg>,
    mut batch_rx: mpsc::Receiver<BatchMsg>,
    outbox_tx: broadcast::Sender<i64>,
    config: ActorConfig,
) {
    loop {
        match next_msg(&mut interactive_rx, &mut batch_rx, config.idle_timeout).await {
            Next::Idle => break,
            Next::Batch(msg) => {
                let result = handle_batch(&mut conn, &outbox_tx, msg.id, &msg.commands).await;
                let _ = msg.reply.send(result);
            }
            Next::Interactive(first) => {
                if config.group_commit {
                    let mut group = vec![first];
                    while group.len() < config.group_commit_max {
                        match interactive_rx.try_recv() {
                            Ok(cmd) => group.push(cmd),
                            Err(_) => break,
                        }
                    }
                    handle_group(&mut conn, &outbox_tx, group).await;
                } else {
                    let result =
                        handle_cmd(&mut conn, &outbox_tx, first.id, &first.action, &first.args)
                            .await;
                    let _ = first.reply.send(result);
                }
            }
        }
    }
    let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&mut conn)
        .await;
    let _ = conn.close().await;
}

/// Spawns workspace actors and hands out handles to them.
pub struct WorkspaceActor;

impl WorkspaceActor {
    /// Open `path` (created and migrated if missing), spawn its actor task
    /// and return a handle to it.
    pub async fn spawn(
        path: impl AsRef<Path>,
        config: ActorConfig,
    ) -> anyhow::Result<WorkspaceHandle> {
        let url = format!("sqlite://{}", path.as_ref().display());
        let options = crate::db::options(&url)?;
        let mut conn = SqliteConnection::connect_with(&options).await?;
        Box::pin(sqlx::migrate!("./migrations").run(&mut conn)).await?;

        let (interactive_tx, interactive_rx) = mpsc::channel(config.mailbox_capacity);
        let (batch_tx, batch_rx) = mpsc::channel(config.mailbox_capacity);
        let (outbox_tx, _subscriber) = broadcast::channel(256);
        let handle = WorkspaceHandle {
            interactive_tx,
            batch_tx,
            outbox: outbox_tx.clone(),
        };
        tokio::spawn(run_loop(conn, interactive_rx, batch_rx, outbox_tx, config));
        Ok(handle)
    }
}

/// Per-workspace actor lifecycle: a lazily-spawned handle per workspace id,
/// re-spawned on the next call after an idle eviction. One
/// `Arc<Mutex<Option<WorkspaceHandle>>>` slot per workspace serializes
/// concurrent wakes -- many callers racing `get_or_wake` for the same
/// sleeping (or never-started) workspace produce exactly one spawn.
/// One lazily-created slot per workspace id; its own `Mutex` serializes
/// concurrent wakes for that workspace without blocking other workspaces.
type Slot = Arc<Mutex<Option<WorkspaceHandle>>>;

#[derive(Clone)]
pub struct ActorRegistry {
    data_dir: PathBuf,
    config: ActorConfig,
    slots: Arc<Mutex<HashMap<Uuid, Slot>>>,
}

impl ActorRegistry {
    pub fn new(data_dir: impl Into<PathBuf>, config: ActorConfig) -> Self {
        Self {
            data_dir: data_dir.into(),
            config,
            slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The on-disk SQLite path for `id`, following the same
    /// `<data_dir>/workspaces/<id>.db` convention as
    /// `crate::workspace_db::WorkspaceRegistry`.
    pub fn path_for(&self, id: Uuid) -> PathBuf {
        self.data_dir.join("workspaces").join(format!("{id}.db"))
    }

    /// The live handle for `id`, waking (or starting) its actor if needed.
    pub async fn get_or_wake(&self, id: Uuid) -> anyhow::Result<WorkspaceHandle> {
        let slot = {
            let mut slots = self.slots.lock().await;
            slots
                .entry(id)
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone()
        };
        let mut guard = slot.lock().await;
        if let Some(handle) = guard.as_ref()
            && !handle.is_closed()
        {
            return Ok(handle.clone());
        }
        let path = self.path_for(id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let handle = WorkspaceActor::spawn(path, self.config.clone()).await?;
        *guard = Some(handle.clone());
        Ok(handle)
    }
}
