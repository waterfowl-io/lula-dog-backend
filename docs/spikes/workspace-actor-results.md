# Workspace actor spike — results

Branch `spike/workspace-actor` (worktree `/workspace/lula-actor/backend-spike`). Captured 2026-10-28 ~10:00–10:05 CDT.
Raw output: `bench-raw.txt` (full `actor_bench all` run, with JSON + markdown sections), `bench-raw.json`, and
`bench-raw-latency-rep2.txt` / `-rep3.txt` (two more `latency` runs to show the variance). Machine: `machine.txt`.

## Machine
- 8 vCPU Intel Xeon (KVM guest, 1 thread per core), 15 GB RAM, no swap
- ext4 on a virtio disk (`/dev/vdb`, 128 GB). Bench DBs were written under `/workspace/lula-actor/tmp` on that disk, not tmpfs
- Linux 6.12, rustc/cargo 1.99.1, release build
- SQLite comes bundled via libsqlite3-sys 0.30.1, through sqlx. WAL mode, `synchronous=NORMAL`, busy_timeout 5s, pool max 8 (the app's real `db::options()`)
- Litestream 0.5.17
- Shared dev box: other agents' processes may add noise. Treat numbers as ±15%

## What was measured
- **Real actions** through `actions::dispatch_in`, in a round robin of five: `opportunity.create`, `opportunity.update`, `opportunity.update_stage`, `task.create`, `lead.accept`.
- **Seed:** 200 opportunities and 2000 open leads.
- **Run size:** N=5000 measured per case, after 200 warmup commands.
- **direct:** today's path. A pooled connection, `pool.begin()` (DEFERRED), the action, then COMMIT.
- **actor:** one task owns the only write connection and uses a bounded mailbox (256). Each command runs as `BEGIN IMMEDIATE`, then a `_lula_commands` dedupe lookup, then the action, then the command record with its reply and undo, then an `_lula_outbox` row, then COMMIT. The actor path does **more** writes per command than direct.
- **actor_group_commit:** the same, but up to 32 queued commands share one transaction (one SAVEPOINT each).

## Latency and throughput (µs; three runs: main / rep2 / rep3)

| path | conc. | p50 | p95 | p99 | ok cmd/s | errors |
|---|---|---|---|---|---|---|
| direct | 1 | 174 / 178 / 188 | 384 / 417 / 405 | 789 / 776 / 749 | 4325 / 4191 / 4018 | 0 |
| direct | 50 | 7851 / 7557 / 7190 | 14425 / 11904 / 12397 | 40369 / 40992 / 40533 | 4614 / 4861 / 4864 | **448 / 436 / 449 SQLITE_BUSY (~9%)** |
| actor | 1 | 175 / 186 / 158 | 276 / 302 / 246 | 934 / 834 / 1184 | 4030 / 3740 / 4281 | 0 |
| actor | 50 | 13947 / 10681 / 9268 | 22378 / 17477 / 18639 | 23761 / 23218 / 20878 | 3410 / 3895 / 4177 | 0 |
| actor+group | 1 | 238 / 204 / 195 | 418 / 302 / 302 | 1399 / 932 / 1429 | 3141 / 3539 / 3668 | 0 |
| actor+group | 50 | 12364 / 9027 / 8856 | 20008 / 17536 / 16202 | 22772 / 20059 / 18552 | 3804 / 4909 / 4973 | 0 |

Max latency at 50 concurrent callers was 185–339 ms on direct, against 21–38 ms on the actor paths.

**Mixed load** (50 writers + 50 readers on a separate read pool, 3 runs):

| path | write p50 | write p99 | write errors (main run) | read p50 | read p99 |
|---|---|---|---|---|---|
| direct | 36.5–49.5 ms | 164–226 ms | 874 SQLITE_BUSY | 2.7–3.9 ms | 3.8–5.3 ms |
| actor | 66–70 ms | 106–126 ms | 0 | 2.7–3.0 ms | 4.1–4.8 ms |
| actor+group | 44–65 ms | 56–94 ms | 0 | 2.5–4.2 ms | 3.5–5.4 ms |

**Reading it:**
- **One caller:** the actor matches direct (p50 ≈ 160–190 µs either way). The mailbox hop costs nothing measurable next to the fsync-free WAL commit, even though the actor writes two extra rows per command.
- **50 concurrent callers:** the direct path loses about 9% of commands to `SQLITE_BUSY`. That happens because DEFERRED transactions that upgrade to a write lock return BUSY right away. SQLite skips the busy handler there to avoid deadlock, so busy_timeout doesn't help. Direct's p99 is also about 2× worse (40 ms against 19–24 ms).
- **The actor at 50 callers** lands 0 errors and has a tight tail (p99 ≈ p95 + 3 ms), because everything is FIFO queueing. Its p50 is higher than direct's (9–14 ms against 7–8 ms) because every caller waits in line instead of failing fast.
- **Throughput is capped at roughly 3.4–5k commands/s per workspace** on both paths. That is about 200 µs of SQLite work per command. Group commit adds about 10–25% at 50 callers and gave the best mixed-load write tail. At one caller it costs a little.
- **Readers aren't hurt by the actor**, because WAL lets readers run while the actor writes.

Fair-comparison caveat: the direct baseline uses DEFERRED transactions, which is what the app does today. A direct path using `BEGIN IMMEDIATE` would turn the BUSY errors into busy_timeout waits. It still would not give single-writer FIFO ordering, idempotency, or the outbox.

## Runner batches vs. interactive clicks
The runner sends 10 batches of `task.create` through `send_batch`, one transaction per batch. Ten interactive callers run at the same time, each doing 500 commands after 50 warmup. Errors were 0 everywhere.

| batch size | batch tx p50 / p95 | interactive p50, no batch → with batch | interactive p99, no batch → with batch |
|---|---|---|---|
| 100 | 4.4 / 5.7 ms | 1.98 → 1.67 ms | 11.8 → 9.8 ms |
| 500 | 22.2 / 24.0 ms | 1.82 → 1.80 ms | 11.1 → 11.5 ms |

With the biased interactive lane, batches add no measurable click latency. The worst case is one in-flight batch transaction (about 22 ms at 500 rows), so batches should be capped at roughly 500 commands or 25 ms.

An earlier run had a bench bug: the batch DB had 0 seeded leads, so every `lead.accept` failed. I fixed it and re-ran, and the numbers above come from the fixed run.

## Cold wake (50 reps each)
| case | spawn (open + migrate + pragmas) p50 / p95 | first command after wake p50 / p95 |
|---|---|---|
| new DB (creates the file and runs all migrations) | 5.2 / 8.6 ms | 0.63 / 1.43 ms |
| existing seeded DB | 9.1 / 10.9 ms | 1.06 / 2.72 ms |

A cold wake costs about 10 ms. That is small enough to wake on demand inside a request.

## Crash recovery (`kill -9` on a child process, 20 reps per scenario, all on real files)
| scenario | pass | recovery (actor re-spawn) p50 / p95 |
|---|---|---|
| single: kill inside the `lead.accept` tx, after the "IN_TX" hook and before COMMIT | 20/20 | 0.64 / 0.79 ms |
| mid_batch: kill inside a 500-command batch tx | 20/20 | 0.61 / 1.04 ms |
| between_batches: kill after batch k commits, during batch k+1 | 20/20 | 0.71 / 1.60 ms |

After each kill, the bench checked four things:
- `PRAGMA integrity_check` returns ok;
- no `_lula_commands` row exists for the killed command;
- no partial rows are left: the lead is still `new`, the batch k+1 tasks are absent, and the batch k tasks are all present;
- retrying the same command id applies it exactly once and the second send returns `deduped=true`. The count checks found no double opportunity.

Limitation: recovery time is measured after an integrity-check connection already replayed the WAL, so it is re-spawn time, not WAL replay time. The WALs here are a few MB, so replay is also in the millisecond range.

## Litestream (optional)
- Setup: a `file://` replica on local disk, a seeded DB of 5.99 MB, `litestream replicate`, writes, then a replica-sync wait.
- Then `litestream restore` to a fresh path, with an integrity check and row-count match: 5/5 OK.
- Restore p50 / p95 was 78 / 80 ms.
- It did **not** test S3 latency, time to the first LTX upload, or the RPO under load. The design doc's figure of about 1 s RPO is Litestream's default `sync-interval` and was not measured here.

## Bugs found in review and fixed in the actor during the spike
1. A failed `COMMIT` (for example disk full) left the connection inside the transaction, and every later `BEGIN IMMEDIATE` would have failed. The fix is to ROLLBACK on COMMIT error, for single commands and group commit alike.
2. On idle exit, messages that arrived between the idle timer firing and loop exit were dropped, and callers saw a spurious 503. The fix closes both receivers, drains the queued messages with `try_recv` and processes them, then exits.
3. A handle could go stale, with the actor already idled out. The fix is a new `ActorRegistry::send` that wakes and retries once on `ServiceUnavailable`, which is safe because of the command-id dedupe.

## Not covered (by design: local only)
- Postgres leases, fencing, and forwarding between nodes; the spike is single-process.
- S3 Litestream, failover, and RTO.
- EBS or gp3 fsync characteristics. With `synchronous=NORMAL` and WAL, commits don't fsync, so a power-loss durability window exists. A process crash is safe, as shown above.
- The HTTP, chat, and build-mode callers still use the direct path; only the bench routes through the actor.

## Checks
`cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features --locked -D warnings`, and `cargo test --locked` all pass. The tests include `tests/actor.rs`.

## Addendum (2026-10-28 ~10:40 CDT): key partitions — `actor_bench partitions`
Raw output: `bench-raw-partitions.txt`. Uses the DuckDB 2.0.0 CLI with `sqlite_scanner`; the extension was installed from a local file because the box blocks plain-HTTP extension downloads. Code: `src/actor/partition.rs` (`PartitionKey`, validated paths, `fanout_attach_sql`, 3 unit tests) plus `examples/actor_bench/partitions.rs`.

**Correctness: PASS.** The setup was workspace A with shared + `user/u1` + `user/u2`, and workspace B with `user/u1` (the same key as in A).
- Isolation: each file holds only its own rows, and A/u1 and B/u1 are different files.
- Atomic within a partition: a 3-command batch whose third command fails applied nothing (u1 stayed at 5 rows).
- The fan-out builder refuses a partition from workspace B (`CrossWorkspace`), and the key `../../other` is rejected.
- The DuckDB fan-out over A's 3 partitions returns exactly `{shared: 2 rows / 200, u1: 5 / 5000, u2: 7 / 70000}`, with no B rows.
- An `INSERT` through the `READ_ONLY` attach fails, and the row count is unchanged.
- Separately verified: after `SET enable_external_access=false; SET lock_configuration=true`, a further `ATTACH` or `read_csv('/etc/passwd')` fails, and the setting can't be re-enabled.

**Scale.** Each case has shared + N user partitions, 200 deals each, with live actors (WAL readers). The DuckDB fan-out is a `UNION ALL` count/sum, 5 reps per case.

| user partitions | files | wake p50 / p95 | fan-out p50 / p95 | net of the 65 ms CLI start + LOAD | db+wal bytes |
|---|---|---|---|---|---|
| 2 | 3 | 5.4 / 5.4 ms | 79 / 81 ms | ~14 ms | 1.4 MB |
| 50 | 51 | 6.6 / 11.2 ms | 162 / 166 ms | ~96 ms | 24.6 MB |
| 200 | 201 | 6.0 / 9.4 ms | 400 / 401 ms | ~335 ms | 96.9 MB |

Fan-out costs about 1.7 ms per attached partition, and the results were correct in every rep. Not covered: in-process DuckDB with cached attachments, Litestream across many files, re-partition migrations, and transfer sagas.
