import { test, expect } from "@playwright/test";
import { NotesPage } from "../fixtures/notes-page";
import { FAKE_APP_URL } from "../fixtures/urls";

test("adding a note via NoteForm makes it appear in the list", async ({ page }) => {
  const notes = new NotesPage(page);
  await notes.goto(FAKE_APP_URL);

  await notes.addNote("Playwright note", "Added by an e2e test");

  await expect(notes.noteTitle("Playwright note")).toBeVisible();
  await expect(notes.page.getByText("Added by an e2e test")).toBeVisible();
});
