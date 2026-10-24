# lula-dog-backend

Lula.dog gives AI agents structured data. This is the backend: Rust, Axum and
SQLite, with SQLite as the system of record. DuckDB for analytics and Rust
ingestion runners come later.

The first app is a lightweight sales opportunity tracker for Jack, who sells
packaging materials (corrugated boxes, mailers, pallets). Jack pastes an email
thread or call notes onto a deal; the backend saves it, has an AI extractor
pull out contacts, amounts, stage, next steps, dates and follow-up tasks, and
turns that into proposals he can accept or reject.

## Run locally

Needs a Rust toolchain (stable, edition 2024). SQLite is bundled; nothing else
to install.

```sh
cargo run
```

That starts the API on `http://127.0.0.1:3000` with a SQLite file `lula.db` in
the current directory. The file is created on first start and the checked-in
migrations (`migrations/`) are applied on every start.

Configuration is by environment variable; a `.env` file is read if present.
Copy `.env.example` to `.env` to change anything. All variables are optional:

| Variable | Default | |
|---|---|---|
| `DATABASE_URL` | `sqlite://lula.db` | SQLite file |
| `BIND_ADDR` | `127.0.0.1:3000` | Listen address |
| `RUST_LOG` | `info` | Log filter |
| `EXTRACTOR` | `stub` | `stub` or `grok` |
| `XAI_API_KEY` | | Required when `EXTRACTOR=grok` |
| `XAI_MODEL` | | Required when `EXTRACTOR=grok` |
| `XAI_BASE_URL` | `https://api.x.ai/v1` | xAI API root |

## Seed

```sh
cargo run -- seed
```

Fills an empty database with clearly fake packaging-sales data: 8
opportunities across every stage (corrugated mailer and double-wall box
quotes, poly mailers, pallets, a repeat order or two), timeline entries, tasks,
a pending proposal, and two stale deals (one untouched for 20 days, one with an
overdue task). Dates are relative to now, so the data stays realistic whenever
you seed. Timeline entries go through the real paste flow with the stub
extractor. It refuses to run on a non-empty database; delete `lula.db` to start
over.

## Test

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests drive the router in-process against a fresh SQLite file per test. The
xAI extractor is tested against a local fake of the chat completions endpoint,
so no key or network is needed. CI (`.github/workflows/ci.yml`) runs the same
three commands plus `cargo deny check`.

## API

All bodies are JSON. Errors are `{"error": {"code", "message"}}` with codes
`bad_request`, `not_found`, `conflict`, `internal`. Unknown request fields are
rejected. Timestamps are accepted as RFC 3339, RFC 2822, `YYYY-MM-DD HH:MM:SS`
or `YYYY-MM-DD`, and always returned as `YYYY-MM-DD HH:MM:SS` UTC. The OpenAPI
document is served at `GET /openapi.json`.

| Method | Path | |
|---|---|---|
| GET | `/health` | Liveness plus a database round trip |
| GET | `/openapi.json` | OpenAPI 3.1 document |
| GET | `/opportunities?stage=` | List, with `open_task_count`, `overdue_task_count`, `stale` |
| POST | `/opportunities` | Create (`company` required; `stage` defaults to `lead`) |
| GET | `/opportunities/:id` | Deal + timeline + pending proposals + tasks |
| PATCH | `/opportunities/:id` | Partial update; `null` clears a nullable field |
| POST | `/opportunities/:id/tasks` | Add a task `{title, due_at?}` |
| PATCH | `/tasks/:id` | Edit `{title?, due_at?, done?}` |
| DELETE | `/tasks/:id` | Delete a task |
| GET | `/alerts/stale` | Stale deals, longest without a touch first |
| POST | `/opportunities/:id/paste` | Paste `{text}`; see below |
| POST | `/proposals/:id/accept` | Accept and apply a proposal |
| POST | `/proposals/:id/reject` | Reject a proposal |
| POST | `/opportunities/:id/proposals/accept-all` | Accept every pending proposal |
| POST | `/opportunities/:id/proposals/dismiss-all` | Reject every pending proposal |

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

## Layout

Code is organized by feature: each of `src/opportunities`, `src/tasks`,
`src/timeline`, `src/proposals`, `src/paste`, `src/alerts` holds its handlers,
SQL and types. `src/proposals` is the one writer of deal field values after
creation.

## Not yet

- Litestream replication of the SQLite file comes later.
- No auth, frontend, email sync or DuckDB yet.
