# Lula Site Template

A fully static SPA template: Vite + React + TypeScript + Tailwind v4, backed
by SQLite only. No DuckLake, no branch environments. Everything the app
needs ships as static assets plus a thin `@lula/sdk` client that talks to a
single `/v1/sql` endpoint.

## Layout

```
lula.yaml              app name + settings (sdk baseUrl/mode, storage engine)
index.html             Vite entry point, loads /app/main.tsx
app/
  main.tsx             configures the SDK, mounts <App />
  App.tsx              top-level component
  routes/Home.tsx       example route: db.sql against the notes table
  components/           shared UI components
  index.css             Tailwind entry (@import "tailwindcss")
migrations/
  0001_init.sql         versioned schema, applied in filename order
seed/
  seed.sql              optional starter data (not run by migrations)
functions/
  .gitkeep              reserved for future serverless functions, empty for now
packages/
  lula-sdk/              @lula/sdk workspace package (db.sql, useLiveQuery, currentUser)
scripts/
  validate.mjs           layout + dependency gate (see below)
  check-dist.mjs         asserts build output is content-hashed
dev/
  lula-site-test         single dev entry point: lint/unit/integration/build/all
tests/
  unit/                  pure, no network/fs, mock-mode SDK
  integration/           real SQLite actor (sql.js) over HTTP
  support/sqlite-actor.ts test-only HTTP+SQLite server used by integration tests
allowlist.json           allowed dependency names + version ranges
dist/                    build output (gitignored)
```

## Rules enforced by `npm run validate`

- `lula.yaml` must exist and declare a non-empty top-level `name`.
- Only these top-level paths are allowed: `lula.yaml`, `index.html`,
  `package.json`, `package-lock.json`, `allowlist.json`, `.gitignore`,
  `README.md`, `vite.config.ts`, `tsconfig*.json`, `app/`, `migrations/`,
  `seed/`, `functions/`, `packages/`, `scripts/`, `tests/`, `dev/`.
  `node_modules/`, `dist/`, and `coverage/` are ignored. Anything else is
  rejected.
- No symlinks anywhere in the tree.
- Migration filenames must match `^\d{4}_[a-z0-9_]+\.sql$` and be
  contiguous starting at `0001` (no gaps, no duplicates).
- `functions/` may only contain `.gitkeep`.
- Every dependency in `package.json` and `packages/*/package.json`
  (excluding `@lula/*` workspace packages) must appear in `allowlist.json`
  with a matching version range.

## Build output

`vite.config.ts` content-hashes every build asset (`assets/[name]-[hash].js`,
etc.) — only `dist/index.html` is unhashed. There is no `public/` directory,
so nothing unhashed gets copied into `dist/`. `scripts/check-dist.mjs` runs
after every build and fails if it finds an unhashed file other than
`index.html`.

## SQL access: `db.sql`

`@lula/sdk`'s `db.sql` is a tagged template. Interpolated values always
become `?` placeholders with a parallel `params` array — they are never
string-interpolated into the SQL text — and the `{ sql, params }` pair is
POSTed as JSON to `${baseUrl}/v1/sql` with `credentials: "include"`.

```ts
const rows = await db.sql`SELECT * FROM notes WHERE id = ${id}`;
```

With no `VITE_LULA_API_URL` set, the SDK defaults to `mode: "mock"` and
serves the notes example from an in-memory store seeded with fixture data,
so the template runs standalone with no backend.

## Testing

- `npm run test:unit` — `tests/unit/`: pure functions, mock-mode SDK,
  hand-written in-memory stubs. No network, no filesystem side effects.
  Runs with coverage (`@vitest/coverage-v8`).
- `npm run test:integration` — `tests/integration/`: spins up a real
  SQLite database (`sql.js`) behind an HTTP actor
  (`tests/support/sqlite-actor.ts`) that applies `migrations/*.sql` in
  order inside transactions and records them in `_lula_migrations`.
  Covers ordered/idempotent migrations, checksum-mismatch rejection,
  failed-migration rollback, and `db.sql` over HTTP including a SQL
  injection attempt via a parameter (stored literally, never executed).
- `./dev/lula-site-test all` runs the full gate: `lint` (typecheck +
  validate) → `unit` → `integration` → `build`, failing fast on the first
  broken step. This is what CI runs.

## npm scripts

| script              | what it does                                   |
| ------------------- | ----------------------------------------------- |
| `dev`                | Vite dev server                                 |
| `build`              | `tsc --noEmit` → `vite build` → `check-dist.mjs` |
| `validate`           | layout + dependency gate                        |
| `typecheck`          | `tsc --noEmit`                                   |
| `test`               | `test:unit` + `test:integration`                 |
| `test:unit`          | unit tests with coverage                         |
| `test:integration`   | integration tests against a real sqlite actor    |
