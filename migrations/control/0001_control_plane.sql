-- Lula.dog control plane: profiles, workspaces, membership, app definitions,
-- saved queries, schema catalog, runner configs. Applied to
-- DATABASE_URL/CONTROL_DATABASE_URL (Postgres/Supabase) on startup,
-- separately from the per-workspace SQLite migrations in ../ (the data
-- plane).
--
-- This never references Supabase's `auth` schema (no `auth.users` stand-in,
-- no `auth.uid()`, no `auth.users` FK or trigger): the backend connects to
-- Postgres directly, as a privileged role, and verifies identity itself
-- (`crate::auth`) rather than through RLS keyed on `auth.uid()`. `profiles.id`
-- is a plain `uuid` -- the verified JWT's `sub` -- with no foreign key to any
-- Supabase-managed table, so this migration applies cleanly to a bare
-- Postgres with no `auth` schema at all.

-- One row per authenticated user, keyed by their verified JWT `sub`. The
-- backend upserts this on first sighting of a verified token (see
-- `crate::auth::provision` / `crate::control::profiles::upsert_from_jwt`).
CREATE TABLE profiles (
  id uuid PRIMARY KEY,
  email text NOT NULL,
  display_name text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE workspaces (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  name text NOT NULL,
  created_by uuid REFERENCES profiles(id) ON DELETE SET NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE workspace_members (
  workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  user_id uuid NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
  role text NOT NULL CHECK (role IN ('owner', 'admin', 'member', 'viewer')),
  created_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (workspace_id, user_id)
);

CREATE INDEX workspace_members_user_idx ON workspace_members (user_id);

-- Immutable versions of an AI-generated app definition (pages/layout/
-- components as JSON; see `crate::app_definition`). `published_version_id`
-- is the pointer the frontend reads by default; rollback inserts a new
-- version (a copy of an older one's `definition`) and republishes that new
-- version, so the published pointer only ever moves forward and version
-- history stays a strict, immutable log -- see docs/architecture.md.
CREATE TABLE app_definitions (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  key text NOT NULL,
  name text NOT NULL,
  published_version_id uuid,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (workspace_id, key)
);

CREATE TABLE app_definition_versions (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  app_definition_id uuid NOT NULL REFERENCES app_definitions(id) ON DELETE CASCADE,
  version integer NOT NULL,
  definition jsonb NOT NULL,
  created_by uuid REFERENCES profiles(id) ON DELETE SET NULL,
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (app_definition_id, version)
);

ALTER TABLE app_definitions
  ADD CONSTRAINT app_definitions_published_version_fk
  FOREIGN KEY (published_version_id) REFERENCES app_definition_versions(id)
  ON DELETE SET NULL;

CREATE INDEX app_definition_versions_app_idx
  ON app_definition_versions (app_definition_id, version DESC);

-- Reusable SQL a component binds to by (workspace_id, key); see
-- `crate::app_definition`. `params` is a JSON array of `{name, type}`.
CREATE TABLE saved_queries (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  key text NOT NULL,
  name text NOT NULL,
  description text,
  sql_text text NOT NULL,
  params jsonb NOT NULL DEFAULT '[]'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (workspace_id, key)
);

-- Introspected from the workspace SQLite file by GET /w/{ws}/schema, so the
-- AI building apps knows what tables/columns exist without a live connection.
CREATE TABLE schema_catalog (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  table_name text NOT NULL,
  column_name text NOT NULL,
  data_type text NOT NULL,
  is_nullable boolean NOT NULL,
  is_primary_key boolean NOT NULL DEFAULT false,
  foreign_key_table text,
  foreign_key_column text,
  refreshed_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (workspace_id, table_name, column_name)
);

-- Ingestion runner configuration. Not executed by this step; config only.
CREATE TABLE runner_configs (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  kind text NOT NULL,
  config jsonb NOT NULL DEFAULT '{}'::jsonb,
  schedule_cron text,
  enabled boolean NOT NULL DEFAULT true,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

-- No RLS and no auth.uid()-keyed policies: the backend is the only writer
-- of this schema, connects as a privileged role, and verifies identity and
-- workspace membership itself (`crate::auth`, `crate::workspace_db::scope`).
-- There is no `auth.uid()` session variable to key a policy on, since this
-- service does not run as Supabase's `authenticated`/`anon` Postgres roles.
