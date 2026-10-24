-- Lead generation: target profiles, discovered companies with provenance,
-- enrichment/classification results, the lead queue, and a coverage log that
-- lets import/search runs advance through uncovered space instead of
-- repeating. See docs/lead-gen.md.
--
-- Timestamps follow 0001_initial.sql's convention: TEXT in
-- 'YYYY-MM-DD HH:MM:SS' UTC, written by the application or defaulted by
-- CURRENT_TIMESTAMP, so plain string comparison orders them correctly.

-- Jack's saved search. Seeded by `cargo run -- seed` or a future CLI, not by
-- this migration, so an empty database has zero profiles until asked for one.
CREATE TABLE target_profiles (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  industries TEXT NOT NULL DEFAULT '[]',       -- JSON array of strings
  regions TEXT NOT NULL DEFAULT '[]',          -- JSON array of strings, e.g. ["IL"]
  size_min INTEGER,                            -- employees, if known
  size_max INTEGER,
  size_notes TEXT,                             -- freeform, when headcount isn't the signal
  include_keywords TEXT NOT NULL DEFAULT '[]', -- JSON array of strings
  exclude_keywords TEXT NOT NULL DEFAULT '[]',
  min_fit_score INTEGER NOT NULL DEFAULT 50,
  created_at TEXT DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

-- A business entity discovered from any source. Unique on normalized domain
-- when one is known, else on (jurisdiction, state_file_number); see the
-- partial unique indexes below. `companies_merge` in src/companies/mod.rs is
-- the only code that re-keys a row from the file-number identity onto a
-- newly-discovered domain, so a company is never duplicated either way.
CREATE TABLE companies (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  entity_type TEXT,
  jurisdiction TEXT NOT NULL DEFAULT 'IL',
  state_file_number TEXT,
  formation_date TEXT,
  status TEXT,
  principal_address TEXT,
  mailing_address TEXT,
  officers TEXT,                               -- JSON array of {name, title, address}
  registered_agent TEXT,
  website TEXT,
  domain TEXT,                                  -- normalized: lowercase, no scheme/www/path/port
  phone TEXT,
  category TEXT,
  filter_status TEXT NOT NULL DEFAULT 'pending'
    CHECK (filter_status IN ('pending', 'passed', 'filtered')),
  filter_reason TEXT,
  pre_score INTEGER,
  created_at TEXT DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX companies_domain_idx ON companies (domain) WHERE domain IS NOT NULL;
CREATE UNIQUE INDEX companies_file_number_idx ON companies (jurisdiction, state_file_number)
  WHERE state_file_number IS NOT NULL;
CREATE INDEX companies_filter_status_idx ON companies (filter_status);
CREATE INDEX companies_normalized_name_idx ON companies (normalized_name);

-- Provenance: every raw record that contributed to a company, idempotent on
-- (source, source_record_key) so re-importing a file or an overlapping CSV
-- never creates duplicate rows.
CREATE TABLE company_sources (
  id INTEGER PRIMARY KEY,
  company_id INTEGER NOT NULL REFERENCES companies(id),
  source TEXT NOT NULL,            -- e.g. 'bizapedia', 'il_sos_llc'
  source_record_key TEXT NOT NULL, -- e.g. a state file number or a row hash
  raw_file_path TEXT NOT NULL,     -- path in the bucket this record came from
  raw_row TEXT NOT NULL,           -- JSON of the original row
  imported_at TEXT DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (source, source_record_key)
);

CREATE INDEX company_sources_company_idx ON company_sources (company_id);

-- What has already been searched or imported: a source/query/region/category
-- and a file or period covered, with counts. Importers and the Places search
-- write one row per run; enrichment selection and importers consult it so a
-- run advances through uncovered space instead of repeating work.
CREATE TABLE coverage_log (
  id INTEGER PRIMARY KEY,
  source TEXT NOT NULL,
  query TEXT,
  region TEXT,
  category TEXT,
  period TEXT,         -- e.g. the imported file name, or a date range
  run_at TEXT DEFAULT CURRENT_TIMESTAMP,
  seen_count INTEGER NOT NULL DEFAULT 0,
  new_count INTEGER NOT NULL DEFAULT 0,
  duplicate_count INTEGER NOT NULL DEFAULT 0,
  filtered_count INTEGER NOT NULL DEFAULT 0,
  error_count INTEGER NOT NULL DEFAULT 0,
  error TEXT
);

CREATE INDEX coverage_log_source_idx ON coverage_log (source, period);

-- One row per enrichment batch (places lookup, crawl or classify), for
-- counts and CLI output; see also `company_enrichments` for per-company
-- results.
CREATE TABLE enrichment_runs (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('places', 'crawl', 'classify')),
  started_at TEXT DEFAULT CURRENT_TIMESTAMP,
  finished_at TEXT,
  attempted_count INTEGER NOT NULL DEFAULT 0,
  success_count INTEGER NOT NULL DEFAULT 0,
  error_count INTEGER NOT NULL DEFAULT 0,
  error TEXT
);

-- Per-company enrichment result, one row per attempt. Selection for `enrich`
-- and `classify` checks for an existing row of the given kind for a company
-- and skips it, so re-running a command only processes what is new.
-- Classification results (industry, products, fit_score, ...) are stored in
-- `result` for kind = 'classify'.
CREATE TABLE company_enrichments (
  id INTEGER PRIMARY KEY,
  company_id INTEGER NOT NULL REFERENCES companies(id),
  kind TEXT NOT NULL CHECK (kind IN ('places', 'crawl', 'classify')),
  run_id INTEGER REFERENCES enrichment_runs(id),
  result TEXT,          -- JSON
  files TEXT NOT NULL DEFAULT '[]', -- JSON array of bucket paths written
  status TEXT NOT NULL CHECK (status IN ('ok', 'error')),
  error TEXT,
  created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX company_enrichments_company_kind_idx ON company_enrichments (company_id, kind);

-- A classified company worth Jack's attention. `company_id` is unique while
-- a candidate is open (new or snoozed): a company can be re-queued after a
-- rejection (e.g. once the profile changes) but never has two open
-- candidates at once.
CREATE TABLE lead_candidates (
  id INTEGER PRIMARY KEY,
  company_id INTEGER NOT NULL REFERENCES companies(id),
  profile_id INTEGER NOT NULL REFERENCES target_profiles(id),
  fit_score INTEGER NOT NULL,
  fit_reason TEXT,
  status TEXT NOT NULL DEFAULT 'new'
    CHECK (status IN ('new', 'accepted', 'rejected', 'snoozed')),
  reject_reason TEXT,
  snoozed_until TEXT,
  opportunity_id INTEGER REFERENCES opportunities(id),
  created_at TEXT DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX lead_candidates_company_open_idx ON lead_candidates (company_id)
  WHERE status IN ('new', 'snoozed');
CREATE INDEX lead_candidates_status_score_idx ON lead_candidates (status, fit_score DESC);
