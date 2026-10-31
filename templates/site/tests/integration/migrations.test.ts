// Proves: applyMigrations (tests/support/sqlite-actor.ts) runs
// migrations/*.sql in filename order inside one transaction each,
// records them in _lula_migrations, is idempotent on re-run, rejects an
// already-applied migration whose content changed (checksum mismatch),
// and rolls back a failing migration fully (no partial schema, no
// migration record). Uses a real sql.js (SQLite/WASM) database, not a mock.
// Run: npm run test:integration -- tests/integration/migrations.test.ts
import { describe, expect, it } from "vitest";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { applyMigrations, createDatabase } from "../support/sqlite-actor";

function makeMigrationsDir(files: Record<string, string>): string {
  const dir = mkdtempSync(join(tmpdir(), "lula-migrations-"));
  for (const [name, content] of Object.entries(files)) {
    writeFileSync(join(dir, name), content);
  }
  return dir;
}

describe("applyMigrations", () => {
  it("applies migrations in filename order and records them", () => {
    const dir = makeMigrationsDir({
      "0001_init.sql": "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT);",
      "0002_add_body.sql": "ALTER TABLE notes ADD COLUMN body TEXT;",
    });
    const db = createDatabase();

    applyMigrations(db, dir);

    const applied = db.prepare("SELECT version, name FROM _lula_migrations ORDER BY version").all();
    expect(applied).toEqual([
      { version: "0001", name: "0001_init.sql" },
      { version: "0002", name: "0002_add_body.sql" },
    ]);

    const columns = (db.prepare("PRAGMA table_info(notes)").all() as Array<{ name: string }>).map(
      (c) => c.name,
    );
    expect(columns).toEqual(["id", "title", "body"]);

    rmSync(dir, { recursive: true, force: true });
    db.close();
  });

  it("is idempotent: re-running does not re-apply or error", () => {
    const dir = makeMigrationsDir({
      "0001_init.sql": "CREATE TABLE notes (id INTEGER PRIMARY KEY);",
    });
    const db = createDatabase();

    applyMigrations(db, dir);
    expect(() => applyMigrations(db, dir)).not.toThrow();

    const row = db.prepare("SELECT COUNT(*) as count FROM _lula_migrations").get() as {
      count: number;
    };
    expect(row.count).toBe(1);

    rmSync(dir, { recursive: true, force: true });
    db.close();
  });

  it("rejects an already-applied migration whose content was edited", () => {
    const dir = makeMigrationsDir({
      "0001_init.sql": "CREATE TABLE notes (id INTEGER PRIMARY KEY);",
    });
    const db = createDatabase();

    applyMigrations(db, dir);
    writeFileSync(join(dir, "0001_init.sql"), "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT);");

    expect(() => applyMigrations(db, dir)).toThrow(/checksum mismatch/);

    rmSync(dir, { recursive: true, force: true });
    db.close();
  });

  it("rolls back a failing migration fully, leaving no partial schema or record", () => {
    const dir = makeMigrationsDir({
      "0001_init.sql": "CREATE TABLE notes (id INTEGER PRIMARY KEY);",
    });
    const db = createDatabase();

    applyMigrations(db, dir); // 0001 succeeds on its own

    writeFileSync(
      join(dir, "0002_broken.sql"),
      "CREATE TABLE widgets (id INTEGER PRIMARY KEY); INSERT INTO not_a_table VALUES (1);",
    );
    expect(() => applyMigrations(db, dir)).toThrow();

    const widgets = db
      .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'widgets'")
      .all();
    expect(widgets).toEqual([]);

    const recorded = db.prepare("SELECT version FROM _lula_migrations WHERE version = '0002'").all();
    expect(recorded).toEqual([]);

    rmSync(dir, { recursive: true, force: true });
    db.close();
  });
});
