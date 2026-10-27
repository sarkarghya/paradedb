# ParadeDB HA

A fork of [ParadeDB](https://github.com/paradedb/paradedb) whose `pg_search` keeps
WAL-logged BM25 indexes working across PostgreSQL physical replication and Patroni
failover. Stock Community stops standby recovery at `pg_search/INIT_INDEX`; this build
replays index WAL so the promoted primary can search immediately, with no reindex.

It is experimental and not ParadeDB Enterprise. **Search on a standby is still
unsupported**; send search traffic to the primary only.

## Layout

The `paradedb-ha` branch is an upstream release tag plus a small commit stack. Everything
the fork adds lives in fork-owned files, so upstream merges rarely conflict:

| Path | Purpose |
| --- | --- |
| `pg_search/` (6 files) | WAL replay, recovery-aware metadata, standby planner guards, the `experimental-physical-failover` Cargo feature and the read-only `paradedb.physical_failover` setting |
| `tests/tests/replication.rs` | Physical replication and promotion regression (`PG_SEARCH_TEST_PHYSICAL_FAILOVER=1`) |
| `tests/physical_failover/` | Patroni elections and rejoining smoke test |
| `docker/Dockerfile.paradedb-ha` | Build, test and export the extension |
| `.github/workflows/paradedb-ha.yml` | CI for AMD64 and ARM64; publishes releases from tags |
| `fork/release.json` | Pinned upstream tag/commit and fork revision |
| `fork/sync-upstream.sh` | Merges a new upstream release |

To see the whole fork diff: `git diff "$(jq -r .upstreamCommit fork/release.json)" -- pg_search tests`.

Upstream SQL, the control file and `Cargo.lock` must stay identical to the pinned upstream
release (CI enforces this), so the extension version stays `0.26.3`; the fork revision
only identifies our binary.

## Updating to a new upstream release

```sh
fork/sync-upstream.sh          # pinned vs latest upstream release
fork/sync-upstream.sh v0.26.4  # creates sync/v0.26.4 and merges the tag
```

Resolve any conflicts (review upstream storage, WAL and planner changes carefully), then
push `sync/v0.26.4`. CI builds both architectures and runs the replication regression and
Patroni smoke test. When it passes, merge the branch into `paradedb-ha`.

Never merge upstream `main`; only stable release tags.

## Releasing

Tag the tested commit and push the tag. The tag must match `fork/release.json`:

```sh
git tag paradedb-ha-0.26.4-r1 && git push origin paradedb-ha-0.26.4-r1
```

CI rebuilds and retests, then publishes a prerelease with
`pg_search-pg18-trixie-{amd64,arm64}.tar.gz` and `SHA256SUMS`. Each tarball holds the
`usr/` tree, `BUILD-INFO`, `LICENSE` and `SOURCE.tar.gz`. For a new binary on the same
upstream version, bump `revision` in `fork/release.json`. Never replace published assets.

Fork tags never start with `v`, so upstream's inherited release workflows (Debian, PGXN,
Docs and others), which run on `v*` tags, are never triggered. Keep them disabled in the
Actions settings anyway.

## Using a release

Every cluster member must run the same build with `pg_search` in
`shared_preload_libraries` before any logged BM25 index exists. `SHOW
paradedb.physical_failover` returns `on` only on this build. Never roll back to stock
Community while logged BM25 indexes exist.

```dockerfile
ARG TARGETARCH
RUN curl -fsSL -o /tmp/pg_search.tar.gz \
      "https://github.com/sarkarghya/paradedb-ha/releases/download/paradedb-ha-0.26.3-r1/pg_search-pg18-trixie-${TARGETARCH}.tar.gz" \
    && echo "<pinned sha256>  /tmp/pg_search.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/pg_search.tar.gz -C / usr
```

The extension depends on `postgresql-18-pgvector` and `libopenblas0-pthread`.

## Running tests locally

```sh
cargo pgrx install --package pg_search --release \
  --features experimental-physical-failover --pg-config "$PG_CONFIG"
PG_SEARCH_TEST_PHYSICAL_FAILOVER=1 cargo test --locked --package tests \
  --test replication test_wal_streaming_replication_with_pg_search -- --exact --nocapture

docker build -f docker/Dockerfile.paradedb-ha --target patroni-test -t paradedb-ha:local .
PARADEDB_HA_IMAGE=paradedb-ha:local python3 tests/physical_failover/smoke.py
```

## License

ParadeDB HA retains ParadeDB's [AGPL-3.0 license](../LICENSE). It is not an official
ParadeDB release.
