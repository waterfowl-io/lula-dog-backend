-- Lula.dog sales opportunity tracker: initial schema.
--
-- Timestamps are TEXT in SQLite's CURRENT_TIMESTAMP format ('YYYY-MM-DD HH:MM:SS',
-- UTC). The application normalizes every timestamp it writes to that format, so
-- plain string comparison orders them correctly.

CREATE TABLE opportunities (
  id INTEGER PRIMARY KEY,
  company TEXT NOT NULL,
  contact_name TEXT, contact_email TEXT,
  value_cents INTEGER,
  stage TEXT NOT NULL DEFAULT 'lead'
    CHECK (stage IN ('lead', 'qualified', 'proposal', 'won', 'lost')),
  next_step TEXT,
  last_touch_at TEXT,
  created_at TEXT DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE timeline_entries (          -- append-only (see triggers below)
  id INTEGER PRIMARY KEY,
  opportunity_id INTEGER REFERENCES opportunities(id),
  raw_text TEXT NOT NULL, summary TEXT, occurred_at TEXT,
  created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE field_changes (             -- AI proposals + audit trail
  id INTEGER PRIMARY KEY,
  opportunity_id INTEGER REFERENCES opportunities(id),
  entry_id INTEGER REFERENCES timeline_entries(id),
  field TEXT, old_value TEXT, new_value TEXT,
  status TEXT DEFAULT 'proposed',        -- proposed, accepted, rejected
  created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE tasks (
  id INTEGER PRIMARY KEY,
  opportunity_id INTEGER REFERENCES opportunities(id),
  title TEXT NOT NULL, due_at TEXT, done INTEGER DEFAULT 0,
  source_entry_id INTEGER REFERENCES timeline_entries(id)
);

CREATE INDEX opportunities_stage_idx ON opportunities (stage);
CREATE INDEX timeline_entries_opportunity_idx ON timeline_entries (opportunity_id);
CREATE INDEX field_changes_opportunity_status_idx ON field_changes (opportunity_id, status);
CREATE INDEX tasks_opportunity_idx ON tasks (opportunity_id, done);

-- Append-only timeline. The pasted text is the record and is never edited or
-- deleted. The AI enrichment columns (summary, occurred_at) are written once:
-- the paste flow saves the raw entry first, so nothing is lost if extraction
-- fails, and fills them in afterwards.
CREATE TRIGGER timeline_entries_no_delete
BEFORE DELETE ON timeline_entries
BEGIN
  SELECT RAISE(ABORT, 'timeline_entries is append-only');
END;

CREATE TRIGGER timeline_entries_write_once
BEFORE UPDATE ON timeline_entries
WHEN NEW.raw_text IS NOT OLD.raw_text
  OR NEW.opportunity_id IS NOT OLD.opportunity_id
  OR NEW.created_at IS NOT OLD.created_at
  OR (OLD.summary IS NOT NULL AND NEW.summary IS NOT OLD.summary)
  OR (OLD.occurred_at IS NOT NULL AND NEW.occurred_at IS NOT OLD.occurred_at)
BEGIN
  SELECT RAISE(ABORT, 'timeline_entries is append-only');
END;
