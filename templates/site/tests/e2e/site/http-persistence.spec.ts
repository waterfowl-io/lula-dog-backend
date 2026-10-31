import { test, expect } from "@playwright/test";
import { NotesPage } from "../fixtures/notes-page";
import { HTTP_APP_URL } from "../fixtures/urls";

test("adding a note over HTTP mode persists across a reload", async ({ page }) => {
  const notes = new NotesPage(page);
  await notes.goto(HTTP_APP_URL);

  const countBeforeAdd = await notes.noteItems.count();
  await notes.addNote("HTTP mode note", "Written through db.sql over HTTP");
  await expect(notes.noteTitle("HTTP mode note")).toBeVisible();

  await page.reload();
  await expect(page.getByRole("heading", { name: "Notes" })).toBeVisible();

  await expect(notes.noteTitle("HTTP mode note")).toBeVisible();
  await expect(notes.noteItems).toHaveCount(countBeforeAdd + 1);
});
