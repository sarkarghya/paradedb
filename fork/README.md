# Parallel ParadeDB fork

Keep upstream `main` and release tags intact. Develop only on `fork/*` or the
existing `fix/experimental-physical-failover-v0.26.3` branch. Do not merge upstream
development main into a release-matched build.

`release.json` pins the exact upstream release commit, extension version and fork
revision. The extension remains SQL/format-compatible with that upstream release;
the fork revision identifies our binary separately. CI rejects changes to upstream
SQL, the control template or `Cargo.lock`. An incompatible upstream change must be
reviewed explicitly, never relabelled as an existing release.

## Updating alongside upstream

```sh
python3 fork/prepare-release.py --check
python3 fork/prepare-release.py v0.26.4 ../paradedb-ha-0.26.4
```

The second command creates a new worktree/branch from the upstream tag and applies
only `physical-failover.patch`, the small runtime/test patch series. Conflicts stop
the operation for human review. It never resets a branch, overwrites existing
worktrees, pushes, publishes or deploys. Review the new upstream release, update
tests if needed, commit the prepared changes and push the new `fork/*` branch to
**sarkarghya/paradedb**, not upstream. The existing release branch remains usable.

After changing runtime code, regenerate the patch from the pinned upstream commit:

```sh
git diff --binary --full-index "$(python3 -c 'import json; print(json.load(open("fork/release.json"))["upstreamCommit"])')" -- \
  pg_search/Cargo.toml pg_search/src/gucs.rs \
  pg_search/src/postgres/storage/custom_rmgr.rs pg_search/src/postgres/storage/metadata.rs \
  pg_search/src/postgres/index_only.rs pg_search/src/postgres/customscan/hook.rs \
  tests/tests/replication.rs > fork/physical-failover.patch
```

CI builds PG18 on AMD64 and ARM64, tests real physical WAL/promotion and two
Patroni elections/rejoining cycles, and records source/checksums in the artifact.
Run Pointsy's isolated production-mode storefront HA journey before adopting a new
fork build. Do not promote merely because compilation passed.

## Consumer contract

- `SHOW paradedb.physical_failover` must be `on`. This is an INTERNAL read-only
  build capability, not a user-toggleable way to bypass Community safety checks.
- Every cluster member must run the exact tested build with `pg_search` preloaded
  before a logged search index is created. The application schema push checks the
  writer's capability before making any such index.
- Search traffic goes only to the current primary. Concurrent standby search is
  still unsupported. This fork is experimental, not Enterprise or upstream-supported.
- Pin a fork revision and immutable checksums. Keep the library, matching SQL,
  control file, license, corresponding source and BUILD-INFO together. GitHub
  Actions artifacts expire; archive a validated build as a versioned fork
  prerelease before using it as an application-image dependency.
- Publish only to the fork, with a distinct tag such as `v0.26.3-pointsy.1`, and
  never overwrite a published artifact. No production deployment is automatic.
