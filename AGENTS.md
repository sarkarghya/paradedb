# ParadeDB fork: agent instructions

- This is Pointsy's experimental physical-failover fork, not upstream ParadeDB.
  Before changing runtime, builds, releases or replication tests, read
  `fork/AGENT-HANDOFF.md` and `fork/README.md`.
- Work on `fix/experimental-physical-failover-v0.26.3` or a new `fork/*` release
  branch. Preserve upstream `main` and tags. Match an exact stable upstream release;
  never merge development main into a release-matched binary.
- `fork/release.json` is the upstream/version pin. Keep upstream extension SQL,
  control template and `Cargo.lock` unchanged unless compatibility changes are
  explicitly reviewed. Regenerate `fork/physical-failover.patch` after runtime edits.
- The published runtime is `v0.26.3-pointsy.1`, source
  `3e9cafa32c9a73fb0e9bd05593268b6f1a4fb802`. Later maintenance commits are not
  included in that binary. Never overwrite a published artifact.
- This fork supports WAL replay and search **after promotion**, not concurrent
  standby search. Require the INTERNAL `paradedb.physical_failover=on` capability
  and the identical verified build on every member before logged BM25 creation.
- Keep inherited upstream release publishers disabled in the fork's GitHub Actions
  settings. Do not configure upstream publishing credentials or deploy from here.
- Do not modify production databases, run live failover, deploy containers or delete
  volumes without fresh explicit authorization. Validate with the isolated local
  production-mode Pointsy cluster and synthetic data.
- Distinguish CI extension binaries from container images: the tested Pointsy image
  was built locally, not published to GHCR. Check current registry state before
  claiming an image exists on GitHub. Preserve unrelated Pointsy working-tree edits.
