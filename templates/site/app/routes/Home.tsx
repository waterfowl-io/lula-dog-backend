import { db, useLiveQuery } from "@lula/sdk";
import NoteForm from "../components/NoteForm";

interface Note {
  id: number;
  title: string;
  body: string;
  created_at: string;
}

async function fetchNotes(): Promise<Note[]> {
  return db.sql<Note>`SELECT * FROM notes ORDER BY id DESC`;
}

export default function Home() {
  const { rows: notes, loading, error, refetch } = useLiveQuery(fetchNotes);

  async function createNote(title: string, body: string) {
    await db.sql`INSERT INTO notes (title, body) VALUES (${title}, ${body})`;
    await refetch();
  }

  return (
    <main className="mx-auto max-w-xl px-4 py-12">
      <h1 className="text-2xl font-semibold text-slate-900">Notes</h1>
      <p className="mt-1 text-sm text-slate-500">
        A minimal example of db.sql against the notes table from migrations/0001_init.sql.
      </p>

      {error && <p className="mt-4 text-sm text-red-600">{error.message}</p>}
      {loading ? (
        <p className="mt-6 text-sm text-slate-500">Loading...</p>
      ) : (
        <ul className="mt-6 space-y-3">
          {notes.map((note) => (
            <li key={note.id} className="rounded-md border border-slate-200 p-3">
              <h2 className="text-sm font-medium text-slate-900">{note.title}</h2>
              <p className="mt-1 text-sm text-slate-600">{note.body}</p>
            </li>
          ))}
        </ul>
      )}

      <NoteForm onCreate={createNote} />
    </main>
  );
}
