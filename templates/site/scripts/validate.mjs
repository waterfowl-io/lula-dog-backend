#!/usr/bin/env node
// Validates that a Lula site template directory follows the required
// layout and dependency rules. No dependencies beyond Node's stdlib.
import { readdirSync, readFileSync, lstatSync, existsSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const IGNORED_TOP_LEVEL = new Set([
  "node_modules",
  "dist",
  ".git",
  "coverage",
  "test-results",
  "playwright-report",
]);

const ALLOWED_TOP_LEVEL_DIRS = new Set([
  "app",
  "migrations",
  "seed",
  "functions",
  "packages",
  "scripts",
  "tests",
  "dev",
]);

const ALLOWED_TOP_LEVEL_FILES = new Set([
  "lula.yaml",
  "index.html",
  "package.json",
  "package-lock.json",
  "allowlist.json",
  ".gitignore",
  "README.md",
  "vite.config.ts",
  "vitest.config.ts",
  "playwright.config.ts",
]);

const REQUIRED_PATHS = [
  "lula.yaml",
  "index.html",
  "package.json",
  "app",
  "app/main.tsx",
  "app/App.tsx",
  "migrations",
  "functions",
  "packages",
];

const MIGRATION_NAME_RE = /^(\d{4})_[a-z0-9_]+\.sql$/;

function isTsconfigName(name) {
  return name === "tsconfig.json" || /^tsconfig\..+\.json$/.test(name);
}

function isAllowedTopLevelEntry(name) {
  return (
    ALLOWED_TOP_LEVEL_DIRS.has(name) ||
    ALLOWED_TOP_LEVEL_FILES.has(name) ||
    isTsconfigName(name)
  );
}

function walk(dir, root, errors) {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    const rel = relative(root, full);
    const stat = lstatSync(full);

    if (stat.isSymbolicLink()) {
      errors.push(`symlinks are not allowed: ${rel}`);
      continue;
    }

    if (stat.isDirectory()) {
      walk(full, root, errors);
    }
  }
}

function checkRequiredPaths(root, errors) {
  for (const rel of REQUIRED_PATHS) {
    if (!existsSync(join(root, rel))) {
      errors.push(`missing required path: ${rel}`);
    }
  }
}

function checkLulaYaml(root, errors) {
  const path = join(root, "lula.yaml");
  if (!existsSync(path)) return;
  const content = readFileSync(path, "utf8");
  const match = content.match(/^name:\s*["']?(.+?)["']?\s*$/m);
  if (!match || match[1].trim() === "") {
    errors.push("lula.yaml must declare a non-empty top-level `name`");
  }
}

function checkTopLevelRoots(root, errors) {
  for (const name of readdirSync(root)) {
    if (IGNORED_TOP_LEVEL.has(name)) continue;
    if (!isAllowedTopLevelEntry(name)) {
      errors.push(`stray file/dir outside allowed roots: ${name}`);
    }
  }
}

function checkSymlinks(root, errors) {
  for (const name of readdirSync(root)) {
    if (IGNORED_TOP_LEVEL.has(name)) continue;
    const full = join(root, name);
    const stat = lstatSync(full);
    if (stat.isSymbolicLink()) {
      errors.push(`symlinks are not allowed: ${name}`);
      continue;
    }
    if (stat.isDirectory()) {
      walk(full, root, errors);
    }
  }
}

function checkMigrations(root, errors) {
  const dir = join(root, "migrations");
  if (!existsSync(dir)) return;

  const entries = readdirSync(dir, { withFileTypes: true });
  const versions = [];

  for (const entry of entries) {
    if (!entry.isFile()) {
      errors.push(`migrations/ must contain only files: ${entry.name}`);
      continue;
    }
    const match = entry.name.match(MIGRATION_NAME_RE);
    if (!match) {
      errors.push(
        `migration filename must match ^\\d{4}_[a-z0-9_]+\\.sql$: ${entry.name}`,
      );
      continue;
    }
    versions.push(parseInt(match[1], 10));
  }

  versions.sort((a, b) => a - b);
  for (let i = 0; i < versions.length; i++) {
    const expected = i + 1;
    if (versions[i] !== expected) {
      errors.push(
        `migrations must be contiguous starting at 0001 (expected ${String(expected).padStart(4, "0")}, found gap/duplicate at ${String(versions[i]).padStart(4, "0")})`,
      );
      break;
    }
  }
}

function checkFunctionsDir(root, errors) {
  const dir = join(root, "functions");
  if (!existsSync(dir)) return;
  const entries = readdirSync(dir);
  const unexpected = entries.filter((name) => name !== ".gitkeep");
  if (unexpected.length > 0) {
    errors.push(`functions/ must contain only .gitkeep, found: ${unexpected.join(", ")}`);
  }
}

function collectExternalDeps(pkg, { skipPrefix } = {}) {
  const deps = {};
  for (const field of ["dependencies", "devDependencies"]) {
    for (const [name, version] of Object.entries(pkg[field] ?? {})) {
      if (skipPrefix && name.startsWith(skipPrefix)) continue;
      deps[name] = { version, field };
    }
  }
  return deps;
}

function checkAllowlist(root, errors) {
  const allowlistPath = join(root, "allowlist.json");
  const pkgPath = join(root, "package.json");
  if (!existsSync(allowlistPath) || !existsSync(pkgPath)) return;

  const allowlist = JSON.parse(readFileSync(allowlistPath, "utf8"));
  const allowed = {
    ...(allowlist.dependencies ?? {}),
    ...(allowlist.devDependencies ?? {}),
  };

  function checkPackageJson(path, label, skipPrefix) {
    const pkg = JSON.parse(readFileSync(path, "utf8"));
    const deps = collectExternalDeps(pkg, { skipPrefix });
    for (const [name, { version, field }] of Object.entries(deps)) {
      const allowedVersions = allowed[name];
      if (!allowedVersions) {
        errors.push(`${label}: ${field} "${name}" is not in allowlist.json`);
        continue;
      }
      if (!allowedVersions.includes("*") && !allowedVersions.includes(version)) {
        errors.push(
          `${label}: ${field} "${name}@${version}" does not match an allowed version range in allowlist.json (${allowedVersions.join(", ")})`,
        );
      }
    }
  }

  checkPackageJson(pkgPath, "package.json", "@lula/");

  const packagesDir = join(root, "packages");
  if (existsSync(packagesDir)) {
    for (const name of readdirSync(packagesDir)) {
      const pkgJsonPath = join(packagesDir, name, "package.json");
      if (existsSync(pkgJsonPath)) {
        checkPackageJson(pkgJsonPath, `packages/${name}/package.json`, "@lula/");
      }
    }
  }
}

export function validate(root) {
  const errors = [];

  checkRequiredPaths(root, errors);
  checkLulaYaml(root, errors);
  checkTopLevelRoots(root, errors);
  checkSymlinks(root, errors);
  checkMigrations(root, errors);
  checkFunctionsDir(root, errors);
  checkAllowlist(root, errors);

  return { ok: errors.length === 0, errors };
}

function isMain() {
  return process.argv[1] === fileURLToPath(import.meta.url);
}

if (isMain()) {
  const root = process.cwd();
  const { ok, errors } = validate(root);

  if (ok) {
    console.log("validate: OK");
    process.exit(0);
  }

  console.error(`validate: FAILED (${errors.length} issue${errors.length === 1 ? "" : "s"})`);
  for (const error of errors) {
    console.error(`  - ${error}`);
  }
  process.exit(1);
}
