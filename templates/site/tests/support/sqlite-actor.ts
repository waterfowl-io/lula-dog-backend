// Test support: a tiny real SQLite-backed HTTP server used by integration
// tests. Implements POST /v1/sql (parameterized-only) and GET /v1/me,
// applies migrations/*.sql in filename order inside one transaction each,
// and records them in a _lula_migrations(version, name, checksum,
// applied_at) table so re-runs are idempotent and edited migrations are
// rejected. Not used by the app itself, only by tests/integration/*.
//
// Backed by sql.js (SQLite compiled to WASM) instead of a native binding
// so it builds without a C/C++ toolchain. SqliteDatabase/SqliteStatement
// below wrap sql.js's lower-level Database/Statement API to expose just
// the slice of the better-sqlite3 surface this file needs.
import initSqlJs, { type Database as SqlJsDatabase } from "sql.js";
import { createServer, type Server } from "node:http";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { createHash } from "node:crypto";

const SQL = await initSqlJs();

export type SqlParam = string | number | null | Uint8Array;

export interface SqliteStatement {
  readonly reader: boolean;
  all<Row = Record<string, unknown>>(...params: SqlParam[]): Row[];
  get<Row = Record<string, unknown>>(...params: SqlParam[]): Row | undefined;
  run(...params: SqlParam[]): { changes: number };
}

class Statement implements SqliteStatement {
  readonly reader: boolean;
  private raw: ReturnType<SqlJsDatabase["prepare"]>;

  constructor(db: SqlJsDatabase, sql: string) {
    this.raw = db.prepare(sql);
    this.reader = this.raw.getColumnNames().length > 0;
  }

  all<Row = Record<string, unknown>>(...params: SqlParam[]): Row[] {
    this.raw.bind(params);
    const rows: Row[] = [];
    while (this.raw.step()) {
      rows.push(this.raw.getAsObject() as Row);
    }
    this.raw.reset();
    this.raw.free();
    return rows;
  }

  get<Row = Record<string, unknown>>(...params: SqlParam[]): Row | undefined {
    return this.all<Row>(...params)[0];
  }

  run(...params: SqlParam[]): { changes: number } {
    this.raw.run(params);
    this.raw.free();
    return { changes: 1 };
  }
}

/** Wraps a sql.js Database with the subset of the better-sqlite3 API this file needs. */
export class SqliteDatabase {
  private raw: SqlJsDatabase;

  constructor(data?: ArrayLike<number>) {
    this.raw = new SQL.Database(data);
  }

  exec(sql: string): void {
    this.raw.run(sql);
  }

  prepare(sql: string): SqliteStatement {
    return new Statement(this.raw, sql);
  }

  /** Mirrors better-sqlite3's db.transaction(fn): BEGIN, run fn, COMMIT, or ROLLBACK and rethrow on error. */
  transaction<Args extends unknown[]>(fn: (...args: Args) => void): (...args: Args) => void {
    return (...args: Args) => {
      this.raw.run("BEGIN");
      try {
        fn(...args);
        this.raw.run("COMMIT");
      } catch (err) {
        this.raw.run("ROLLBACK");
        throw err;
      }
    };
  }

  close(): void {
    this.raw.close();
  }
}

export interface SqliteActor {
  url: string;
  db: SqliteDatabase;
  close: () => void;
}

/** Opens a sql.js-backed database, always in-memory unless given existing database bytes to load. */
export function createDatabase(dbPath?: string): SqliteDatabase {
  if (dbPath && dbPath !== ":memory:") {
    return new SqliteDatabase(readFileSync(dbPath));
  }
  return new SqliteDatabase();
}

export interface StartSqliteActorOptions {
  migrationsDir: string;
  seedPath?: string;
  dbPath?: string;
}

export function computeChecksum(content: string): string {
  return createHash("sha256").update(content).digest("hex");
}

function ensureMigrationsTable(db: SqliteDatabase): void {
  db.exec(`
    CREATE TABLE IF NOT EXISTS _lula_migrations (
      version TEXT PRIMARY KEY,
      name TEXT NOT NULL,
      checksum TEXT NOT NULL,
      applied_at TEXT NOT NULL
    )
  `);
}

/**
 * Applies every migrations/*.sql file in filename order inside its own
 * transaction, recording version/checksum in _lula_migrations. Re-running
 * is a no-op for already-applied migrations unless their content has
 * changed, in which case it throws instead of silently re-applying.
 */
export function applyMigrations(db: SqliteDatabase, migrationsDir: string): void {
  ensureMigrationsTable(db);

  const files = readdirSync(migrationsDir)
    .filter((name) => name.endsWith(".sql"))
    .sort();

  for (const file of files) {
    const match = file.match(/^(\d{4})_/);
    if (!match) continue;
    const version = match[1];
    const content = readFileSync(join(migrationsDir, file), "utf8");
    const checksum = computeChecksum(content);

    const existing = db
      .prepare("SELECT checksum FROM _lula_migrations WHERE version = ?")
      .get<{ checksum: string }>(version);

    if (existing) {
      if (existing.checksum !== checksum) {
        throw new Error(
          `migration ${file} was edited after being applied (checksum mismatch): refusing to re-apply`,
        );
      }
      continue;
    }

    const applyOnce = db.transaction(() => {
      db.exec(content);
      db.prepare(
        "INSERT INTO _lula_migrations (version, name, checksum, applied_at) VALUES (?, ?, ?, ?)",
      ).run(version, file, checksum, new Date().toISOString());
    });

    applyOnce();
  }
}

function readBody(req: import("node:http").IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    let body = "";
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => resolve(body));
    req.on("error", reject);
  });
}

function createActorServer(db: SqliteDatabase): Server {
  return createServer((req, res) => {
    if (req.method === "GET" && req.url === "/v1/me") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ id: "demo-user", name: "Demo User", email: "demo@example.com" }));
      return;
    }

    if (req.method === "POST" && req.url === "/v1/sql") {
      readBody(req)
        .then((body) => {
          const { sql, params } = JSON.parse(body) as { sql: string; params: SqlParam[] };
          const stmt = db.prepare(sql);
          const rows = stmt.reader ? stmt.all(...(params ?? [])) : (stmt.run(...(params ?? [])), []);
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ rows }));
        })
        .catch((err) => {
          res.writeHead(400, { "Content-Type": "application/json" });
          res.end(JSON.stringify({ error: err instanceof Error ? err.message : String(err) }));
        });
      return;
    }

    res.writeHead(404, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ error: "not found" }));
  });
}

/** Starts a real SQLite-backed HTTP actor on an ephemeral localhost port. */
export function startSqliteActor(options: StartSqliteActorOptions): Promise<SqliteActor> {
  const db = createDatabase(options.dbPath);
  applyMigrations(db, options.migrationsDir);

  if (options.seedPath) {
    db.exec(readFileSync(options.seedPath, "utf8"));
  }

  const server = createActorServer(db);

  return new Promise((resolve, reject) => {
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      resolve({
        url: `http://127.0.0.1:${port}`,
        db,
        close: () => {
          server.close();
          db.close();
        },
      });
    });
  });
}
