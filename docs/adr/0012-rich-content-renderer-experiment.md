# Rich-content renderer experiment: Chromium and Kitty approved

## Status

Decided. The bounded ticket-01 experiment approves the tested Linux configuration: system Chromium rendered through mandatory Bubblewrap and cgroup isolation, displayed through Kitty's direct PNG graphics protocol. Other configurations retain readable fallback behavior.

The 2026-09-06 macOS evaluation does not approve a graphical renderer. Keep the readable text fallback on macOS: an outer `sandbox-exec` profile crashed Chrome before CDP became available, while Chrome's built-in child-process Seatbelt sandbox left the controlling browser PID able to read an arbitrary host sentinel and the measured process tree exceeded the 200 MiB ticket ceiling.

## Context

The TUI needs a safe path from untrusted static HTML/CSS to terminal graphics. ADR 0008 keeps presentation in the frontend; ADRs 0009 and 0010 keep Extension execution in QuickJS and retain broker and Replay authority. This experiment used no engine, Session, database, Plugin, or Extension integration.

The initial attempt created an incognito BrowserContext but passed `newWindow: false` when creating that context's first target. Chromium 152 returned `Failed to open new tab - no browser is open`. Creating the first target with `newWindow: true` is required for a fresh context in this headless configuration. A diagnostic 256-task run proved that increasing the task ceiling alone did not fix the original call. The recovered configuration combines the corrected target call with a measured 256-task ceiling; the successful workload peaked at 127 tasks.

## Decision

Use system-provided Chromium with all of the following requirements:

- Chromium runs inside Bubblewrap with new user, PID, mount, network, IPC, and UTS namespaces, no capabilities, no inherited host environment, private `/tmp`, minimal `/dev`, and only the recorded read-only runtime and DejaVu font mounts.
- The trusted controller and Chromium run in a transient user systemd scope capped at 1 GiB memory, zero swap, 256 tasks, and 200% CPU.
- Control uses inherited private CDP file descriptors with NUL-terminated JSON. No debugging TCP port is opened.
- Every job creates and disposes a fresh incognito BrowserContext and creates that context's first target with `newWindow: true`.
- Script execution and downloads are disabled before navigation. Request interception supplies exactly one controller-owned main document and exact digest-bound PNG assets. Every other resource is denied.
- Documents receive the recorded CSP header. Output, input, asset, response, time, resource, and concurrency limits remain mandatory.
- Kitty direct PNG graphics is the approved terminal path. Capability and cell geometry are queried; identity alone is not accepted.
- Text fallback remains available for unsupported terminals, disabled graphics, missing renderers, and renderer failures.

Chromium is an optional system package updated by the user's OS package manager. STcli must not download browsers, reuse user browser profiles, change the default terminal, or install or update packages. Renderer changes require this proof to be repeated.

## Observed configuration

| Component | Tested version / policy |
|---|---|
| Chromium | 152.0.7977.82, system package, fresh private profile |
| Bubblewrap | 0.12.0; isolated namespaces and restricted mounts |
| systemd | 261.2-1-arch; 1 GiB memory, zero swap, 256 tasks, 200% CPU |
| Kitty | 0.48.2; direct PNG stream protocol with owned image and placement IDs |
| Alacritty | 0.17.0 (94e7c887); capability probe returned no Kitty acknowledgement, so the readable fallback path was selected |
| Font | Read-only DejaVu Sans from `/usr/share/fonts/TTF/DejaVuSans.ttf` |
| Assets | Repository-owned PNG, SHA-256 `f331033487acbbe3714bda038a21e91ef640c9f224748bb7078b9bd5e2eb4817` |
| macOS | macOS 26.4.1 arm64; Google Chrome 152.0.7977.76; `sandbox-exec`; Kitty 0.48.2. Terminal graphics passed, but renderer isolation and memory limits failed, so text fallback remains selected. |

## Evidence and acceptance criteria

Compact evidence is in `docs/experiments/rich-content-renderer/`: `results.json`, `reproduce.txt`, `isolation.json`, `measurements.json`, `terminal-transfer.json`, `rendered-card.png`, and `terminal-evidence.png`.

| Ticket criterion | Result | Evidence |
|---|---|---|
| Styled card, columns, and approved asset inside a terminal | Met | `rendered-card.png` is the exact restricted-renderer image displayed by the TUI; `terminal-evidence.png` records its real Kitty placement. |
| Resize, scroll, popup, removal, missing renderer, no graphics | Met | The dedicated Kitty TUI received fixture, scroll, popup, source, removal, restore, literal, and exit keys. Missing-renderer and graphics-off fallback were retained from the original run. |
| Deny scripts, handlers, network, files, redirects, nested documents, control access | Met | `isolation.json`: marker unchanged, main URL unchanged, zero nested markers, zero unexpected targets, zero listener requests; the same Bubblewrap boundary could not see the host sentinel, host process root, runtime sockets, or loopback listener. Observed Chromium renderer subprocesses had seccomp filtering, no capabilities, `NoNewPrivs=1`, and no listening sockets. Child frames were instantiated as blocked `about:srcdoc`/`chrome-error`/empty URLs with no nested marker content, which the probe classifies as denied frames rather than loaded documents. |
| Startup, warm render, transfer, peak resource measurements | Met | `measurements.json` and `terminal-transfer.json`. |
| Installation/update, support, asset/font, lifecycle, fallback policy | Met | This ADR records the selected policies. |
| Select backend or exact unmet requirement | Met | The tested Chromium/Bubblewrap/cgroup plus Kitty configuration is approved. |
| Preserve QuickJS/Core/Session behavior | Met | Experiment-only frontend files; no production or domain changes. |
| ADR, no production availability claim | Met | This decision does not claim shipped TUI availability. |
| Real isolation and terminal evidence, no mock-only claim | Met | Real Chromium, OS boundary, Kitty, process cleanup, and terminal capture were exercised. |

### macOS evaluation

The macOS evidence is `results-macos.json`, `isolation-macos.json`, `seatbelt-primary.json`, `measurements-macos.json`, `terminal-macos.json`, `rendered-card-macos.png`, `terminal-card-macos.png`, `terminal-graphics-off-macos.png`, `terminal-missing-renderer-macos.png`, and `reproduce-macos.txt` in the same evidence directory.

| macOS criterion | Result | Evidence |
|---|---|---|
| Positive controls and hostile-document denial | Partial | The host sentinel and loopback listener were reachable outside. The CDP policy kept the hostile marker unchanged, retained the main URL, loaded no nested marker, opened no unexpected target, and sent zero listener requests. |
| OS process isolation | Unmet | The outer deny-default Seatbelt profile exited by signal 11 before CDP. With Chrome's built-in sandbox, `sandbox_check` on the live controlling browser PID reported `file-read-data` allowed for the host sentinel. Request interception and child-process Seatbelt do not confine that trusted browser parent. |
| Positive rendering | Met experimentally, not approved | Card and columns rendered at 800/900 pixels; maximum PNGs were 182,336 and 103,376 bytes, below the ticket's 3 MiB ceiling. |
| Terminal graphics and fallbacks | Met | Kitty returned cell geometry and acknowledged the macOS-rendered card 0.0139 s after flush. Dedicated real-Kitty window captures record the graphical card, graphics-off fallback, and missing-renderer fallback. Resize, scroll, popup, source, removal, restore, literal, and exit paths were exercised. |
| Startup and warm rendering | Met | Three starts were 1.121 s, 0.335 s, and 0.850 s. Card median/max were 0.717/0.825 s; columns were 0.696/0.712 s. |
| Memory and task ceilings | Memory unmet; tasks met | The observed Chrome process tree used 1,094,025,216 bytes RSS, over the 200 MiB ticket ceiling. macOS `libproc` reported 125 aggregate threads, below the 256-task ceiling. |
| Cleanup | Met | Chrome exited gracefully, the controller reaped it, the temporary profile was removed, and no private-profile process remained. |

This rejection does not weaken the Linux decision or select Chrome's built-in sandbox as a substitute. A future macOS backend must repeat the experiment with an enforceable parent-process filesystem/network boundary, finite memory/task controls, and framebuffer capture permission before graphical support can be approved.


## Measurements and selected ceilings
The full cold path—transient systemd scope, Python controller, Chromium startup, first restricted card render, response framing, and process cleanup—was measured three times at 0.681 s, 0.669 s, and 0.700 s. Chromium-only startup was 0.139 s, 0.142 s, and 0.164 s. Ten sequential 900-pixel renders per fixture produced:

| Fixture | Median render | Maximum render | Maximum PNG | Output |
|---|---:|---:|---:|---:|
| Card | 0.240 s | 0.398 s | 181,767 bytes | 900×600 |
| Columns | 0.213 s | 0.257 s | 80,994 bytes | 900×841 |

The measured scope peak was 182,935,552 bytes memory and 127 tasks. CPU accounting is recorded in `measurements.json`. The Kitty transfer used 303,886 protocol bytes for a 227,386-byte PNG; write and flush took 0.00056 s and the matching protocol acknowledgement arrived 0.0105 s after flush. The acknowledgement proves receipt, not display latency.

Selected ceilings are: 256 KiB combined HTML/CSS; 1 MiB and 1,048,576 pixels per approved PNG; 4 MiB aggregate assets; 1600×4096 output; 32 MiB PNG; 48 MiB framed response; 10 seconds for the full cold path; 3 seconds warm rendering; one active plus one replaceable pending request; 1 GiB scope memory; zero swap; 256 tasks; two CPUs; 128 MiB private `/tmp`; and 64 MiB private `/dev/shm`. Observed values fit these ceilings without widening fidelity or isolation.


## Support table
| Environment | Outcome |
|---|---|
| Kitty 0.48.2 on the tested Linux/Sway setup | Tested graphical path |
| Text and `--graphics off` on this Linux setup | Tested readable fallback; no Chromium launch |
| Missing renderer | Tested readable fallback |
| Alacritty 0.17.0 | Tested readable fallback after capability negotiation returned no Kitty acknowledgement |
| Multiplexers, remote transport, other Linux terminals | Unverified; readable fallback intended |
| macOS 26.4.1 with Kitty 0.48.2 | Kitty protocol tested; renderer rejected, readable fallback selected |

## Lifecycle and trust consequences

On the approved Linux path, text-only startup does not launch Chromium. A successful browser is reused for sequential renders and retired after 30 seconds idle, explicit exit, or failure. Each render disposes its BrowserContext. Exit removes the owned Kitty placement and image, closes pipes, terminates and reaps the owned scope, and removes private profiles. The observed normal exit left no `rich_content_probe`, `stcli-rich-probe`, or private-profile process.

The approved Linux renderer never receives Session data, credentials, Extension authority, a user browser profile, arbitrary paths, network access, or generic CDP commands. The rejected macOS secondary approach did retain host filesystem authority in its controlling browser PID and is therefore not an approved implementation. Replay and headless use remain renderer-free. This experiment introduces no production rendering or user-facing availability.
