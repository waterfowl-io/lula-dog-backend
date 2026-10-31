import { useState, type FormEvent } from "react";

interface NoteFormProps {
  onCreate: (title: string, body: string) => void | Promise<void>;
}

export default function NoteForm({ onCreate }: NoteFormProps) {
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!title.trim()) return;
    await onCreate(title, body);
    setTitle("");
    setBody("");
  }

  return (
    <form onSubmit={handleSubmit} className="mt-6 space-y-3">
      <input
        value={title}
        onChange={(event) => setTitle(event.target.value)}
        placeholder="Title"
        className="w-full rounded-md border border-slate-300 px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-slate-400"
      />
      <textarea
        value={body}
        onChange={(event) => setBody(event.target.value)}
        placeholder="Body"
        className="w-full rounded-md border border-slate-300 px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-slate-400"
      />
      <button
        type="submit"
        className="rounded-md bg-slate-900 px-4 py-2 text-sm font-medium text-white hover:bg-slate-700"
      >
        Add note
      </button>
    </form>
  );
}
