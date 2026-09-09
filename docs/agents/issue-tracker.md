# Issue tracker: Gitea

Issues for this repo are tracked on the local Gitea instance at
[git.februus.net/its-a-unixsystem/STcli-scratch](https://git.februus.net/its-a-unixsystem/STcli-scratch/issues).

The historical `.scratch/` issue files were migrated to Gitea on 2026-09-09;
their content (and the migration mapping in `.scratch-migration-mapping.json`,
committed inside the scratch repo) remains available in the scratch repo's git
history at `ssh://git@192.168.178.10:2222/its-a-unixsystem/STcli-scratch.git`.

## Conventions

- One Gitea issue per ticket; issues carry a `feature/<feature-slug>` label
  identifying the epic they belong to.
- Triage state is recorded as a Gitea label; see `triage-labels.md` for the
  canonical role strings (`needs-triage`, `needs-info`, `ready-for-agent`,
  `ready-for-human`, `wontfix`, `done`). Closed issues map to `done`,
  `resolved`, `dropped`, or `wontfix`.
- Comments and conversation history live as Gitea issue comments.
- Specs and wayfinding maps remain as markdown files in the scratch repo
  (`.scratch/<feature-slug>/spec.md`, `.scratch/<effort>/map.md`).

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
GET https://git.februus.net/api/v1/repos/its-a-unixsystem/STcli-scratch/issues?state=open&labels=<id>&limit=50&page=<n>
```

## Wayfinding operations

Used by `/wayfinder`. The **map** is a markdown file in the scratch repo with
one Gitea issue per child ticket.

- **Map**: `.scratch/<effort>/map.md` (the Notes / Decisions-so-far / Fog body).
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
