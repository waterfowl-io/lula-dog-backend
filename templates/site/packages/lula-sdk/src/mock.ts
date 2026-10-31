import type { SqlQuery } from "./query";

interface Note {
  id: number;
  title: string;
  body: string;
  created_at: string;
}

const FIXTURE_NOTES: readonly Note[] = [
  {
    id: 1,
    title: "Welcome to Lula",
    body: "Edit app/routes/Home.tsx to get started.",
    created_at: "2024-01-01T00:00:00.000Z",
  },
  {
    id: 2,
    title: "db.sql is parameterized",
    body: "Values are always sent as params, never interpolated.",
    created_at: "2024-01-01T00:00:00.000Z",
  },
];

let notes: Note[] = FIXTURE_NOTES.map((note) => ({ ...note }));
let nextId = FIXTURE_NOTES.length + 1;

/** Restores the mock store to its seeded fixture state. Mainly for tests. */
export function resetMockStore(): void {
  notes = FIXTURE_NOTES.map((note) => ({ ...note }));
  nextId = FIXTURE_NOTES.length + 1;
}

/**
 * Minimal in-memory stand-in for the real SQL backend so the template runs
 * standalone. Only understands the notes example from this template;
 * anything else resolves to an empty result set.
 */
export async function runMockQuery({ sql, params }: SqlQuery): Promise<unknown[]> {
  const normalized = sql.trim().toUpperCase();

  if (normalized.startsWith("INSERT INTO NOTES")) {
    const [title, body] = params as [string, string];
    const note: Note = {
      id: nextId++,
      title,
      body,
      created_at: new Date().toISOString(),
    };
    notes.push(note);
    return [note];
  }

  if (normalized.startsWith("SELECT") && normalized.includes("NOTES")) {
    return notes.map((note) => ({ ...note }));
  }

  return [];
}
