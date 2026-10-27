# Pointsy ParadeDB physical-failover fork

This repository maintains a release-matched parallel fork of ParadeDB for Pointsy:
WAL-logged BM25 indexes on source tables, physical replication and search after
Patroni promotion. It is experimental; standby search is not supported.

The provenance and validation snapshot below was recorded on 2026-10-27. Read
`fork/release.json`, inspect Git status and verify current CI/registry state before
making changes. Nothing in this README authorizes production deployment.

## Purpose and non-negotiable constraints

Pointsy needs BM25 indexes directly on its authoritative PostgreSQL tables while
retaining Patroni physical failover. Stock Community 0.26.3 terminates standby WAL
recovery at `pg_search/INIT_INDEX` for logged BM25 indexes. The previous workaround
used unlogged search projection tables, triggers, a readiness row and a rebuild
poller; the requested replacement removes those copies and indexes source tables.

The user explicitly prohibited modifying the production database and required a
**separate database in production mode**. All application validation used synthetic
data in uniquely named local clusters. No live schema push, production failover,
production deployment or production volume replacement was authorized/performed.
Maintaining this fork or publishing binaries is not authorization for those actions.

This is experimental, not Enterprise or an upstream-supported HA implementation.
It allows index WAL replay and primary search after promotion. **Concurrent search
on a standby remains unsupported** because safe recovery-conflict handling has not
been implemented. Ordinary source-table reads/replication are distinct from search.
Do not simply remove the Community tripwire on a newer upstream release and assume
that replication, promotion or standby-query safety follows.

## Repositories, branches and exact provenance

| Item | Current value |
| --- | --- |
| Upstream | `https://github.com/paradedb/paradedb` |
| Maintained fork | `https://github.com/sarkarghya/paradedb` |
| Corrected release branch | `fix/experimental-physical-failover-v0.26.3` |
| Upstream release | `v0.26.3` |
| Exact upstream base | `b3421945b45fdd177272dc9a204c7d47a7cdb527` |
| Published fork prerelease | `v0.26.3-pointsy.1` |
| Published binary source | `3e9cafa32c9a73fb0e9bd05593268b6f1a4fb802` |
| Extension SQL version | `0.26.3` (not `0.26.3-pointsy.1`) |
| Build feature | `experimental-physical-failover` |
| Primary application target | PostgreSQL 18, Debian trixie, Linux AMD64 |
| Additional binary validation | Linux ARM64 in fork CI |

The branch advanced after the binary release for packaging and maintenance docs.
Never equate branch HEAD with the source of an already archived binary. Inspect
`BUILD-INFO`, the release target SHA and checksums. `fork/release.json` pins the upstream
base and fork revision; it does not assert that every branch commit was released.

Local checkouts at the time of validation:

- This corrected fork: `/Users/charlene/Desktop/PYPY/paradedb-ha-0.26.3`.
- Consumer application: `/Users/charlene/Desktop/PYPY/pointsy-storefront`.
- Older checkout: `/Users/charlene/Desktop/PYPY/paradedb-ha`; its original 0.26.0
  experimental artifacts are **not** substitutes for the release-matched binary.

Use repository-relative paths and metadata when moving machines. Pointsy contains
substantial unrelated user changes; its source-index changes were left uncommitted
as of this snapshot. Do not reset, clean or commit its entire working tree indiscriminately.

## What is on GitHub, and what is only local

The binary that the tested image uses came from successful CI run
[40457228418](https://github.com/sarkarghya/paradedb/actions/runs/40457228418), at the
published binary source SHA above. Its workflow is
`.github/workflows/experimental-physical-failover.yml`.

- AMD64 artifact: `pg_search-0.26.3-pg18-linux-amd64-experimental-failover`, ID
  `12714030507`.
- ARM64 artifact: `pg_search-0.26.3-pg18-linux-arm64-experimental-failover`, ID
  `12713821528`.
- CI builds/tests the extension and a disposable Patroni image. Its exported
  extension artifact is **not** the complete Pointsy database container image.
- Actions artifacts expire. The retained, versioned binaries are in the
  [fork prerelease](https://github.com/sarkarghya/paradedb/releases/tag/v0.26.3-pointsy.1).

Published archive SHA-256 values (also pinned in Pointsy's Dockerfile):

```text
fd57971c35c3a87fe25bf9f2c0be957130722fd89f478e05cf9fa5008bdda073  pg_search-0.26.3-pointsy.1-pg18-trixie-amd64.tar.gz
0d536aa922d9b8e63d3b2f1b8859c248273067440d462655e1960439f87dedff  pg_search-0.26.3-pointsy.1-pg18-trixie-arm64.tar.gz
```

The **full Pointsy image was built locally**, from
`pointsy-storefront/docker/database/image/Dockerfile`, using the archived binary:

```text
pointsy-search-ha:0.26.3-pinned
pointsy-search-source-standalone:0.26.3-pointsy.1
```

Neither full image was pushed to GHCR/another registry or deployed. Do not invent a
`ghcr.io/...` image URL or call the extension tarballs Docker images. Local image
tags can change/disappear; inspect Docker and rebuild from the pinned Dockerfile.
Both Docker targets (`standalone` and `ha`) built successfully. The final browser
drill used the **full pinned HA image**, not only an overlay on the old test image.

Follow-up maintenance CI at `2b1d5e3c...` was superseded/cancelled, not a failure of
the released runtime. Run
[40462422558](https://github.com/sarkarghya/paradedb/actions/runs/40462422558), at
`478cf6c03707adcef50495a0aedea1800df36f9b`, subsequently completed successfully.
This validates the later maintenance tooling too; the archived binary remains
pinned to `3e9cafa32c9a73fb0e9bd05593268b6f1a4fb802`, not that later commit.
Do not rerun/cancel builds solely to refresh docs.

## Implementation map

Fork runtime/test patch paths are recorded in `fork/physical-failover.patch`. Principal
areas: custom WAL resource-manager replay, recovery-aware metadata access,
standby planner/index-only-scan safeguards, and physical replication regressions.
`pg_search/src/gucs.rs` exposes `paradedb.physical_failover` as an INTERNAL,
read-only build capability. Upstream extension SQL, control template and dependency
lock stayed unchanged; do not relabel an older binary or downgrade extension SQL.

In Pointsy:

- Drizzle definitions in `packages/database/src/schema/` attach
  `organization_bm25_idx`, `stores_bm25_idx`, `categories_bm25_idx` and partial
  `opinions_bm25_idx` to their source tables. Tables and indexes remain logged.
- The opinion index predicate is `published_at IS NOT NULL AND private_at IS NULL`.
  Search SQL also enforces current published/project/team/privacy eligibility.
- `packages/database/src/lib/search.ts` checks the actual `pg_settings` row for
  `paradedb.physical_failover=on` with context `internal`, before logged-index
  creation. A custom-GUC placeholder or ordinary user-settable flag is not enough.
  This writer-side check cannot attest to other members: verify every node first.
- Preparation retires only the old projection triggers/functions. Drizzle removes
  the obsolete projection tables; there is no search readiness table or poller.
  A future cutover must stop old projection-reading/rebuilding API instances first.
- Store search in `packages/storefront-service/src/services/stores.ts` combines
  per-term organization/store/category index hits, requires every normalized term
  across their union, and keeps name/phrase boosts, prefixes, typo matching,
  publication filters, facets, maps and bounded spatial RRF with a pagination tail.
  Do not regress cross-table matching by searching only the store description.
- Opinion search in the adjacent `opinions.ts` queries the source index directly.
- Prefix aliases must use tokenizer options such as `alias: 'name_prefix'` on
  `tokenizer.edgeNgram(...)`. Casting `pdb.alias` to `pdb.edge_ngram` fails on 0.26.3;
  the isolated schema drill caught and corrected that syntax.
- The image verifies outer archive hashes, internal `SHA256SUMS` and `BUILD-INFO`,
  includes required pgvector/OpenBLAS dependencies and retains license/source.

## Validation and repeatable local commands

From the Pointsy repository (local Docker engine only):

```sh
docker build --platform linux/amd64 --target ha \
  -t pointsy-search-ha:0.26.3-pinned docker/database/image
SEARCH_HA_IMAGE=pointsy-search-ha:0.26.3-pinned \
  moon run storefront-service:smoke-local-search-ha
SEARCH_HA_IMAGE=pointsy-search-ha:0.26.3-pinned SEARCH_HA_REQUIRE_PATCHED=1 \
  moon run e2e:smoke-search-ha-production
moon run database:test-unit storefront-service:typecheck
```

The cluster harness creates its own database, volumes, three etcd members, two
strictly synchronous Patroni nodes and loopback-only ports. The browser drill uses
the production HAProxy configuration and the real API/storefront in production
mode. Runtime dotenv loading is disabled; credentials and Sentry/Cloudflare sinks
are local-only. Browser external requests are blocked. Do not repoint this drill
at production or shared staging. It retains only its own volumes for investigation.

Validated with 20 public stores and 5,005 opinions:

- Full pinned-image browser drill: run `7decd029`, passed. Evidence in Pointsy at
  `packages/e2e/test/results/search-ha-production/7decd029/metrics.json`, plus
  screenshots and process logs. Existing indexes answered after promotion before
  new writes. No rebuild, reindex or API restart; post-promotion writes and
  privacy/deletion checks passed. Observed final checks: switchover 22,505 ms;
  primary loss/rejoining 51,439 ms. These are drill observations, not outage SLAs.
- Final source-service HA suite: run `5e0a719d`, five tests passed, evidence at
  `packages/storefront-service/test/results/search-ha/5e0a719d/`. Includes plans,
  accents/name/phrase ranking, similarity, cross-source terms, prefixes/typos,
  paging, rollback, schema reapplication/legacy retirement and both failover modes.
- Database unit tests: 40 passed; service typechecking passed. Offline artifact
  packaging tests: five passed (`python3 -m unittest discover -s
  tests/physical_failover -p test_artifact.py -v` in this fork).
- Full E2E package typechecking has an unrelated existing `getPlan` error in
  `test/support/ui-state-demo/seed.ts:320`; it was not fixed as part of this work.
  An existing storefront SSR `Missing getServerSnapshot` location-hook warning is
  documented too. The isolated browser journey itself passed.

Older run `8f885779` used **unlogged projections and rebuilding**. Do not cite that
historical result as validation of the new source-index implementation. Backups,
PITR, production scale/networking, live cutover and concurrent standby search are
not certified by these isolated tests. See Pointsy's `docker/database/README.md`.

## Maintaining the parallel fork

Updates are explicit and release-by-release; there is no automatic upstream merge
or deployment. Use this workflow:

1. Check upstream with `python3 fork/prepare-release.py --check`. Inspect a real
   stable release; do not assume the example `v0.26.4` already exists.
2. Run `python3 fork/prepare-release.py vX.Y.Z NEW_DIRECTORY`. It fetches the tag,
   creates a new `fork/pg18-vX.Y.Z-r1` worktree and applies the small runtime/test
   patch. Conflicts stop for review. It does not reset, push, publish or deploy.
   Keep the new worktree's README current with its fork version and validation.
3. Review upstream storage/WAL/planner changes. Resolve conflicts deliberately,
   update release metadata/tests and regenerate the patch as described in
   `fork/README.md`. Keep previous release branches usable. For a new runtime binary at
   the same upstream release, increment `forkRevision`; do not overwrite revision 1.
4. Run formatting, offline packaging tests and reverse patch validation. Commit and
   push only the intended fork branch to `sarkarghya/paradedb`, never upstream/main.
   Run `experimental-physical-failover.yml` at that exact ref. Confirm **both**
   architecture jobs and the complete run succeeded, not merely compilation.
5. Download both artifacts from that same successful run. Confirm `headSha`,
   `BUILD-INFO`, platform, extension version and every checksum. Use the test-only
   overlay Dockerfile in Pointsy for candidates; rerun its isolated source-service
   and production-mode browser journeys, without rebuilding indexes after failure.
6. Before tagging, verify inherited upstream release publishers are disabled in
   this fork's Actions settings: Docs, Debian, macOS, PGXN, Postgres.app, Red Hat,
   Schema and Ubuntu. They match `v*` tags, even prereleases. They were disabled
   after the initial fork tag triggered queued builds (cancelled) and unauthorized
   docs/PGXN attempts (failed with missing credentials; nothing published there).
   Keep upstream workflow source intact, but never enable those publishers or add
   upstream credentials. Recheck for newly introduced publishers after upgrades.
7. Package validated artifacts with `fork/package-artifact.py` using the tested
   exact source SHA. It verifies provenance/checksums, retains corresponding source
   and license, produces deterministic tarballs, refuses overwrites and does not
   publish. Archive as a distinct fork prerelease only when publication is approved;
   preserve the matching tested SHA. Never reuse upstream release tags.
8. Pin the new fork URL, independent architecture hashes and expected source in
   Pointsy's Dockerfile. Build and rerun the drill with the **full pinned image**.
   Update this README and Pointsy's database README with real results/limits.
   If registry publication is requested, use a separately approved image workflow
   and record the actual registry URL and digest. Do not claim it exists beforehand.
9. Production rollout remains a separate, explicitly approved task: restorable
   backup, preserved volumes, legacy-data/target decisions, all-member library and
   preload rollout, restarts, provisioning, guarded schema application, then apps.
   Never roll back only the library to stock Community while logged BM25 indexes
   exist. Do not weaken strict synchronous replication to hide a replay failure.

## License and upstream

This fork is based on [ParadeDB](https://github.com/paradedb/paradedb) and retains
its [GNU Affero General Public License v3.0](LICENSE). It is not an official
ParadeDB release or a replacement for commercially supported Enterprise features.
