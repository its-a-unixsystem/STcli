# Issue tracker: Gitea

Issues for this repo are tracked on the local Gitea instance at
[git.februus.net/its-a-unixsystem/STcli-scratch](https://git.februus.net/its-a-unixsystem/STcli-scratch/issues).

The historical local issue files were migrated to Gitea on 2026-09-09. Their
content and the migration mapping remain available in the scratch repository's
git history. The repository has no persistent local checkout inside STcli.

## Conventions

- One Gitea issue per ticket; issues carry a `feature/<feature-slug>` label
  identifying the epic they belong to.
- Triage state is recorded as a Gitea label; see `triage-labels.md` for the
  canonical role strings (`needs-triage`, `needs-info`, `ready-for-agent`,
  `ready-for-human`, `wontfix`, `done`). Closed issues map to `done`,
  `resolved`, `dropped`, or `wontfix`.
- Comments and conversation history live as Gitea issue comments.
- Specs, wayfinding maps, verification evidence, and other planning artifacts
  are versioned files in the remote scratch repository.

## When a skill needs a spec, map, or evidence

Read repository files from
`https://git.februus.net/its-a-unixsystem/STcli-scratch` through Gitea, or use
a temporary checkout of
`ssh://git@192.168.178.10:2222/its-a-unixsystem/STcli-scratch.git` outside the
STcli working tree. Repository-relative paths keep their historical layout,
for example `<feature-slug>/spec.md`, `<effort>/map.md`, and
`<feature-slug>/verification/`.

## When a skill says "publish to the issue tracker"

Create a Gitea issue in `its-a-unixsystem/STcli-scratch` via the API:

```
POST https://git.februus.net/api/v1/repos/its-a-unixsystem/STcli-scratch/issues
Authorization: token $GITEA_TOKEN
```

Set the epic label (`feature/<feature-slug>`) and the matching triage label.
Label IDs must be integers resolved from
`GET /repos/its-a-unixsystem/STcli-scratch/labels` (paginated, 50 per page).

## When a skill says "fetch the relevant ticket"

Fetch the Gitea issue by number, or query open issues:

```
GET https://git.februus.net/api/v1/repos/its-a-unixsystem/STcli-scratch/issues?state=open&labels=<name>&limit=50&page=<n>
```

## Wayfinding operations

Used by `/wayfinder`. The **map** is a markdown file in the remote scratch
repository with one Gitea issue per child ticket.

- **Map**: `<effort>/map.md` in the remote scratch repository (the Notes /
  Decisions-so-far / Fog body).
- **Child ticket**: a Gitea issue labeled `feature/<effort>`, with the question
  in the body. A `Type:` line records the ticket type
  (`research`/`prototype`/`grilling`/`task`).
- **Blocking**: a `Blocked by: #NN, #NN` line near the top of the issue body.
  A ticket is unblocked when every issue it lists is closed.
- **Frontier**: query open, unclaimed issues with the `feature/<effort>` label;
  lowest issue number wins.
- **Claim**: comment `claimed` on the issue (or assign yourself) before work.
- **Resolve**: close the issue with the answer in the final comment, then
  append a context pointer (gist + issue link) to the map's Decisions-so-far
  in `map.md`.
