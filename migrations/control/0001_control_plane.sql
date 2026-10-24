-- Lula.dog control plane: profiles, workspaces, membership, app definitions,
-- saved queries, schema catalog, runner configs. Applied to
-- CONTROL_DATABASE_URL (Postgres/Supabase) on startup, separately from the
-- per-workspace SQLite migrations in ../ (the data plane).
--
-- `auth.users` and `auth.uid()` are a Supabase convention: real Supabase
-- projects already have them (managed by GoTrue), so this only creates a
-- minimal stand-in when they are missing, e.g. a bare local/CI Postgres. The
-- checks are deliberately "if it doesn't already exist" rather than
-- CREATE OR REPLACE, so a real Supabase project's `auth` schema is never
-- touched.

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = 'auth') THEN
    CREATE SCHEMA auth;
  END IF;
END
$$;

CREATE TABLE IF NOT EXISTS auth.users (
  id uuid PRIMARY KEY,
  email text
);

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_proc p
    JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'auth' AND p.proname = 'uid'
  ) THEN
    CREATE FUNCTION auth.uid() RETURNS uuid AS $fn$
      SELECT COALESCE(
        NULLIF(current_setting('request.jwt.claim.sub', true), ''),
        (NULLIF(current_setting('request.jwt.claims', true), '')::json ->> 'sub')
      )::uuid
    $fn$ LANGUAGE sql STABLE;
  END IF;
END
$$;

-- One row per Supabase Auth user. The backend upserts this on first sighting
-- of a verified JWT; see the optional trigger below for the Supabase-native
-- alternative (create on auth.users insert).
CREATE TABLE profiles (
  id uuid PRIMARY KEY REFERENCES auth.users(id) ON DELETE CASCADE,
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

-- RLS: for tables the frontend might read directly with the Supabase anon/
-- authenticated role. The backend itself connects as a privileged role
-- (table owner), which Postgres always exempts from RLS, so these policies
-- are for direct-from-frontend reads only and change nothing for the
-- backend's own queries.
ALTER TABLE profiles ENABLE ROW LEVEL SECURITY;
CREATE POLICY profiles_select_self ON profiles
  FOR SELECT USING (id = auth.uid());

ALTER TABLE workspaces ENABLE ROW LEVEL SECURITY;
CREATE POLICY workspaces_select_member ON workspaces
  FOR SELECT USING (
    EXISTS (
      SELECT 1 FROM workspace_members m
      WHERE m.workspace_id = workspaces.id AND m.user_id = auth.uid()
    )
  );

ALTER TABLE workspace_members ENABLE ROW LEVEL SECURITY;
CREATE POLICY workspace_members_select_member ON workspace_members
  FOR SELECT USING (
    EXISTS (
      SELECT 1 FROM workspace_members m
      WHERE m.workspace_id = workspace_members.workspace_id AND m.user_id = auth.uid()
    )
  );

ALTER TABLE app_definitions ENABLE ROW LEVEL SECURITY;
CREATE POLICY app_definitions_select_member ON app_definitions
  FOR SELECT USING (
    EXISTS (
      SELECT 1 FROM workspace_members m
      WHERE m.workspace_id = app_definitions.workspace_id AND m.user_id = auth.uid()
    )
  );

ALTER TABLE app_definition_versions ENABLE ROW LEVEL SECURITY;
CREATE POLICY app_definition_versions_select_member ON app_definition_versions
  FOR SELECT USING (
    EXISTS (
      SELECT 1 FROM app_definitions d
      JOIN workspace_members m ON m.workspace_id = d.workspace_id
      WHERE d.id = app_definition_versions.app_definition_id AND m.user_id = auth.uid()
    )
  );

ALTER TABLE saved_queries ENABLE ROW LEVEL SECURITY;
CREATE POLICY saved_queries_select_member ON saved_queries
  FOR SELECT USING (
    EXISTS (
      SELECT 1 FROM workspace_members m
      WHERE m.workspace_id = saved_queries.workspace_id AND m.user_id = auth.uid()
    )
  );

-- Optional: create a profile automatically when Supabase Auth creates a
-- user, mirroring the common Supabase pattern. The backend's own middleware
-- also upserts a profile on first verified request from a user (see
-- `crate::control::profiles::upsert_from_jwt`), so this trigger is a
-- convenience, not a requirement.
CREATE OR REPLACE FUNCTION public.handle_new_auth_user() RETURNS trigger AS $$
BEGIN
  INSERT INTO public.profiles (id, email)
  VALUES (NEW.id, NEW.email)
  ON CONFLICT (id) DO NOTHING;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql SECURITY DEFINER;

DROP TRIGGER IF EXISTS on_auth_user_created ON auth.users;
CREATE TRIGGER on_auth_user_created
  AFTER INSERT ON auth.users
  FOR EACH ROW EXECUTE FUNCTION public.handle_new_auth_user();
