-- Live agent chat for an app, in two modes (data, build). Conversations and
-- messages are customer data, so -- like opportunities, leads, tasks -- they
-- live in the workspace's own SQLite file, never in the Postgres control
-- plane. `app_id` is a `control.app_definitions.id` (a Postgres uuid),
-- stored as text with no SQLite-level FK across databases; the chat HTTP
-- handlers resolve and own that relationship.
--
-- Timestamps follow 0001_initial.sql's convention: TEXT in
-- 'YYYY-MM-DD HH:MM:SS' UTC, written by the application or defaulted by
-- CURRENT_TIMESTAMP, so plain string comparison orders them correctly.
-- Ids are TEXT uuids, assigned by the application (`uuid::Uuid::new_v4()`),
-- not SQLite `INTEGER PRIMARY KEY` rowids, so a conversation/message/action
-- id is stable to reference before the row round-trips through a RETURNING
-- clause (useful for streaming: the id is minted before the first SSE byte).

CREATE TABLE chat_conversations (
  id TEXT PRIMARY KEY,
  app_id TEXT NOT NULL,
  mode TEXT NOT NULL CHECK (mode IN ('data', 'build')),
  user_id TEXT NOT NULL,         -- the auth user who started it
  title TEXT,                    -- first user message, truncated; for a picker
  created_at TEXT DEFAULT CURRENT_TIMESTAMP,
  last_message_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX chat_conversations_app_mode_idx
  ON chat_conversations (app_id, mode, last_message_at DESC);

CREATE TABLE chat_messages (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES chat_conversations (id),
  role TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'tool')),
  content TEXT,                  -- assistant/user text; null for a pure tool-call message
  tool_calls TEXT,               -- JSON [{id, name, args}], assistant messages that call tools
  tool_call_id TEXT,             -- role = 'tool': which call this is the result of
  tool_name TEXT,                -- role = 'tool': the tool name, for display without a join
  tool_result TEXT,              -- role = 'tool': JSON result returned to the model
  provider TEXT,                 -- assistant messages: 'grok' | 'fake'
  model TEXT,                    -- assistant messages: the resolved model version string
  prompt_tokens INTEGER,
  completion_tokens INTEGER,
  cost_usd REAL,                 -- null if price is unconfigured for this model
  latency_ms INTEGER,
  created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX chat_messages_conversation_idx ON chat_messages (conversation_id, id);

-- One row per applied write in data mode: the audit trail the spec asks
-- for, plus everything an undo needs. `undo` is null when the action isn't
-- reversible; `undoable` is redundant with that but kept as its own column
-- so "can this be undone" is a single indexed boolean check, not a NULL test
-- buried in a JSON blob.
CREATE TABLE chat_actions (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES chat_conversations (id),
  message_id TEXT NOT NULL REFERENCES chat_messages (id),
  user_id TEXT NOT NULL,
  action TEXT NOT NULL,           -- e.g. 'opportunity.update_stage'
  args TEXT NOT NULL,             -- JSON, exactly what the model passed
  result TEXT NOT NULL,           -- JSON, what the action returned (for the action card)
  undoable INTEGER NOT NULL DEFAULT 0,
  undo TEXT,                      -- JSON: {action, args} to reverse it, or null
  undone_at TEXT,
  created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX chat_actions_conversation_idx ON chat_actions (conversation_id, id);
