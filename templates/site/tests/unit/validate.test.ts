// Proves: scripts/validate.mjs accepts a well-formed template layout and
// rejects stray files outside allowed roots, malformed migration names,
// and dependencies missing from allowlist.json. Builds real directory
// trees under os.tmpdir() rather than mocking fs.
// Run: npm run test:unit -- tests/unit/validate.test.ts
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, symlinkSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { validate } from "../../scripts/validate.mjs";

let dir: string;

function write(relPath: string, content: string) {
  const full = join(dir, relPath);
  mkdirSync(join(full, ".."), { recursive: true });
  writeFileSync(full, content);
}

function scaffoldValidTemplate() {
  write("lula.yaml", "name: test-template\nversion: 1\n");
  write("index.html", "<!doctype html><html><body></body></html>\n");
  write(
    "package.json",
    JSON.stringify(
      {
        name: "test-template",
        dependencies: { react: "^19.3.1" },
        devDependencies: { typescript: "^7.0.2" },
      },
      null,
      2,
    ),
  );
  write(
    "allowlist.json",
    JSON.stringify(
      {
        dependencies: { react: ["^19.3.1"] },
        devDependencies: { typescript: ["^7.0.2"] },
      },
      null,
      2,
    ),
  );
  write("app/main.tsx", "export {}\n");
  write("app/App.tsx", "export default function App() { return null }\n");
  write("migrations/0001_init.sql", "CREATE TABLE notes (id INTEGER PRIMARY KEY);\n");
  write("functions/.gitkeep", "");
  mkdirSync(join(dir, "packages"), { recursive: true });
}

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), "lula-validate-"));
});

afterEach(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe("validate", () => {
  it("passes for a well-formed template layout", () => {
    scaffoldValidTemplate();
    const { ok, errors } = validate(dir);
    expect(errors).toEqual([]);
    expect(ok).toBe(true);
  });

  it("fails when a stray file exists outside the allowed roots", () => {
    scaffoldValidTemplate();
    write("oops.txt", "not allowed here\n");
    const { ok, errors } = validate(dir);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes("oops.txt"))).toBe(true);
  });

  it("fails when a migration filename doesn't match the required pattern", () => {
    scaffoldValidTemplate();
    write("migrations/init.sql", "CREATE TABLE widgets (id INTEGER PRIMARY KEY);\n");
    const { ok, errors } = validate(dir);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes("init.sql"))).toBe(true);
  });

  it("fails when migrations are not contiguous starting at 0001", () => {
    scaffoldValidTemplate();
    write("migrations/0003_extra.sql", "CREATE TABLE widgets (id INTEGER PRIMARY KEY);\n");
    const { ok, errors } = validate(dir);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes("contiguous"))).toBe(true);
  });

  it("fails when a dependency is missing from allowlist.json", () => {
    scaffoldValidTemplate();
    write(
      "package.json",
      JSON.stringify(
        {
          name: "test-template",
          dependencies: { react: "^19.3.1", leftpad: "^1.0.0" },
          devDependencies: { typescript: "^7.0.2" },
        },
        null,
        2,
      ),
    );
    const { ok, errors } = validate(dir);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes("leftpad"))).toBe(true);
  });

  it("fails when a dependency version isn't within the allowlisted range", () => {
    scaffoldValidTemplate();
    write(
      "package.json",
      JSON.stringify(
        {
          name: "test-template",
          dependencies: { react: "^18.0.0" },
          devDependencies: { typescript: "^7.0.2" },
        },
        null,
        2,
      ),
    );
    const { ok, errors } = validate(dir);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes("react"))).toBe(true);
  });

  it("fails when functions/ contains files other than .gitkeep", () => {
    scaffoldValidTemplate();
    write("functions/handler.ts", "export {}\n");
    const { ok, errors } = validate(dir);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes("functions/"))).toBe(true);
  });

  it("fails when lula.yaml has no name", () => {
    scaffoldValidTemplate();
    write("lula.yaml", "version: 1\n");
    const { ok, errors } = validate(dir);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes("lula.yaml"))).toBe(true);
  });

  it("fails when a symlink is present anywhere in the tree", () => {
    scaffoldValidTemplate();
    symlinkSync(join(dir, "lula.yaml"), join(dir, "app", "link.tsx"));
    const { ok, errors } = validate(dir);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes("symlink"))).toBe(true);
  });

  it("fails when a required path is missing", () => {
    scaffoldValidTemplate();
    rmSync(join(dir, "app", "App.tsx"));
    const { ok, errors } = validate(dir);
    expect(ok).toBe(false);
    expect(errors.some((e) => e.includes("app/App.tsx"))).toBe(true);
  });
});
