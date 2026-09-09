# brickdata — project instructions

brickdata is the **dataset** repo of the trio (studkit = library,
brickdata = dataset, blockstar = app — boundary of record: studkit
`PRD.md` Appendix B). It mirrors pinnable snapshots of brick-ecosystem
data (Rebrickable CSVs, LDraw library) to dated GitHub Releases and
builds `catalog.sqlite`. The `justfile` drives everything
(`mirror-*`, `build-catalog`, `publish-*`, `verify`); cleaning rules —
the package's main added value — are in `docs/cleaning.md`.

## Cross-project workstream context

Portfolio state for all four repos (blockstar, studkit, brickdata,
hyperbreak) lives in a separate private repo, **bkfunk/workstreams** —
one file per workstream holding its current position, typed next actions
for Brian, tangent stack, and cross-repo dependency edges. It is not in
this repo and never should be.

- **Skills:** `/ws-brief` (orient), `/ws-next` (what needs Brian),
  `/ws-handoff` (session end), `/ws-tangent` (detours), `/ws-sync`
  (reconcile with GitHub). They load when `~/dev/workstreams` is in the
  session. If they aren't available, orient manually:

  ```sh
  gh api repos/bkfunk/workstreams/contents/workstreams/<id>.md \
    --jq .content | base64 -d
  ```

  Read its frontmatter, `## Next actions`, and `## Pick up here`.
  `INDEX.md` at the repo root lists every workstream.
- **This repo appears in:** `brickdata-set-type`, `v0.1-release`,
  `catalog-builder-retirement`, `taxonomy-ownership`
- **Session protocol:** orient from the relevant workstream file before
  starting; capture detours with `/ws-tangent` rather than silently
  drifting; if workstream position changed, end with `/ws-handoff`.
- **State commits go to bkfunk/workstreams `main`, never to this repo.**
