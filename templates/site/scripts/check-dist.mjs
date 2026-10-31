#!/usr/bin/env node
// Asserts that every build output file except dist/index.html has a
// content hash in its filename, so assets can be cached forever.
import { readdirSync, lstatSync } from "node:fs";
import { join, relative } from "node:path";

// Vite content hashes are base64url (letters, digits, "_", "-").
const HASHED_NAME_RE = /-[\w-]{6,}\.[a-z0-9.]+$/i;

function walk(dir, root, files) {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    const stat = lstatSync(full);
    if (stat.isDirectory()) {
      walk(full, root, files);
    } else {
      files.push(relative(root, full));
    }
  }
}

const distDir = join(process.cwd(), "dist");
const files = [];
walk(distDir, distDir, files);

const errors = [];
for (const file of files) {
  if (file === "index.html") continue;
  if (!HASHED_NAME_RE.test(file)) {
    errors.push(file);
  }
}

if (errors.length > 0) {
  console.error("check-dist: FAILED - unhashed files in dist/:");
  for (const file of errors) {
    console.error(`  - ${file}`);
  }
  process.exit(1);
}

console.log(`check-dist: OK (${files.length} files, all hashed except index.html)`);
