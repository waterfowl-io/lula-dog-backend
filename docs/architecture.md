# Architecture

Lula.dog is an agentic data platform: runners ingest data, SQLite is the
editable system of record, DuckDB queries across sources, and AI builds apps
(dashboards, CRM, ops tools) that are data, not code. This document covers
step 3: the control plane, auth, workspace routing, app definitions and the
query runner. For the sales tracker and lead-gen data model themselves, see
the main [`README.md`](../README.md) and [`docs/lead-gen.md`](lead-gen.md).

## Control plane vs. data plane

**Control plane (Postgres -- Supabase's project database in production,
`DATABASE_URL`/`APP__DATABASE__URL`, or the legacy `CONTROL_DATABASE_URL`):**
everything about *who* and *what*, never customer data:

- `profiles` — one row per authenticated user, keyed by their verified
  JWT `sub`. No foreign key to any Supabase-managed table: this service
  verifies identity itself (`src/auth`) and connects to Postgres directly
  as a privileged role, so there is nothing to reference.
- `workspaces`, `workspace_members` — a workspace and its members, each with
  a role (`owner`, `admin`, `member`, `viewer`; see `Role::can_assign`).
- `workspace_invites` — pending invites by email, claimed automatically the
  first time the invited address is seen in a verified token (see
  [Request flow](#request-flow-jwt--user--membership--workspace-pool)
  below and `src/control/invites.rs`). Phase 1: the operator creates the
  auth user from the Supabase dashboard; there is no API-driven invite yet.
- `app_definitions`, `app_definition_versions` — AI-generated apps as
  versioned JSON (see [App definitions are data](#app-definitions-are-data)).
- `saved_queries` — named SQL a component binds to.
- `schema_catalog` — a cached, introspected table/column catalog per
  workspace, refreshed by `GET /w/{ws}/schema`.
- `runner_configs` — ingestion runner configuration (kind, config, cron
  schedule, enabled). Created by this step's migration; no runner executes
  them yet.

Migrations live in `migrations/control/`, applied on startup, separate from
the data-plane SQLite migrations in `migrations/`. They never reference
Supabase's `auth` schema (no `auth.users` stand-in, no `auth.uid()`, no RLS
keyed on either): this service verifies identity and workspace membership
itself, in Rust (`src/auth`, `src/workspace_db::scope`, `src/members.rs`),
not through Postgres row-level security, so the migration applies cleanly
to a bare Postgres with no `auth` schema at all -- a real Supabase project's
`auth` schema is simply never touched.

**Data plane (SQLite, one file per workspace):** customer data never lives
in Postgres. Editable app data (opportunities, tasks, timeline, proposals,
leads, companies, target profiles, ...) is one SQLite file per workspace,
created and migrated on first use under `DATA_DIR/workspaces/{id}.db` (see
`src/workspace_db.rs`). Runner-ingested data will live in DuckLake instead
(see [below](#ducklake-later)); DuckDB queries across both.

One exception, for backward compatibility: the workspace id
`00000000-0000-0000-0000-000000000000` (`config::DEFAULT_WORKSPACE_ID`) is
still the implicit default for the CLI's `--workspace` flag, but its data
now lives under `DATA_DIR` like every other workspace (it no longer reads
`DATABASE_URL` directly -- that name is now the control-plane Postgres URL).

## Request flow: JWT → user → membership → workspace pool

1. **Auth** (`src/auth`, ported from Desde0's `TokenVerifier`/`OidcVerifier`
   seam). `AuthUser` is an Axum extractor. Under `auth.mode = "jwks"` (the
   default), `"hs256"` or `"hybrid"`, it reads `Authorization: Bearer <jwt>`,
   decodes the header to find `alg`/`kid`, and verifies: ES256/RS256 against
   `auth.jwks_url` (cached by `kid`, refetched on a miss), or HS256 against
   `auth.jwt_secret`. `hybrid` configures both and dispatches per token by
   its header `alg`, so a locally-minted HS256 token and a real provider's
   ES256/RS256 token validate against the same running server; algorithm
   confusion is impossible by construction (see `src/auth/oidc.rs`'s module
   doc). `exp`, `aud` and `iss` are all validated, and the token must carry
   an `email` claim (checked in the extractor, since invite acceptance needs
   it). The result is `{id: Uuid, email: String}` from the token's
   `sub`/`email` claims, plus the `aal`/`app_metadata` claims used below and
   by `src/chat`'s build-mode gate.

   Under `auth.mode = "dev"` (legacy: `AUTH_MODE=dev`), verification is
   skipped entirely: every request is a fixed dev user (`DEV_USER_ID`,
   default `config::DEFAULT_DEV_USER_ID`), overridable per-request with
   `X-Dev-User: <uuid>`, and always treated as `aal2`. This only activates
   on an explicit `dev` mode (never a fallback), logs a loud warning on
   startup, and `Config::validate` refuses to boot in it outside
   `Env::Dev` (`APP_ENV=dev`).

   Every route requires `aal2` (a second factor on this session) except
   `GET /me`, so the frontend can render the TOTP enrollment/challenge step
   before anything else; below it, 403 `mfa_required`
   (`AppError::MfaRequired`), distinct from the generic `forbidden` a
   membership check returns. `tests/auth.rs`'s router-walk test enumerates
   every route from the live OpenAPI document and asserts this.

   The same extraction step provisions: it upserts the caller's `profiles`
   row and claims any still-pending `workspace_invites` for their verified
   email (`src/auth::provision`, best-effort with a 500ms ceiling, same
   rule `AUTH_MODE=dev` demonstrates elsewhere -- a transient DB hiccup must
   not reject an already-authenticated caller). See
   [Invites and members](#invites-and-members) below.

2. **Workspace membership** (`src/workspace_db.rs::scope`, a middleware on
   every `/w/{workspace_id}/...` route). Given the authenticated user and the
   path's `workspace_id`, it looks up the user's role in
   `workspace_members`. No row → 403. A write (any method but
   GET/HEAD/OPTIONS) from a `viewer` → 403. If no control-plane URL is
   configured, this check is skipped entirely and every workspace id is
   treated as accessible with role `owner` — a deliberate, documented bypass
   (logged once at startup, like `auth.mode = "dev"`) so the data plane and
   most of the test suite work without standing up Postgres. Tests that need
   real enforcement (`tests/workspaces.rs`, `tests/invites.rs`) go through
   `TestApp::with_control`, gated on `TEST_CONTROL_DATABASE_URL`.

   `/workspaces/{id}/invites`, `/workspaces/{id}/members` and
   `/workspaces/{id}/transfer-ownership` (`src/members.rs`) are siblings of
   `/workspaces`, not nested under `/w/{workspace_id}`, so they resolve
   membership the same way but enforce their own `admin`-or-above gate on
   top of it, plus `Role::can_assign` for owner-touching changes and a
   last-owner check before any demotion or removal (409 if it would leave
   the workspace with no owner). See
   [Invites and members](#invites-and-members).

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

## Invites and members

Roles, least to most privileged (this is the matrix Nolan gave on
2026-10-25, which supersedes the auth plan's original one -- see `Role`'s
doc comment in `src/control/workspaces.rs`, the single source of truth):

- `viewer`: read-only. No data writes, no chat (data or build mode), no
  publish/discard/rollback.
- `member`: `viewer` + write workspace data, build-mode chat, publish,
  discard and roll back app versions.
- `admin`: `member` + share/unshare (invite, remove and change the role of
  `viewer`/`member`/`admin` members) + delete any workspace-level resource
  or member.
- `owner`: `admin` + the only role that may grant or remove `owner` or
  call `transfer-ownership`. A workspace always keeps at least one owner.

Every gate that enforces a minimum role -- `workspace_db::scope` (workspace
data writes, build-mode chat, publish/discard/rollback need `member`+; a
DELETE needs `admin`+), `members::require_admin` (`admin`+) and the chat
build-mode check -- goes through the same `Role::require(self, min)`
(`src/control/workspaces.rs`), so none of them can quietly compare
differently from the others. `src/actions::dispatch`, the action-dispatch
layer chat's data-mode write tools and the HTTP write endpoints both call
into, deliberately has no role parameter at all: both callers already sit
behind the one `scope` gate on the same route, so there is nothing to
duplicate there. It also has no action that deletes anything -- see below.

**Every DELETE route is `admin`+ (Nolan, 2026-10-25: "delete any
workspace-level resource or member" -- no carve-out for ordinary data),
enumerated so a new one can't quietly ship at `member`+:**

| Route | Minimum role | Why |
|---|---|---|
| `DELETE /workspaces/{id}/invites/{invite_id}` | `admin` (owner-role invite needs owner) | `members::require_admin` + `Role::can_assign` |
| `DELETE /workspaces/{id}/members/{user_id}` | `admin` (owner needs owner) | `members::require_admin` + `Role::can_assign` |
| `DELETE /w/{ws}/tasks/{id}` | `admin` | `workspace_db::scope`: any DELETE under `/w/{workspace_id}` needs `admin`+ |
| `DELETE /w/{ws}/target-profiles/{id}` | `admin` | Same gate as tasks |

Undoing a chat action (`POST .../chat/actions/{id}/undo`) isn't a delete in
this sense, even though undoing a `task.create` happens to delete the task
row it created: it's a POST that reverses one specific write the caller
who's already in this conversation made, gated the same `member`+ way as
every other data write under `/w/{workspace_id}`, not a standalone deletion
capability -- and it can't be pointed at an arbitrary record, since the
row(s) it touches come from the stored `Undo` payload the *original*
action recorded, never from the undo request itself. There's no "delete
your own draft" endpoint distinct from `discard` (`member`+, see above) to
make a separate call about. Relatedly, `src/actions::dispatch` -- the one
path both chat's data-mode write tools and the UI's own buttons go
through -- has no delete-capable action at all (`KNOWN_ACTIONS` in
`src/app_definition/mod.rs` has no `*.delete`); the only thing that deletes
a task row is `Undo::DeleteTask`, reachable only by undoing the exact
`task.create` that made it. So a `member`, who can't call the HTTP DELETE
route, has no path to deleting anything via chat either. Workspace
deletion, if it's ever added, should stay `owner`-only per the original
plan -- nothing in the matrix update says otherwise, and no such endpoint
exists today.

Who may change a role is one function, `Role::can_assign(actor, from, to)`
(`src/control/workspaces.rs`), tested as a table in `src/members.rs`:
`from`/`to` are `None` for "no membership yet" (an invite) and "membership
removed", so the same rule covers inviting, promoting, demoting, removing
and transferring ownership (modeled as `owner -> owner`, since transferring
IS granting `owner` to someone else):

- An `admin` may invite, promote, demote or remove `viewer`/`member`/`admin`.
- Only an `owner` may grant or remove `owner` -- which is also why only an
  owner may call `POST /workspaces/{id}/transfer-ownership`.
- Below `admin`, nothing is assignable; `viewer`/`member` can't even invite.

A change is additionally refused with 409 if it would leave the workspace
with no owner at all, never assumed safe just because the role pair passed
`can_assign`. `change_role`/`remove_member`/`transfer_ownership`
(`src/control/workspaces.rs`) each run this check in the SAME transaction
as the write, after `SELECT ... FOR UPDATE`-locking the workspace's member
rows: a naive separate "count owners, then write" let two concurrent
demotions of a workspace's two owners each see "the other owner" still in
the count and both proceed, leaving zero. The lock makes the second call
block until the first commits, so it re-checks against the up-to-date
membership instead. `workspaces::owner_count_excluding` (no lock) still
exists for display/advisory reads that aren't making a change.

**Creating the auth user is phase 1, following Desde0**: the operator
clicks "Invite user" in the Supabase dashboard. This service's own
`workspace_invites` table is what routes that person into the right
workspace with the right role the first time they're seen here:
`POST /workspaces/{id}/invites {email, role}` creates a pending row
(14-day expiry), and `src/auth::provision` claims it automatically on the
invited address's first authenticated request, matched on the VERIFIED
`email` claim only (never a client-supplied one) and only while unaccepted
and unexpired. Phase 2 (not built yet) would add `IdentityAdmin::invite` so
the operator never has to touch the dashboard -- see Desde0's
`src/auth/identity.rs` for the shape.

Invite/member endpoints are workspace-scoped: an invite or member id that
belongs to a DIFFERENT workspace than the one in the URL reads as 404 (or,
if the caller isn't even a member of the url's workspace, the same 403 a
data-plane route gives a non-member) -- never a cross-workspace leak.

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
