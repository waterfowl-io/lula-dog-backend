# Lead generation for Jack

Jack already uses AI chat to find packaging-buyer leads, but it keeps
surfacing the same handful of companies. The fix isn't a smarter prompt,
it's structure: a saved target profile, a record of every company already
seen, a log of what's been searched so runs advance instead of repeating,
structured sources instead of open-ended chat, and enrichment/classification
that turns a bare filing into something Jack can act on.

This implements that pipeline end to end, offline by default (every
network-touching step has a deterministic stub), with one real structured
source (Illinois SOS LLC filings) and a second queued up for tomorrow
(Bizapedia). Everything below marked **unverified** is a real gap — either
because it needs a human (a phone call, a paid trial) or because this box
had no way to confirm it (e.g. no browser to test a site that blocks
non-browser traffic).

## Data sources

### Illinois Secretary of State — free bulk files

The Business Data Transparency Act (P.A. 102-0049, 2021) requires the
Illinois SOS to publish corporation and LLC filings as free open data:
<https://www.ilsos.gov/data/bus-serv-home.html>. The files are fixed-width,
mainframe-style exports, keyed by file number, full snapshots rather than
deltas. Fields include formation/status/dissolution dates, status,
registered agent, and officers/managers with names and addresses. They do
**not** include industry, website, phone or email — that's what the
enrichment step is for.

- **This box could not download the files directly** — fetching
  `ilsos.gov` timed out / was blocked here, consistent with a site that
  filters non-browser traffic. **Unverified**: whether that's a permanent
  bot-block or just this sandbox's network. Next step: try from a normal
  browser session, and call Illinois Business Services at
  217-782-6961 to ask (a) the real refresh cadence — the state's own page
  says daily, a third party (see below) says every few weeks, so that's
  **unverified** — and (b) whether automated/scripted downloads are
  permitted at all.
- The record layout used by this implementation (`src/importers/il_sos_llc.rs`)
  is **not guessed**: it's the field-position table published at
  <https://github.com/fgregg/il-corporate-filings>
  (`schemas/llcallmst.schema`, `llcallnam.schema`, `llcallagt.schema`,
  fetched and read directly while building this). That repo also nightly
  re-publishes the files as CSV
  (<https://github.com/fgregg/il-corporate-filings/releases/download/nightly/llc.zip>),
  which may be a more practical automated source than the state site
  itself once licensing/robots are checked — **unverified**, next step.
- The two-letter `status_code` on the master record (e.g. `AC`) is stored
  raw; the code-to-meaning dictionary is **unverified**, so the cheap
  filter's inactive-status check does not yet catch IL-SOS-sourced
  companies. Next step: find or derive the code table (the `docs/*.pdf`
  files in the same repo likely document it; this box did not parse them).
- The manager/officer file (`llcallmgr.txt`) is not joined yet — only
  master, name and (optionally) registered agent. See "Next steps" below.

### Bizapedia Pro Data — chosen first feed

<https://www.bizapedia.com/pro-data.aspx>: ~$99/month, a weekly CSV of new
filings with officers, roughly 13,500 new Illinois filings/month, 7-day
trial. **Chosen as the first commercial feed.** Nolan gets access the day
after this was written, so the exact CSV column names are **unverified** —
the importer (`src/importers/bizapedia.rs`) ships a best-guess default
column mapping built from Bizapedia's documented fields (filing date, entity
name, entity type, file number, status, principal/mailing address,
registered agent, officers/managers), overridable by a `--mapping <file>`
TOML file without a code change once real columns are known.

### Alternatives considered

- **BusinessNa.me Pro**: weekly CSV of new IL filings; price not published
  on the page seen. **Unverified** depth vs. Bizapedia.
- **OpenCorporates**: API/bulk, from about £2,250/yr for the tier that would
  cover this volume. Good normalization, probably overkill price for one
  state.
- **Data Axle**: new-business lead lists, likely including phone and SIC/NAICS
  industry codes directly (which would reduce reliance on crawling). Quote
  only, not evaluated — **unverified**.

### Enrichment

- **Website discovery**: Google Places Text Search (New). About $35 per
  1,000 requests after 1,000 free/month — **unverified**, pulled from
  public pricing pages, not confirmed against a live account.
- **Crawl + classify**: this repo's own crawler (home page + a few
  about/products/contact pages) feeding Grok for classification — see
  below.
- **Later**: People Data Labs or Apollo for company size/contacts once a
  lead has cleared classification, so that per-contact spend only happens
  on companies worth it.

## Pipeline

```
 import (bizapedia | il-sos-llc)
   -> archive raw file in the bucket, unchanged
   -> parse rows
   -> companies::upsert (dedupe/merge)
   -> company_sources (provenance, idempotent)
   -> cheap filter (once per newly-seen company)
   -> coverage_log row for the run
         |
         v
     enrich [--limit N]
   -> Places lookup for companies that passed the filter, not yet looked up
   -> crawl the discovered (or already-known) website, a few pages, text only
   -> company_enrichments rows (places, crawl)
         |
         v
     classify [--limit N]
   -> Grok (or the stub) scores company + profile + crawled text
   -> company_enrichments row (classify)
   -> fit_score >= profile.min_fit_score -> lead_candidates (status new)
         |
         v
   GET /leads -> Jack reviews, highest fit_score first
   accept -> opportunity (stage lead) in the step-1 tracker, linked
   reject {reason} / snooze {until}
```

Every step after import is **idempotent and incremental**: re-running
`enrich` or `classify` only processes companies that don't yet have a
`company_enrichments` row of that kind, so a cron job can call all three on
a schedule and each run only does new work.

## Schema

`migrations/0002_lead_gen.sql` (new; `0001_initial.sql` is untouched):

- **`target_profiles`** — Jack's saved search: industries, regions, size
  (min/max employees plus a freeform `size_notes`, since headcount isn't
  always the signal), include/exclude keywords, `min_fit_score`. Editable
  via `GET/POST /target-profiles`, `GET/PATCH/DELETE /target-profiles/{id}`.
  **Not seeded by the migration.** A default profile for Jack (Illinois,
  packaging-buying industries, `min_fit_score` from `LEAD_MIN_FIT_SCORE`) is
  inserted by `target_profiles::seed_default`, called from
  `cargo run -- seed` — chosen over a migration seed so an empty database
  stays genuinely empty until asked for data, matching how step-1's seed
  already works.
- **`companies`** — one row per discovered business. Deduped on normalized
  `domain` when known (partial unique index), else on
  `(jurisdiction, state_file_number)` (partial unique index). A company
  first seen from a filing (file number, no domain) that later gets a
  domain from Places or a second source is **updated in place**, never
  duplicated — see `companies::upsert` and `tests/companies.rs`.
  `filter_status`/`filter_reason`/`pre_score` record the cheap filter's
  verdict.
- **`company_sources`** — provenance, one row per raw record that
  contributed to a company. Unique on `(source, source_record_key)`, so
  re-importing a file (or one with overlapping rows) never creates a
  duplicate.
- **`coverage_log`** — one row per import or search *run* (not per row):
  source, query/region/category, the period covered (e.g. the file name),
  and seen/new/duplicate/filtered/error counts. Per-row idempotency lives in
  `company_sources`/`company_enrichments`; this table is the human-visible
  record of what a run covered.
- **`enrichment_runs`** / **`company_enrichments`** — one `enrichment_runs`
  row per batch (`enrich` writes a `places` run and a `crawl` run each
  time it's invoked; `classify` writes one `classify` run), and one
  `company_enrichments` row per company per kind with the raw result,
  any bucket files written, and status/error. **Selection for `enrich` and
  `classify` is "no existing row of this kind for this company"** — that's
  the mechanism that makes repeated runs advance through uncovered space
  instead of repeating work. A company with no discoverable website is
  deliberately left *without* a `crawl` row (not marked done with an
  error), so it's retried automatically once a website turns up.
- **`lead_candidates`** — a classified company above threshold.
  `company_id` is unique while a candidate is **open** (`new` or
  `snoozed`, partial unique index), so a company is never queued twice at
  once, but can be re-queued after a rejection (e.g. the profile changes)
  since the index doesn't cover `rejected`/`accepted`. `opportunity_id` is
  set on accept — that's the company-to-opportunity link, added without any
  change to the step-1 `opportunities` table or its tests.

## Cheap filter

Runs once per newly-seen company, no network (`src/companies/filter.rs`):

1. **Inactive status** — a lowercase substring match against
   `dissolved`/`revoked`/`inactive`/`terminated`/`withdrawn`/`expired`/`forfeited`.
   (Does not yet fire for IL SOS raw status codes — see above.)
2. **Registered-agent / holding-company shells** — a small constant list
   (`registered agent`, `holdings`, `holding company`, `capital`,
   `investments`, `properties`, `realty`, `trust`) matched against the
   normalized name, plus a direct check that the entity's name doesn't
   equal its own registered agent's name (a strong shell signal).
3. **Exclude keywords** from the target profile.
4. Anything left over is **scored**: 10 points per include-keyword or
   industry word found in the name, recorded as `pre_score` for triage
   before enrichment spend.

## Dedupe and the coverage log, together

The two mechanisms that stop Jack from seeing the same company twice:

- **Identity**: `companies` dedupes by domain or filing identity (above),
  so the same business from two sources, or re-imported twice, is one row.
- **Progress**: `company_enrichments` existence per kind is what makes
  `enrich`/`classify` **advance** — a company already looked up in Places
  is never looked up again; a company already classified is never
  reclassified (until a real "reclassify" need arises, which isn't built
  yet). `coverage_log` is the audit trail of *runs*, for the human and for
  future per-source/region/query planning (e.g. "we've imported every
  Bizapedia file through October").

## CLI

```sh
cargo run -- seed                                   # fake step-1 data + Jack's default profile
cargo run -- import bizapedia <path.csv> [--mapping <file>]
cargo run -- import il-sos-llc <path to llcallmst.txt>   # llcallnam.txt required alongside, llcallagt.txt optional
cargo run -- enrich [--limit N]                      # Places lookup, then crawl; default limit 50
cargo run -- classify [--limit N]                    # score + queue leads; requires a target profile to exist
```

All four of `PLACES`, `CRAWLER`, `CLASSIFIER` and the step-1 `EXTRACTOR`
default to network-free stubs, so the whole pipeline runs offline:

```sh
cargo run -- seed
cargo run -- import bizapedia tests/fixtures/bizapedia_fake_filings.csv
cargo run -- enrich
cargo run -- classify
cargo run                      # serve, then GET /leads
```

## API additions

| Method | Path | |
|---|---|---|
| GET/POST | `/target-profiles` | List / create |
| GET/PATCH/DELETE | `/target-profiles/{id}` | |
| GET | `/leads?status=&min_score=&industry=&profile_id=&limit=&offset=` | `status` defaults to `new`; a `snoozed` lead whose `snoozed_until` has passed reads back as `new` |
| POST | `/leads/{id}/accept` | Creates a step-1 opportunity (stage `lead`), a timeline note with website/phone/fit reason, links `lead_candidates.opportunity_id`; 409 if already decided |
| POST | `/leads/{id}/reject` | `{reason}` |
| POST | `/leads/{id}/snooze` | `{until}` |

## Deviations from the brief

- **Officers/managers** are joined for Bizapedia (best-effort, splitting a
  freeform field) but not for IL SOS yet (`llcallmgr.txt` not read) — see
  next steps.
- **Crawler size cap** is enforced two ways: a response whose
  `Content-Length` already exceeds the cap is rejected unread (the common
  case); a response with no/understated `Content-Length` is downloaded in
  full then truncated. A true streaming cap would close the second gap but
  needs `reqwest`'s `stream` feature plus `futures-util`; skipped to keep
  the crawler dependency-free. Documented in `src/crawler/http.rs`.
- **`robots.txt` handling** only honors `User-agent: *` / `Disallow` (no
  `Allow` overrides, no other agent groups) — enough to respect an opt-out,
  not a full implementation.

## Next steps

- Confirm IL SOS download cadence/permissibility by phone; try the
  `il-corporate-filings` nightly CSV mirror as an automated source.
- Decode the IL SOS status-code dictionary so inactive entities from that
  source get filtered too.
- Join `llcallmgr.txt` (managers) once the master/name/agent join is
  proven out.
- Get real Bizapedia column names and replace the best-guess mapping
  (or confirm it already matches).
- A cron runner for `import`/`enrich`/`classify` (the CLI commands are
  already shaped for it — idempotent, limit-bounded).
- DuckDB over the bucket's raw files for analytics once there's enough
  volume to need it.
- A frontend lead queue (the API is ready; nothing renders it yet).
- Contact-level enrichment (People Data Labs/Apollo) on accepted or
  high-scoring leads.
