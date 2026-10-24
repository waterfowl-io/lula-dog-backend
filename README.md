# lula-dog-backend

Lula.dog gives AI agents structured data. This is the backend: Rust, Axum,
Supabase Postgres as the control plane (workspaces, membership, app
definitions, saved queries) and one SQLite file per workspace as the system
of record for that workspace's data. See
[`docs/architecture.md`](docs/architecture.md) for the full picture: the
request flow (JWT → user → membership → workspace pool), app definitions as
versioned data, and the DuckLake plan for runner-ingested data.

The first app is a lightweight sales opportunity tracker for Jack, who sells
packaging materials (corrugated boxes, mailers, pallets). Jack pastes an email
thread or call notes onto a deal; the backend saves it, has an AI extractor
pull out contacts, amounts, stage, next steps, dates and follow-up tasks, and
turns that into proposals he can accept or reject.

The second piece is structured lead generation: instead of Jack re-asking an
AI chat for leads and getting the same companies back, the backend imports
business filings, dedupes and filters them, enriches and classifies the
survivors, and queues the good ones for Jack to accept straight into the
tracker above. See [`docs/lead-gen.md`](docs/lead-gen.md) for the research,
the pipeline and what's still open.

The third piece is AI-built apps: an app definition (pages, layout, fixed
components bound to saved SQL queries and named actions) is itself data,
versioned in the control plane, not code -- see
[`docs/architecture.md`](docs/architecture.md#app-definitions-are-data).

## Run locally

Needs a Rust toolchain (stable, edition 2024) and a C++ compiler (for a dev
dependency's build script). SQLite is bundled; nothing else to install for
the data plane.

The quickest way to run without Supabase at all:

```sh
AUTH_MODE=dev cargo run
```

`AUTH_MODE=dev` bypasses JWT verification (every request is a fixed dev
user, overridable with an `X-Dev-User: <uuid>` header) and, with no
`CONTROL_DATABASE_URL` set, workspace-membership checks are skipped too
(every workspace id is accessible as `owner`). That starts the API on
`http://127.0.0.1:3000` with a SQLite file `lula.db` in the current
directory for the legacy/default workspace
(`00000000-0000-0000-0000-000000000000`, see
[`docs/architecture.md`](docs/architecture.md)); the file is created on
first start and the checked-in migrations (`migrations/`) are applied on
every start. Try it:

```sh
curl -X POST http://127.0.0.1:3000/w/00000000-0000-0000-0000-000000000000/opportunities \
  -H 'content-type: application/json' -d '{"company": "Acme"}'
```

To run against a real Supabase project (or a local one via the Supabase
CLI): set `SUPABASE_URL` and `CONTROL_DATABASE_URL` (its Postgres
connection string), leave `AUTH_MODE` unset (defaults to `supabase`), and
send a real user's JWT as `Authorization: Bearer <token>`. The control-plane
migrations (`migrations/control/`) apply on startup, separately from the
per-workspace SQLite migrations.

Configuration is by environment variable; a `.env` file is read if present.
Copy `.env.example` to `.env` to change anything. All variables are optional:

| Variable | Default | |
|---|---|---|
| `CONTROL_DATABASE_URL` | unset | Supabase/Postgres control plane. Unset disables workspace-membership enforcement |
| `AUTH_MODE` | `supabase` | `supabase` or `dev` (bypasses JWT verification; never a fallback, always a loud warning) |
| `SUPABASE_URL` | | Required when `AUTH_MODE=supabase`; JWKS and issuer are derived from it |
| `SUPABASE_JWT_AUD` | `authenticated` | Expected JWT `aud` claim |
| `SUPABASE_JWT_SECRET` | | Legacy HS256 fallback; unset rejects HS256 tokens |
| `JWKS_CACHE_TTL_SECS` | `300` | JWKS cache TTL; an unknown `kid` refetches immediately regardless |
| `DEV_USER_ID` | `00000000-…-000000000001` | `AUTH_MODE=dev`'s fixed user id |
| `DATA_DIR` | `data` | Per-workspace SQLite files live under `DATA_DIR/workspaces/{id}.db` |
| `QUERY_MAX_ROWS` | `1000` | Row cap for `POST /w/{ws}/queries/{id}/run` |
| `QUERY_TIMEOUT_MS` | `5000` | Per-request query timeout |
| `DATABASE_URL` | `sqlite://lula.db` | SQLite file for the legacy/default workspace only; see `DATA_DIR` |
| `BIND_ADDR` | `127.0.0.1:3000` | Listen address |
| `RUST_LOG` | `info` | Log filter |
| `EXTRACTOR` | `stub` | `stub` or `grok` |
| `XAI_API_KEY` | | Required when `EXTRACTOR=grok` or `CLASSIFIER=grok` |
| `XAI_MODEL` | | Required when `EXTRACTOR=grok` or `CLASSIFIER=grok` |
| `XAI_BASE_URL` | `https://api.x.ai/v1` | xAI API root |
| `BUCKET_DIR` | `data/bucket` | Local stand-in for the object store; raw source files and crawled pages |
| `PLACES` | `stub` | `stub` or `google`, the website-lookup backend |
| `GOOGLE_PLACES_API_KEY` | | Required when `PLACES=google` |
| `CRAWLER` | `stub` | `stub` or `http`, the site-crawl backend |
| `CRAWL_MAX_PAGES` | `4` | Pages fetched per company, home page included |
| `CRAWL_TIMEOUT_SECS` | `10` | Per-request timeout for `CRAWLER=http` |
| `CRAWL_MAX_BYTES` | `1000000` | Response size cap for `CRAWLER=http` |
| `CLASSIFIER` | `stub` | `stub` or `grok`, the lead-scoring backend |
| `LEAD_MIN_FIT_SCORE` | `50` | `target_profiles.min_fit_score` for the default profile seeded by `seed` |

## Seed

```sh
cargo run -- seed
```

Fills an empty workspace database with clearly fake packaging-sales data: 8
opportunities across every stage (corrugated mailer and double-wall box
quotes, poly mailers, pallets, a repeat order or two), timeline entries, tasks,
a pending proposal, and two stale deals (one untouched for 20 days, one with an
overdue task). Dates are relative to now, so the data stays realistic whenever
you seed. Timeline entries go through the real paste flow with the stub
extractor. It refuses to run on a non-empty database; delete the workspace's
SQLite file to start over.

It also seeds Jack's default lead-gen target profile (Illinois, packaging-
buying industries), if none exists yet — see [Lead generation](#lead-generation).

With `CONTROL_DATABASE_URL` configured, `seed` also finds-or-creates a dev
user and their "Jack's Packaging" workspace in the control plane (the dev
user is `AUTH_MODE=dev`'s default, so a locally-running server sees it at
`GET /me` with no extra setup), then creates the saved queries and the
"Sales" app definition (pipeline kanban, deals table, stale-deals alerts,
pipeline-value chart, deal detail page, lead queue) for that workspace --
see `src/seed.rs` and the committed fixture at
[`fixtures/apps/sales.json`](fixtures/apps/sales.json). All of this is
idempotent; `--workspace <uuid>` seeds just that workspace's SQLite data and
skips the control-plane bootstrap.

Every CLI command (`seed`, `import`, `enrich`, `classify`) takes
`--workspace <uuid>` to pick which workspace's SQLite file to operate on;
the default is the legacy/default workspace
(`00000000-0000-0000-0000-000000000000`, i.e. `DATABASE_URL` directly).

## Test

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Tests drive the router in-process against a fresh SQLite file per test, with
`AUTH_MODE=dev` and no control plane (so workspace membership is bypassed --
see `tests/common/mod.rs`). The xAI extractor is tested against a local fake
of the chat completions endpoint, and JWT verification against a loopback
fake JWKS server (`tests/auth.rs`), so no key or real network is needed
either way.

Control-plane-backed tests (workspace membership/403s, `/me`, `/workspaces`,
app definitions, saved queries, the query runner, schema introspection --
`tests/workspaces.rs`, `tests/apps.rs`, `tests/queries.rs`) read
`TEST_CONTROL_DATABASE_URL` and skip themselves, printing a note, when it
isn't set; each creates its own profile/workspace with random ids, so
they're safe to run in parallel against a shared database. CI
(`.github/workflows/ci.yml`) runs a `postgres:17` service container and sets
`TEST_CONTROL_DATABASE_URL` so these run there too, plus `cargo deny check`.

## API

All bodies are JSON. Errors are `{"error": {"code", "message"}}` with codes
`bad_request`, `unauthorized`, `forbidden`, `not_found`, `conflict`,
`internal`. Unknown request fields are rejected. Timestamps are accepted as
RFC 3339, RFC 2822, `YYYY-MM-DD HH:MM:SS` or `YYYY-MM-DD`, and always
returned as `YYYY-MM-DD HH:MM:SS` UTC. The OpenAPI document is served at
`GET /openapi.json`.

Every request except `/health`, `/openapi.json` and
`/app-definition.schema.json` needs `Authorization: Bearer <jwt>` (or, under
`AUTH_MODE=dev`, nothing at all). Everything below `/w/{workspace_id}` also
needs the caller to be a member of that workspace -- a non-member gets 403,
and a `viewer` member gets 403 on any write. See
[`docs/architecture.md`](docs/architecture.md).

| Method | Path | |
|---|---|---|
| GET | `/health` | Liveness, plus a control-plane round trip if one is configured |
| GET | `/openapi.json` | OpenAPI 3.1 document |
| GET | `/app-definition.schema.json` | JSON Schema of the app definition format |
| GET | `/me` | The caller's profile + workspace memberships |
| GET | `/workspaces` | The caller's workspaces |
| POST | `/workspaces` | Create a workspace `{name}`; the caller becomes `owner` |
| GET | `/w/{ws}/opportunities?stage=` | List, with `open_task_count`, `overdue_task_count`, `stale` |
| POST | `/w/{ws}/opportunities` | Create (`company` required; `stage` defaults to `lead`) |
| GET | `/w/{ws}/opportunities/:id` | Deal + timeline + pending proposals + tasks |
| PATCH | `/w/{ws}/opportunities/:id` | Partial update; `null` clears a nullable field |
| POST | `/w/{ws}/opportunities/:id/tasks` | Add a task `{title, due_at?}` |
| PATCH | `/w/{ws}/tasks/:id` | Edit `{title?, due_at?, done?}` |
| DELETE | `/w/{ws}/tasks/:id` | Delete a task |
| GET | `/w/{ws}/alerts/stale` | Stale deals, longest without a touch first |
| POST | `/w/{ws}/opportunities/:id/paste` | Paste `{text}`; see below |
| POST | `/w/{ws}/proposals/:id/accept` | Accept and apply a proposal |
| POST | `/w/{ws}/proposals/:id/reject` | Reject a proposal |
| POST | `/w/{ws}/opportunities/:id/proposals/accept-all` | Accept every pending proposal |
| POST | `/w/{ws}/opportunities/:id/proposals/dismiss-all` | Reject every pending proposal |
| GET, POST | `/w/{ws}/target-profiles` | List / create a lead-gen target profile |
| GET, PATCH, DELETE | `/w/{ws}/target-profiles/:id` | |
| GET | `/w/{ws}/leads?status=&min_score=&industry=&profile_id=&limit=&offset=` | Lead queue; `status` defaults to `new`, sorted by fit score descending |
| POST | `/w/{ws}/leads/:id/accept` | Create a step-1 opportunity from a lead |
| POST | `/w/{ws}/leads/:id/reject` | `{reason}` |
| POST | `/w/{ws}/leads/:id/snooze` | `{until}` |
| GET, POST | `/w/{ws}/apps` | List / create an app definition `{key, name}` |
| GET | `/w/{ws}/apps/:app` | Published version, or the latest if nothing is published |
| GET, POST | `/w/{ws}/apps/:app/versions` | List versions / create one `{definition}` (validated) |
| POST | `/w/{ws}/apps/:app/versions/:version/publish` | Move the published pointer |
| POST | `/w/{ws}/apps/:app/rollback` | `{version}`: copy an old version into a new one and publish it |
| GET, POST | `/w/{ws}/queries` | List / create a saved query `{key, name, description?, sql_text, params?}` |
| GET | `/w/{ws}/queries/:id` | |
| POST | `/w/{ws}/queries/:id/run` | `{params?}` → `{query_id, sql, columns, rows, row_count, truncated, elapsed_ms}` |
| GET | `/w/{ws}/schema` | Introspect the workspace's tables/columns and refresh the cached catalog |

**Stages** are `lead`, `qualified`, `proposal`, `won`, `lost`, enforced by the
API and by a `CHECK` constraint.

**Stale** means an open deal (not `won`/`lost`) whose `last_touch_at` (or
`created_at` if never touched) is more than 14 days ago, or that has an
overdue open task. A task is overdue once its due date's calendar day (UTC)
has passed, so a task due today is not overdue yet.

**Paste** (`POST /opportunities/:id/paste`):

1. The text is saved as a timeline entry and committed first, so nothing is
   lost if the AI fails.
2. The extractor runs.
3. Each extracted field that is empty on the deal is filled in and recorded as
   an accepted change. A field that already has a different value is never
   overwritten; it gets a `proposed` change instead (identical pending
   proposals are not duplicated). Suggested tasks are created open, with
   `source_entry_id` set to the entry.
4. `last_touch_at` moves to the latest date found in the thread, if that is
   later than the current value and not in the future.

Response: `{entry, proposals, auto_filled, suggested_tasks, extraction_error}`.
If the extractor fails, the entry is still returned (status 201) with empty
lists and `extraction_error` set.

**Audit trail.** Every change to a deal field after creation is a
`field_changes` row: direct PATCH edits and auto-fills as `accepted`
(`entry_id` null for a PATCH), extractor proposals as `proposed` until
accepted or rejected. Timeline entries are append-only, enforced by triggers:
the raw text can never be edited or deleted, and the summary and `occurred_at`
are written once.

## AI extractor

`src/extractor/` defines the `Extractor` trait with two implementations:

- `StubExtractor` (default, `EXTRACTOR=stub`): deterministic line heuristics
  (`From:`, `Date:`, `Subject:`, `Next step:`, `TODO: ... by YYYY-MM-DD`, `$`
  amounts, stage keywords). Used by tests and for trying the API without a key.
- `GrokExtractor` (`EXTRACTOR=grok`): xAI Grok through the OpenAI-compatible
  chat completions API with a strict JSON-schema response format. Reads
  `XAI_API_KEY` and `XAI_MODEL`.

Extractor output is treated as untrusted: anything that does not validate (an
unknown stage, a malformed email, an unparseable date) is dropped.

## Lead generation

Full research and plan: [`docs/lead-gen.md`](docs/lead-gen.md). Short version:

```sh
cargo run -- seed                                        # Jack's default target profile
cargo run -- import bizapedia <path.csv> [--mapping <file>]
cargo run -- import il-sos-llc <path to llcallmst.txt>    # llcallnam.txt required alongside
cargo run -- enrich [--limit N]                           # Places lookup, then crawl
cargo run -- classify [--limit N]                         # score + queue leads
```

Importers archive the raw file unchanged in the bucket (`BUCKET_DIR`,
default `data/bucket`, under `sources/<source>/<date>/<file>`), parse it,
dedupe/merge into `companies` (by normalized domain, else by jurisdiction +
state file number — never duplicated either way), record provenance in
`company_sources`, run a network-free cheap filter (drops
inactive/dissolved entities and registered-agent/holding-company shells,
scores the rest against the target profile's keywords), and leave a
`coverage_log` row with counts. `enrich` looks up a website (Places) and
crawls a few pages (home plus about/products/contact, text only, respecting
`robots.txt` and a size cap); `classify` scores fit with an AI classifier
and queues anything at or above the profile's `min_fit_score` as a lead.
Every step only processes companies that don't already have a
`company_enrichments` row of that kind, so re-running any command is safe
and only does new work. `PLACES`, `CRAWLER` and `CLASSIFIER` all default to
deterministic stubs, so the whole pipeline runs with no network and no key.

`GET /leads` lists the queue (highest fit score first); accepting one
creates a step-1 opportunity (stage `lead`) with a timeline note carrying
the company's website/phone and why it was accepted.

## Layout

Code is organized by feature. Data plane (one SQLite file per workspace):
each of `src/opportunities`, `src/tasks`, `src/timeline`, `src/proposals`,
`src/paste`, `src/alerts` (step 1) and `src/target_profiles`,
`src/companies`, `src/coverage`, `src/importers`, `src/places`,
`src/crawler`, `src/classifier`, `src/enrichment`, `src/leads` (lead gen)
holds its handlers, SQL and types. `src/proposals` is the one writer of deal
field values after creation; `src/companies` is the one writer of company
rows. `src/schema` introspects a workspace's SQLite file.

Control plane (Supabase Postgres) and platform: `src/control` holds the
Postgres storage for profiles, workspaces/membership, app definitions,
saved queries and the schema catalog; `src/auth` verifies Supabase JWTs (or
bypasses under `AUTH_MODE=dev`); `src/workspace_db` is the per-workspace
SQLite pool registry and the `/w/{workspace_id}` membership middleware;
`src/me` serves `/me` and `/workspaces`; `src/apps` and `src/queries` are
the HTTP layer for app definitions and saved queries; `src/app_definition`
defines the app-definition schema/validation/JSON-Schema export;
`src/query_runner` executes a saved query read-only. See
[`docs/architecture.md`](docs/architecture.md).

## Not yet

- Litestream replication of each workspace's SQLite file comes later.
- Frontend or email sync yet.
- DuckLake (runner-ingested data, queried alongside the SQLite workspace
  data) -- documented, not implemented, in
  [`docs/architecture.md`](docs/architecture.md#ducklake-later).
- The query runner reads the workspace SQLite file directly (not through
  DuckDB): `duckdb-rs`'s bundled build doesn't statically link the
  `sqlite_scanner` extension DuckDB's own `ATTACH ... TYPE SQLITE` needs, and
  that extension otherwise auto-downloads over the network on first use. See
  `src/query_runner`.
- Membership management (inviting/removing members, changing roles) has no
  endpoint yet; only creating a workspace (the creator becomes `owner`).
- A component's `action` binding is singular: a list that needs two actions
  per row (e.g. proposals' accept *and* reject) needs two components for
  now. The app-definition schema is itself versioned data, so this can grow
  without breaking existing apps.
- A cron runner for the lead-gen CLI commands (they're already idempotent
  and limit-bounded, ready to be scheduled).
- Lead-gen next steps (IL SOS download automation, status-code mapping,
  manager/officer join, real Bizapedia columns, contact-level enrichment):
  see [`docs/lead-gen.md`](docs/lead-gen.md#next-steps).
