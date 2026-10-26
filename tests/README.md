# Test suite for `pg_search`

This directory contains the **integration tests** and **client property tests** for the `pg_search` extension. Because these tests run outside the Postgres process, they require the extension to already be installed.

For a complete overview of ParadeDB's testing infrastructure (including unit tests and pg regress tests), please see the [Testing section in `CONTRIBUTING.md`](../CONTRIBUTING.md#testing).

## SQL syntax in tests

Use `USING paradedb`, tokenizer casts for text/JSON configuration, and `===`, `###`, `|||`, or `&&&` for text searches. Non-text columns are columnar by default. Use `pdb.literal` or `pdb.literal_normalized` for columnar whole-value text; a word tokenizer with `columnar=true` preserves tokenized search when the same field also needs columnar access. `@@@` accepts explicit query builders such as `pdb.all()`, `pdb.more_like_this()`, and `pdb.parse()`.

Use SQL comparisons such as `rating = 4` and `in_stock = true` for non-text scalar filters. Keep at least one ParadeDB operator in the query; add `id @@@ pdb.all()` when there is no text search. Explicit term-query tests remain for alias operands, array membership, ltree hierarchy matching, and executor paths that disable filter pushdown or index scans.

The `deprecated_*` compatibility tests deliberately retain old syntax where current APIs cannot preserve the tested behavior:

- JSON tokenizer casts change numeric JSON filter and term matching. The legacy JSON pushdown and aggregate cases retain their schema options.
- Custom stopword lists have no working tokenizer cast equivalent.

Dedicated compatibility tests also cover the old access-method name, ignored key/datetime options, and rejected legacy operator arguments. These are not examples for new queries. Parser-specific tests use explicit parser functions; this includes phrases with stopwords, whose position gaps are currently lost by `###`. JSON query serialization tests exercise typed query objects rather than query-string syntax.

## Client Property Tests

Client property tests are a particularly interesting subcategory of integration tests. Most live in [`qgen.rs`](tests/qgen.rs), but other files also use `crate::fixtures::querygen` to generate tests.

## Environment Variables

The tests require a `DATABASE_URL` environment variable to be set. The easiest way to do this is to create a `.env` file with the following contents:

```env
DATABASE_URL=postgres://USER_NAME@localhost:PORT/pg_search
```

`USER_NAME` should be replaced with your system username (for example, the output of `whoami`).

`PORT` should be replaced with 28800 plus your PostgreSQL major version (for example, 28818 for PostgreSQL 18).

Some tests also require `PG_CONFIG` to point to the `pg_config` binary for the PostgreSQL installation under test. The logical-replication test requires it, while the dump/restore test is skipped when it is not set.

## Running Tests with pgrx-managed PostgreSQL

If you are using pgrx’s bundled PostgreSQL, follow these steps from the root of the repository:

```shell
#!/bin/bash

set -x
export DATABASE_URL="postgresql://$(whoami)@localhost:28818/pg_search"
export PG_CONFIG="$HOME/.pgrx/18.6/pgrx-install/bin/pg_config"
export RUST_BACKTRACE=1
cargo pgrx stop --package pg_search
./scripts/dev-install.sh --package pg_search --pg-config "$PG_CONFIG"
cargo pgrx start --package pg_search
createdb -h localhost -p 28818 pg_search || true

cargo test --package tests
```

## Running Tests with a Self-Hosted PostgreSQL

If you are using a self-hosted PostgreSQL installation, make sure your PostgreSQL server is already running, create a `pg_search` database on it,
and install the `pg_search` extension files into that PostgreSQL instance instead of pgrx's bundled Postgres.
The example below uses Homebrew's PostgreSQL 18 path; replace `PG_CONFIG` with the path to your installation's `pg_config` binary.

```shell
#!/bin/bash

set -x
export DATABASE_URL="postgresql://$(whoami)@localhost:5432/pg_search"
export PG_CONFIG=/opt/homebrew/opt/postgresql@18/bin/pg_config
export RUST_BACKTRACE=1

createdb pg_search || true
./scripts/dev-install.sh --package pg_search --pg-config "$PG_CONFIG"

cargo test --package tests
```

To run a single test, use the following command (replace `<testname>` with the test file name without the `.rs` extension):

```shell
cargo test --package tests --test <testname>
```

## Experimental physical failover (fork only)

The `experimental-physical-failover` extension feature lets PostgreSQL replay
ParadeDB index WAL without the Community `INIT_INDEX` shutdown. Search reads on
the standby remain blocked until promotion. This is **not** Enterprise physical
replication, hot-standby search support, or a production safety guarantee.

Build and install the matching library before opting the regression into this mode:

```shell
cargo pgrx install --package pg_search --release \
  --features experimental-physical-failover --pg-config "$PG_CONFIG"
PG_SEARCH_TEST_PHYSICAL_FAILOVER=1 cargo test --locked --package tests \
  --test replication test_wal_streaming_replication_with_pg_search -- --exact --nocapture
```

Without that environment variable the existing Community shutdown test is unchanged.
The opt-in test checks ordinary-table streaming after index creation, targeted
standby search errors, mutable/immutable writes, rollback, COPY, VACUUM, a separate
concurrent index rebuild, standby restart, in-flight transactions at primary loss,
promotion without rebuilding the copied index, and writes and verification after
promotion. LSN fences ensure index WAL, not only heap rows, has replayed.

### Linux build and artifact export

The PG18 Dockerfile compiles from source and runs that regression before exporting
the library, control file, and SQL scripts together:

```shell
docker build --file docker/Dockerfile.experimental-ha --target runtime \
  --tag paradedb-physical-failover:experimental .
docker build --file docker/Dockerfile.experimental-ha --target artifacts \
  --output type=local,dest=./artifacts/physical-failover .
```

Build natively on the target architecture, or select `--platform linux/amd64` or
`--platform linux/arm64` explicitly. Do not install an ARM library on an x86 server
or a PG18 library on another PostgreSQL major version. Runtime dependencies and
distribution ABI must match; copying the `.so` alone is not sufficient.

The manual **Experimental physical failover artifacts** GitHub Actions workflow
builds and tests natively on both architectures, then uploads matching extension
files with the source revision and SHA-256 checksums. It does not publish releases
or deploy a database. It also runs on pushes to the dedicated
`fix/experimental-physical-index-ha` branch; it does not run on ordinary main pushes.

### Patroni constraints

- Install the same tested extension build and SQL files on **every** member before
  replaying index WAL. Keep `pg_search` in `shared_preload_libraries` on all nodes;
  resource manager 137 is still needed for existing WAL and backups.
- Route search traffic exclusively to Patroni's current primary (the `/primary`
  health check), never to a read-replica pool. Check `pg_is_in_recovery() = false`
  before enabling search on a promoted node. Ordinary standby reads remain native
  PostgreSQL reads; queries that open ParadeDB metadata can still be rejected.
- Keep `wal_level = replica` or `logical`. Use physical replication slots/WAL
  retention appropriate to the existing cluster. Never copy live relation files
  or Tantivy caches between nodes with rsync.
- This patch does not change fencing, leader election, replication lag limits,
  `pg_rewind`, or Patroni configuration. Asynchronous replication can still lose
  acknowledged transactions at failover; configure and validate Patroni's
  synchronous mode separately if zero data loss is required.
- Validate a disposable cluster with production-equivalent PostgreSQL version,
  architecture, workload, backups, restores, cascading replication, failback,
  concurrent maintenance, and crash timing before considering a rollout. The
  regression uses `pg_ctl promote`, **not** a full Patroni/etcd election test.
- Do not downgrade a replica to the ordinary Community library while it may
  replay `INIT_INDEX`: the original shutdown behavior would return. Retain the
  matching extension artifacts with backups. Do not mix this fork with Enterprise
  libraries or assume their custom WAL records are compatible.

Concurrent standby search remains disabled because buffer pins are local to a
server and the Community WAL path does not supply index-specific recovery
conflict handling for segment recycling. Removing that read guard is unsafe.
