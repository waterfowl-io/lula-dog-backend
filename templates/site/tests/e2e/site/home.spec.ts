import { test, expect } from "@playwright/test";
import { NotesPage } from "../fixtures/notes-page";
import { FAKE_APP_URL } from "../fixtures/urls";

test("home page loads and lists the seeded notes", async ({ page }) => {
  const notes = new NotesPage(page);
  await notes.goto(FAKE_APP_URL);

  await expect(notes.noteItems).toHaveCount(2);
  await expect(notes.noteTitle("Welcome to Lula")).toBeVisible();
  await expect(notes.noteTitle("db.sql is parameterized")).toBeVisible();
});
