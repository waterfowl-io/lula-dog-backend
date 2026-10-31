import { expect, type Locator, type Page } from "@playwright/test";

/** Page object for the Home route (app/routes/Home.tsx): the notes list + NoteForm. */
export class NotesPage {
  readonly page: Page;
  readonly titleInput: Locator;
  readonly bodyInput: Locator;
  readonly submitButton: Locator;
  readonly noteItems: Locator;

  constructor(page: Page) {
    this.page = page;
    this.titleInput = page.getByPlaceholder("Title");
    this.bodyInput = page.getByPlaceholder("Body");
    this.submitButton = page.getByRole("button", { name: "Add note" });
    this.noteItems = page.locator("main ul li");
  }

  async goto(baseUrl: string): Promise<void> {
    await this.page.goto(baseUrl);
    await expect(this.page.getByRole("heading", { name: "Notes" })).toBeVisible();
  }

  async addNote(title: string, body: string): Promise<void> {
    const countBefore = await this.noteItems.count();
    await this.titleInput.fill(title);
    await this.bodyInput.fill(body);
    await this.submitButton.click();
    await expect(this.noteItems).toHaveCount(countBefore + 1);
  }

  noteTitle(title: string): Locator {
    return this.noteItems.filter({ has: this.page.getByRole("heading", { name: title, exact: true }) });
  }
}
