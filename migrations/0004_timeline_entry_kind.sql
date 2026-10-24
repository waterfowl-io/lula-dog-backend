-- N4: what kind of contact a timeline entry records ("email", "call",
-- "quote"), so the deal timeline can show an icon/label and filter by it.
-- Null for entries the extractor couldn't classify (shown as "Note").
-- Not covered by the append-only triggers' WHEN clause (unlike raw_text/
-- summary/occurred_at), so it can be written the same way summary/
-- occurred_at are: once, via `timeline::enrich`.
ALTER TABLE timeline_entries ADD COLUMN kind TEXT;
