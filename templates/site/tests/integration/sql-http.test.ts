// Proves: @lula/sdk's db.sql in http mode talks to a real sql.js-backed
// server (tests/support/sqlite-actor.ts) built from this template's own
// migrations/0001_init.sql — insert+select round-trips with params, a
// SQL-injection-shaped value is stored and returned literally instead of
// being executed, and currentUser() hits GET /v1/me on the same actor.
// Run: npm run test:integration -- tests/integration/sql-http.test.ts
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fileURLToPath } from "node:url";
import { startSqliteActor, type SqliteActor } from "../support/sqlite-actor";
import { configure, resetConfig } from "../../packages/lula-sdk/src/config";
import { db, currentUser } from "../../packages/lula-sdk/src/db";

const migrationsDir = fileURLToPath(new URL("../../migrations", import.meta.url));

let actor: SqliteActor;

beforeEach(async () => {
  actor = await startSqliteActor({ migrationsDir });
  configure({ mode: "http", baseUrl: actor.url });
});

afterEach(() => {
  actor.close();
  resetConfig();
});

describe("db.sql over http against a real sqlite actor", () => {
  it("inserts a row with params and selects it back", async () => {
    await db.sql`INSERT INTO notes (title, body) VALUES (${"Integration title"}, ${"Integration body"})`;

    const rows = await db.sql<{
      id: number;
      title: string;
      body: string;
    }>`SELECT * FROM notes WHERE title = ${"Integration title"}`;

    expect(rows).toHaveLength(1);
    expect(rows[0].title).toBe("Integration title");
    expect(rows[0].body).toBe("Integration body");
  });

  it("stores a SQL-injection-shaped param value as literal data, not executable SQL", async () => {
    const malicious = "x'); DROP TABLE notes; --";

    await db.sql`INSERT INTO notes (title, body) VALUES (${malicious}, ${"n/a"})`;

    const rows = await db.sql<{ title: string }>`SELECT * FROM notes WHERE title = ${malicious}`;
    expect(rows).toHaveLength(1);
    expect(rows[0].title).toBe(malicious);

    // the notes table must still exist and be queryable: it wasn't dropped
    const allNotes = await db.sql`SELECT * FROM notes`;
    expect(allNotes.length).toBeGreaterThan(0);
  });

  it("currentUser() GETs /v1/me on the same actor", async () => {
    const user = await currentUser();
    expect(user.id).toBe("demo-user");
  });
});
