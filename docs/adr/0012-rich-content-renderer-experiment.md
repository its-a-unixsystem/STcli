# Rich-content renderer experiment: no backend approved

## Status

Decided. The bounded ticket-01 experiment approves no renderer backend. Renderer-dependent follow-on work remains blocked.

## Context

The TUI needs a safe path from untrusted static HTML/CSS to terminal graphics. ADR 0008 keeps presentation in the frontend; ADRs 0009 and 0010 keep Extension execution in QuickJS and retain broker/Replay authority. This experiment therefore used no engine, Session, database, Plugin, or Extension integration.

The candidate was system Chromium rendered through an isolated Bubblewrap process, controlled only by Chromium's private remote-debugging pipe, and displayed through Kitty's direct PNG graphics protocol. The controller was to run inside a transient user systemd scope.

## Decision

**No backend approved.** Chromium's private `Browser.getVersion` CDP pipe handshake succeeded through Bubblewrap. The next benign operation, `Target.createTarget` for an `about:blank` page, failed with `Failed to open new tab - no browser is open` before any untrusted input was supplied. The required isolated positive render therefore could not begin.

Opening a DevTools TCP port, weakening Bubblewrap isolation, using `--no-sandbox`, or feeding hostile fixtures to an unprotected browser would violate the approved trust model. The experiment therefore stopped before untrusted rendering and did not substitute a weaker backend.

The candidate finite ceilings remain candidate values, not selected production policy. Renderer-dependent work must repeat the proof and establish why this installed Chromium process cannot create a target under the required boundary.

## Observed configuration

| Component | Tested version / policy |
|---|---|
| Chromium | 152.0.7977.82, system package, private profile intended |
| Bubblewrap | 0.12.0; private CDP pipe transport succeeded; isolated positive render did not |
| systemd | 261.2-1-arch; requested 1 GiB memory, zero swap, 128 tasks, 200% CPU |
| Kitty | 0.48.2; direct PNG protocol candidate, graphical transfer not reached |
| Alacritty | 0.17.0 (94e7c887); graphical/fallback capability not claimed |
| Font | Read-only DejaVu Sans candidate |
| Assets | Repository-owned PNG only, SHA-256 `f331033487acbbe3714bda038a21e91ef640c9f224748bb7078b9bd5e2eb4817` |
| Distribution | Optional system packages updated by the user's OS package manager; STcli must not download or update a browser |

## Evidence and acceptance criteria

Compact evidence is in `docs/experiments/rich-content-renderer/`: `results.json`, `reproduce.txt`, `isolation.json`, and `measurements.json`.

| Ticket criterion | Result | Evidence |
|---|---|---|
| Styled card, columns, and approved asset inside a terminal | Unmet | Fixtures and digest exist, but restricted Chromium could not create a target. |
| Resize, scroll, popup, removal, missing renderer, no graphics | Partly met | The real TUI fallback and controls ran; graphical lifecycle was not exercised. |
| Deny scripts, handlers, network, files, redirects, nested documents, control access | Unmet | Positive outside controls succeeded; isolated Chromium failed before hostile input. |
| Startup, warm render, transfer, peak resource measurements | Unmet | Measurement stopped at the same safety prerequisite; no samples are claimed. |
| Installation/update, support, asset/font, lifecycle, fallback policy | Met for decision outcome | This ADR records the candidate policies and readable fallback requirement. |
| Select backend or exact unmet requirement | Met | No backend approved; isolated Chromium could not create the benign page target required for rendering. |
| Preserve QuickJS/Core/Session behavior | Met | Experiment-only frontend files; no production or domain changes. |
| ADR, no production availability claim | Met | This document is the decision; user documentation is unchanged. |
| Real isolation and terminal evidence, no mock-only claim | Partly met | Real process and TUI probes ran; failed graphical/isolation criteria remain explicit. |

## Candidate policy not selected

The attempted ceilings were: 256 KiB combined HTML/CSS; 1 MiB and 1,048,576 pixels per approved PNG; 4 MiB aggregate assets; 1600×4096 output; 32 MiB PNG; 48 MiB framed response; 10 seconds cold startup; 3 seconds warm render; one active plus one replaceable pending request; 1 GiB scope memory; zero swap; 128 tasks; two CPUs; 128 MiB private `/tmp`; and 64 MiB private `/dev/shm`.

Because no positive render or measurement completed, none is approved as a production limit.

## Support table

| Environment | Outcome |
|---|---|
| Text and `--graphics off` on this Linux setup | Tested readable fallback |
| Missing renderer | Tested readable fallback |
| Kitty 0.48.2 graphics | Unverified; isolation prerequisite blocked rendering and transfer |
| Alacritty 0.17.0 | Unverified; no identity-based capability claim |
| Multiplexers, remote transport, other Linux terminals | Unverified; readable fallback intended |
| macOS and Windows | Unverified; readable fallback intended |

## Lifecycle and trust consequences

Text-only startup must not launch Chromium. A future renderer must use a fresh private profile, fresh browser context per job, digest-bound PNG assets, header CSP, disabled scripts, denied downloads/navigation/resources, bounded dimensions/time/memory/concurrency, capped diagnostics, and explicit context/process/profile cleanup. It must never receive Session data, credentials, Extension authority, a user profile, arbitrary paths, network access, or generic CDP commands. Replay and headless use remain renderer-free.

No graphical availability is shipped or documented by this experiment.
