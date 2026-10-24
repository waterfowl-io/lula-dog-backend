# Test fixtures

`bizapedia_fake_filings.csv` is **fake example data** for the Bizapedia CSV
importer tests: invented company names, invented people, invented addresses.
It is not a sample of Bizapedia's real output — the real column names are
unknown until Nolan gets Bizapedia Pro Data access (see docs/lead-gen.md) —
it is shaped to exercise the importer's default column mapping and the cheap
filter (one holding-company shell, one revoked entity, one excluded keyword,
two that should pass).
