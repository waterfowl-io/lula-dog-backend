# Architecture

Lula.dog is an agentic data platform: runners ingest data, SQLite is the
editable system of record, DuckDB queries across sources, and AI builds apps
(dashboards, CRM, ops tools) that are data, not code. This document covers
step 3: the control plane, auth, workspace routing, app definitions and the
query runner. For the sales tracker and lead-gen data model themselves, see
the main [`README.md`](../README.md) and [`docs/lead-gen.md`](lead-gen.md).

## Control plane vs. data plane

**Control plane (Supabase Postgres, `CONTROL_DATABASE_URL`):** everything
about *who* and *what*, never customer data:

- `profiles` — one row per Supabase Auth user, keyed by their auth user id.
- `workspaces`, `workspace_members` — a workspace and its members, each with
  a role (`owner`, `admin`, `member`, `viewer`).
- `app_definitions`, `app_definition_versions` — AI-generated apps as
  versioned JSON (see [App definitions are data](#app-definitions-are-data)).
- `saved_queries` — named SQL a component binds to.
- `schema_catalog` — a cached, introspected table/column catalog per
  workspace, refreshed by `GET /w/{ws}/schema`.
- `runner_configs` — ingestion runner configuration (kind, config, cron
  schedule, enabled). Created by this step's migration; no runner executes
  them yet.

Migrations live in `migrations/control/`, applied to `CONTROL_DATABASE_URL`
on startup, separate from the data-plane SQLite migrations in `migrations/`.
Real Supabase projects already have an `auth` schema (managed by GoTrue); the
control-plane migration only creates a minimal `auth.users`/`auth.uid()`
stand-in *if they don't already exist* (a bare local/CI Postgres), so it
never touches a real project's `auth` schema. RLS policies are included for
the tables the frontend might read directly with the Supabase
anon/authenticated role (`profiles`: self; `workspaces`/`workspace_members`/
`app_definitions`/`app_definition_versions`/`saved_queries`: members can
select) — the backend itself connects as the table owner, which Postgres
always exempts from RLS, so these policies change nothing for the backend's
own queries.

**Data plane (SQLite, one file per workspace):** customer data never lives
in Postgres. Editable app data (opportunities, tasks, timeline, proposals,
leads, companies, target profiles, ...) is one SQLite file per workspace,
created and migrated on first use under `DATA_DIR/workspaces/{id}.db` (see
`src/workspace_db.rs`). Runner-ingested data will live in DuckLake instead
(see [below](#ducklake-later)); DuckDB queries across both.

One exception, for backward compatibility: the workspace id
`00000000-0000-0000-0000-000000000000` (`config::DEFAULT_WORKSPACE_ID`) reads
`DATABASE_URL` directly instead of a `DATA_DIR` path, so a pre-step-3
single-tenant setup (`cargo run` with just `DATABASE_URL` set) keeps reading
the same file.

## Request flow: JWT → user → membership → workspace pool

1. **Auth** (`src/auth`). `AuthUser` is an Axum extractor. Under
   `AUTH_MODE=supabase` (the default), it reads `Authorization: Bearer
   <jwt>`, decodes the header to find `alg`/`kid`, and verifies: ES256/RS256
   against the project's JWKS (`${SUPABASE_URL}/auth/v1/.well-known/jwks.json`,
   cached with a TTL, refetched immediately if the token names an unknown
   `kid`), or HS256 against `SUPABASE_JWT_SECRET` (the legacy scheme). `exp`,
   `aud` and `iss` are all validated. The result is `{id: Uuid, email:
   String}` from the token's `sub`/`email` claims.

   Under `AUTH_MODE=dev`, verification is skipped entirely: every request is
   a fixed dev user (`DEV_USER_ID`, default `config::DEFAULT_DEV_USER_ID`),
   overridable per-request with `X-Dev-User: <uuid>`. This only activates on
   an explicit `AUTH_MODE=dev` (never a fallback) and logs a loud warning on
   startup.

2. **Workspace membership** (`src/workspace_db.rs::scope`, a middleware on
   every `/w/{workspace_id}/...` route). Given the authenticated user and the
   path's `workspace_id`, it looks up the user's role in
   `workspace_members`. No row → 403. A write (any method but
   GET/HEAD/OPTIONS) from a `viewer` → 403. If `CONTROL_DATABASE_URL` isn't
   configured, this check is skipped entirely and every workspace id is
   treated as accessible with role `owner` — a deliberate, documented bypass
   (logged once at startup, like `AUTH_MODE=dev`) so the data plane and most
   of the test suite work without standing up Postgres. Tests that need real
   enforcement (`tests/workspaces.rs`) go through `TestApp::with_control`,
   gated on `TEST_CONTROL_DATABASE_URL`.

3. **Workspace pool** (`src/workspace_db.rs::WorkspaceRegistry`). The
   middleware resolves (creating and migrating on first use) the
   workspace's SQLite pool and attaches `WorkspaceContext { id, role, pool }`
   to the request. Handlers take it as an extractor and use `.pool` exactly
   where they used to use a single global pool. Pools are cached for the
   process lifetime with no eviction yet — fine at the scale of a handful of
   workspaces per process; an LRU/TTL eviction is the natural next step if
   that stops being true.

`GET /me` and `POST /workspaces` sit outside `/w/{workspace_id}` (no
membership to check yet): they only need `AuthUser`, and `POST /workspaces`
both creates the control-plane row (creator becomes `owner`) and the
workspace's SQLite file.

## App definitions are data

An app (dashboard, CRM view, ops tool) is a JSON document — `src/
app_definition`'s `AppDefinitionDoc` — not code: a list of pages (each with
a route `path`, optionally with `:params`), a layout tree (`row`/`column`/
`section`/`component` primitives), and a flat list of components from a
fixed library (`table`, `chart`, `metric_card`, `form`, `kanban`,
`detail_view`, `list`). Each component binds to a saved query (`query`,
reads) and/or a named action (`action`, writes, from a fixed registry —
`opportunity.update_stage`, `paste.submit`, `proposal.accept`/`reject`,
`task.create`/`complete`, `lead.accept`/`reject`/`snooze`) that maps to an
existing endpoint, with `params` that can reference a page's route param as
`"$route.<name>"`.

Storage (`src/control::app_definitions`): `app_definition_versions` rows are
immutable once created; `app_definitions.published_version_id` is the
pointer `GET /w/{ws}/apps/{app}` reads by default (falling back to the
latest version if nothing is published). **Rollback is a new version, not a
pointer move backwards**: `POST /w/{ws}/apps/{app}/rollback {version}` copies
an old version's `definition` into a brand new version and publishes *that*,
so version history stays a strict, append-only log and "what's published"
always points at a real entry in it. The alternative (moving the pointer
back to the old version id directly) was the other option on the table; this
one was chosen so the published pointer's history is monotonic and auditable
without special-casing "pointer moved backwards."

Validation (`src/app_definition::validate`) runs before a version is ever
stored: parse against the schema; unique page/component ids; every layout
`component` leaf resolves to a real component on its page; every `action`
is in the known registry; every `$route.<name>` reference is a param the
page's own `path` declares; each component kind has the bindings it needs
(e.g. a `kanban` needs both a `query` and an `action`, a `chart` needs
`chart_type`/`encoding` in `props`); and, needing the database, every
`query` key actually exists as a saved query in that workspace.

The JSON Schema (for the frontend to generate TypeScript types) is
generated from the same Rust types with `schemars` and served at
`GET /app-definition.schema.json`; it's also committed at
[`schema/app-definition.schema.json`](../schema/app-definition.schema.json)
with a test (`app_definition::tests::committed_schema_matches_generated`)
that fails if they drift. The seeded "Sales" app
(`seed::sales_app_definition`) is committed the same way at
[`fixtures/apps/sales.json`](../fixtures/apps/sales.json).

## Saved queries and the query runner

A saved query (`src/control::saved_queries`) is a name, a single
`SELECT`/`WITH` SQL statement, and a declared param list. `POST
/w/{ws}/queries/{id}/run {params}` executes it and returns `{query_id, sql,
columns, rows, row_count, truncated, elapsed_ms}` — `sql` is always
included, so the frontend's `</>` button on any chart/table can show exactly
what ran.

**The spec target here was DuckDB**, attaching the workspace's SQLite file
read-only (`ATTACH '<path>' (TYPE SQLITE, READ_ONLY)`) and running the query
against it. In practice, `duckdb-rs`'s `bundled` (and `extensions-full`)
Cargo features compile DuckDB core plus the parquet/json/core_functions
extensions from source, but **not** `sqlite_scanner`: DuckDB treats it as a
separate extension it downloads from its extension repository the first
time `ATTACH ... TYPE SQLITE` runs, which needs network access. That
conflicts with this project's no-real-network-in-tests rule (and would make
CI flaky on a cold cache), so `src/query_runner` instead executes directly
against the workspace's SQLite file through `sqlx`, read-only at the
connection level (not just by convention), with the same safety properties
DuckDB would have given:

- **Single statement only**: `sql_text` must start with `SELECT`/`WITH` and
  contain no second `;`-separated statement.
- **Row cap**: wrapped as `SELECT * FROM (<sql>) LIMIT {QUERY_MAX_ROWS + 1}`;
  if the extra row comes back, `truncated: true` and it's dropped.
- **Timeout**: the whole execution runs inside `tokio::time::timeout`. This
  is an honest limitation worth naming: sqlx's SQLite driver doesn't expose
  forced interruption (`sqlite3_interrupt`) to safe Rust, so a timed-out
  query may keep running to completion on its own connection in the
  background rather than being killed outright. Acceptable for now since
  connections are per-request and short-lived; revisit if long-running
  queries become a real resource problem.
- **Named params, bound not interpolated**: `:name`/`$name` placeholders are
  rewritten to positional `?`s (the only mechanism sqlx's runtime, non-macro
  query API supports) and bound through the driver — never string-formatted
  into the SQL text.

Revisit the DuckDB path once `sqlite_scanner` can be statically linked, or if
extension autoload over the network becomes acceptable for this deployment
(e.g. a build step that vendors it once, offline, ahead of time).

## DuckLake (later)

Runner-ingested data (as opposed to the editable app data above) is planned
to live in DuckLake: a Postgres schema named `ducklake` holding the catalog
(table/column metadata, snapshots) and Parquet files in object storage
holding the actual data, queried through DuckDB's `ducklake` extension
alongside the SQLite-backed workspace data. This is **documented only** —
no `ducklake` schema, catalog or runner exists yet. `runner_configs`
(created by this step's control-plane migration: `kind`, `config` jsonb,
`schedule_cron`, `enabled`) is the config row shape a future runner would
read; nothing executes it yet.

## Litestream (later)

Litestream replication of each workspace's SQLite file (continuous backup to
object storage, point-in-time restore) is planned but not implemented.
Nothing about the per-workspace file layout (`DATA_DIR/workspaces/{id}.db`)
should need to change to add it later.
