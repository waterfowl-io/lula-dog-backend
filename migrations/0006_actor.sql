-- The workspace actor's durable state (actor-design.md): idempotent command
-- replay and the change-feed outbox. Both live in the workspace's own
-- SQLite file, written in the same transaction as the command they record,
-- so a crash leaves either everything (command + its effects + its outbox
-- row) or nothing.

-- One row per applied command (or batch) id. `args_hash` lets a replay with
-- the same id but different args be rejected as a conflict instead of
-- silently returning a stale reply. `reply_json` is what dedupe hits
-- return verbatim; `undo_json` is an actions::Undo, when the command
-- produced one.
CREATE TABLE _lula_commands (
  id TEXT PRIMARY KEY,
  action TEXT NOT NULL,
  args_hash TEXT NOT NULL,
  reply_json TEXT NOT NULL,
  undo_json TEXT,
  applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- The change feed: one row per applied command/batch, for SSE live updates
-- (not built in this spike) and for the benchmark harness to confirm
-- `seq` only ever increases.
CREATE TABLE _lula_outbox (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  kind TEXT NOT NULL,
  payload TEXT NOT NULL
);
