-- M6: the evidence quote an extractor based a proposal on, when it found
-- one ("Could you also quote SKU 3 in 32 ECT?"). Null for direct edits and
-- for fields an extractor set with no quotable source line.
ALTER TABLE field_changes ADD COLUMN evidence TEXT;

-- S6: a one-line product description for the kanban card subtitle (e.g.
-- "Kraft mailers 6x9, 5k"). Null until set; there is no proposal/audit
-- trail for it, it is just descriptive.
ALTER TABLE opportunities ADD COLUMN product TEXT;
