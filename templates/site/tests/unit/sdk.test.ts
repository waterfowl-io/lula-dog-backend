// Proves: @lula/sdk's tagged-template SQL builder always parameterizes
// values (never string-interpolates), mock mode serves the notes example
// from an in-memory store, and http mode calls fetch with the right URL,
// method, credentials, and JSON body.
// Run: npm run test:unit -- tests/unit/sdk.test.ts
import { beforeEach, describe, expect, it, vi } from "vitest";
import { buildQuery } from "../../packages/lula-sdk/src/query";
import { configure, resetConfig } from "../../packages/lula-sdk/src/config";
import { db, currentUser } from "../../packages/lula-sdk/src/db";
import { resetMockStore } from "../../packages/lula-sdk/src/mock";

function tag(strings: TemplateStringsArray, ...values: unknown[]) {
  return buildQuery(strings, values);
}

beforeEach(() => {
  resetConfig();
  resetMockStore();
});

describe("tagged template parameterization", () => {
  it("replaces interpolated values with ? placeholders and collects params", () => {
    const id = 42;
    const title = "Robert'); DROP TABLE notes;--";
    const query = tag`SELECT * FROM notes WHERE id = ${id} AND title = ${title}`;

    expect(query.sql).toBe("SELECT * FROM notes WHERE id = ? AND title = ?");
    expect(query.params).toEqual([id, title]);
    expect(query.sql).not.toContain(title);
  });
});

describe("mock mode", () => {
  beforeEach(() => {
    configure({ mode: "mock", baseUrl: "" });
  });

  it("selects the seeded notes", async () => {
    const rows = await db.sql`SELECT * FROM notes`;
    expect(rows.length).toBe(2);
  });

  it("inserts a new note and makes it visible to subsequent selects", async () => {
    const before = await db.sql`SELECT * FROM notes`;

    const [inserted] = await db.sql<{
      id: number;
      title: string;
      body: string;
    }>`INSERT INTO notes (title, body) VALUES (${"Test title"}, ${"Test body"})`;

    expect(inserted.title).toBe("Test title");
    expect(inserted.body).toBe("Test body");

    const after = await db.sql`SELECT * FROM notes`;
    expect(after.length).toBe(before.length + 1);
  });

  it("returns an empty array for unrecognized queries", async () => {
    const rows = await db.sql`SELECT * FROM widgets`;
    expect(rows).toEqual([]);
  });

  it("returns a demo user from currentUser", async () => {
    const user = await currentUser();
    expect(user.id).toBe("demo-user");
  });
});

describe("http mode", () => {
  beforeEach(() => {
    configure({ mode: "http", baseUrl: "https://api.example.com", token: "abc123" });
  });

  it("POSTs { sql, params } JSON to <baseUrl>/v1/sql with credentials included", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      statusText: "OK",
      json: async () => ({ rows: [{ id: 1, title: "From server" }] }),
    });
    vi.stubGlobal("fetch", fetchMock);

    const rows = await db.sql`SELECT * FROM notes WHERE id = ${1}`;

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, options] = fetchMock.mock.calls[0];
    expect(url).toBe("https://api.example.com/v1/sql");
    expect(options.method).toBe("POST");
    expect(options.credentials).toBe("include");
    expect(JSON.parse(options.body)).toEqual({
      sql: "SELECT * FROM notes WHERE id = ?",
      params: [1],
    });
    expect(options.headers.Authorization).toBe("Bearer abc123");
    expect(rows).toEqual([{ id: 1, title: "From server" }]);

    vi.unstubAllGlobals();
  });

  it("GETs <baseUrl>/v1/me for currentUser", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      statusText: "OK",
      json: async () => ({ id: "u1", name: "Ada", email: "ada@example.com" }),
    });
    vi.stubGlobal("fetch", fetchMock);

    const user = await currentUser();

    expect(fetchMock).toHaveBeenCalledWith(
      "https://api.example.com/v1/me",
      expect.objectContaining({ method: "GET", credentials: "include" }),
    );
    expect(user).toEqual({ id: "u1", name: "Ada", email: "ada@example.com" });

    vi.unstubAllGlobals();
  });
});
