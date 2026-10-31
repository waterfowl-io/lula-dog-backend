import { test, expect } from "@playwright/test";
import { NotesPage } from "../fixtures/notes-page";
import { FAKE_APP_URL } from "../fixtures/urls";

test("injection-like note content renders as plain text and never executes", async ({ page }) => {
  let dialogFired = false;
  page.on("dialog", (dialog) => {
    dialogFired = true;
    void dialog.dismiss();
  });

  const notes = new NotesPage(page);
  await notes.goto(FAKE_APP_URL);

  const scriptTitle = "<script>alert(1)</script>";
  const sqlBody = "'); DROP TABLE notes;--";
  await notes.addNote(scriptTitle, sqlBody);

  // Rendered as literal text (React escapes text content), not executed as HTML/SQL.
  await expect(notes.noteTitle(scriptTitle)).toBeVisible();
  await expect(page.getByText(sqlBody, { exact: true })).toBeVisible();
  expect(dialogFired).toBe(false);
  await expect(page.locator("script", { hasText: "alert(1)" })).toHaveCount(0);

  // The table still works: a normal note can still be added afterwards.
  await notes.addNote("Still works", "Table survived the injection attempt");
  await expect(notes.noteTitle("Still works")).toBeVisible();
  await expect(notes.noteItems).toHaveCount(4);
});
