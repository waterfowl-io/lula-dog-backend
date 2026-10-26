-- Build-mode chat produces draft app-definition versions (ordinary,
-- unpublished `app_definition_versions` rows -- see `crate::chat::build`).
-- A draft the user discards can't be deleted (versions are an immutable,
-- append-only log that `rollback` already relies on), so it's marked
-- instead. `fetch_published_or_latest`'s "latest" fallback skips discarded
-- versions so a discarded draft never becomes what the app renders.

ALTER TABLE app_definition_versions ADD COLUMN discarded_at timestamptz;
