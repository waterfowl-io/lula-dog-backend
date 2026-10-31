// Runnable entry point (via `npm run e2e:actor`) that boots the same
// sql.js-backed HTTP actor used by tests/integration/sql-http.test.ts, but
// on a fixed port so Playwright's HTTP-mode webServer can be built against
// a known VITE_LULA_API_URL ahead of time. Not used by the app itself.
import { fileURLToPath } from "node:url";
import { startSqliteActor } from "./sqlite-actor";

const migrationsDir = fileURLToPath(new URL("../../migrations", import.meta.url));
const port = Number(process.env.PORT ?? 4190);

const actor = await startSqliteActor({
  migrationsDir,
  port,
  corsOrigins: ["http://127.0.0.1:4174", "http://localhost:4174"],
});
console.log(`sqlite actor listening on ${actor.url}`);
