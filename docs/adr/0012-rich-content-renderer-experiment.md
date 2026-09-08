# Rich-content renderer experiment: graphical production selection retired

## Status

Retired on 2026-09-08. STcli no longer develops or ships graphical terminal rendering. ADR 0013's styled-text renderer is the sole TUI Candidate presentation path; the executable Chromium, WebKit, and Kitty graphics probes were removed.

The bounded ticket-01 experiment did successfully approve the tested Linux configuration: system Chromium rendered through mandatory Bubblewrap and cgroup isolation, displayed through Kitty's direct PNG graphics protocol. The later macOS WebKit helper experiment was also functionally viable under its documented constraints. Retiring graphical rendering is a product-scope decision, not a reversal of those technical findings.

The measurements, captures, and reproduction ledgers under `docs/experiments/rich-content-renderer/` remain historical evidence. Their commands and probe paths describe the repository at the time of the experiments and are not runnable from current HEAD.

## Context

The experiment evaluated a safe path from untrusted static HTML/CSS to terminal graphics. ADR 0008 keeps presentation in the frontend; ADRs 0009 and 0010 keep Extension execution in QuickJS and retain broker and Replay authority. The experiment used no engine, Session, database, Plugin, or Extension integration.

The initial attempt created an incognito BrowserContext but passed `newWindow: false` when creating that context's first target. Chromium 152 returned `Failed to open new tab - no browser is open`. Creating the first target with `newWindow: true` is required for a fresh context in this headless configuration. A diagnostic 256-task run proved that increasing the task ceiling alone did not fix the original call. The recovered configuration combines the corrected target call with a measured 256-task ceiling; the successful workload peaked at 127 tasks.

## Historical decision

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

### macOS WebKit evaluation

Ticket [13](../../.scratch/extension-interactions-and-rich-content/issues/13-evaluate-macos-sandboxed-webkit.md) evaluated an experiment-only, ad-hoc-signed app bundle using App Sandbox and WKWebView. Evidence is in [`docs/experiments/rich-content-renderer/macos-webkit/`](../experiments/rich-content-renderer/macos-webkit/). The helper is functionally viable under the clarified constraints: the measured footprint is accepted and direct terminal launch is sufficient for this use case. This experiment still does not by itself select a production macOS backend, so text fallback remains the current behavior.

| macOS WebKit criterion | Result | Evidence |
|---|---|---|
| Signed helper and parent/child file confinement | Met in the controlled run | `macos-webkit-entitlements.json` records exactly `app-sandbox` and `network.client`; `macos-webkit-isolation.json` records denied sentinel reads/writes for the helper and timing-correlated WebKit services. Attribution may over-count services created during the controlled window or under-count reused shared services. |
| Hostile-document denial and approved asset control | Met | Content JavaScript stayed disabled, the marker remained `original`, no nested marker loaded, navigation was cancelled, and the loopback listener received zero document requests. The digest-bound `stcli-probe` asset loaded at 128 pixels wide. |
| Unchanged fixture fidelity | Known policy limitation | Card and columns snapshots were valid and viewport changes produced different PNGs. macOS substituted the requested DejaVu font. The fixture's emblem is an arbitrary external HTTPS URL, which the renderer correctly does not fetch. The trusted controller supplies verified/content-addressed asset bytes, and WebKit exposes them to the document over the local `stcli-probe://` scheme instead. |
| Bounds and cancellation | Met for the experiment | HTML, single-asset, aggregate-asset, and 4096-pixel output ceilings rejected excess input; SIGTERM completed in 0.00162 seconds. Resource use is measured rather than cgroup-enforced on macOS; that is accepted for this helper evaluation. |
| Startup and warm rendering | Met | Three cold paths were 0.654, 0.359, and 0.311 seconds. Card warm median/max were 0.174/0.282 seconds; full samples and columns values are in `macos-webkit-measurements.json`. |
| Memory and threads | Accepted | Endpoint aggregate physical footprint was 418,108,048 bytes and aggregate RSS was 490,569,728 bytes, with shared-memory double-counting caveats. This footprint is accepted for the macOS helper. The attributed process set had 64 threads, below 256. |
| Terminal lifecycle and cleanup | Met with one evidence gap | Real Kitty displayed the card and acknowledged direct PNG transfer; popup, source, removal, restore, literal, crash, and fallback paths remained responsive. Explicit graphics-off and missing-renderer states were captured. A non-TTY shell could not provide an additional raw-mode non-graphics run. No owned helper remained after exit. |
| Distribution | Met for the CLI launch model | The ad-hoc-signed helper launched directly from the terminal without an IDE. Gatekeeper acceptance, Developer ID signing, and notarization are not requirements for this development/CLI model; they would become prerequisites only for normal end-user distribution. WebKit is OS-provided. The helper needed a logged-in GUI session but no visible window or Screen Recording permission. Only macOS 26.4.1 arm64 was exercised. |
WKWebView terminated under App Sandbox without `com.apple.security.network.client`; the working configuration therefore requires that entitlement. It gives the helper process outbound-connect capability, which remains documented as residual OS authority. Rendered documents do not receive general network capability: content JavaScript is disabled, content rules block external subresources, and navigation delegates reject non-`stcli-probe://` navigation. ADR 0010 Brokered HTTPS Egress covers deliberate Extension `fetch` and secondary-inference/provider calls; it does not turn arbitrary renderer URLs into allowed effects. The trusted controller resolves approved assets as verified/content-addressed bytes and serves them to WebKit over the local `stcli-probe://` scheme, so WebKit never fetches their original URLs. Therefore the blocked external HTTPS emblem demonstrates the intended resource policy, not a proxy failure. With the measured footprint and direct-terminal launch model accepted, the helper is a viable experimental backend; production integration of this resolver remains a separate decision.


## Measurements and selected ceilings
The full cold path—transient systemd scope, Python controller, Chromium startup, first restricted card render, response framing, and process cleanup—was measured three times at 0.681 s, 0.669 s, and 0.700 s. Chromium-only startup was 0.139 s, 0.142 s, and 0.164 s. Ten sequential 900-pixel renders per fixture produced:

| Fixture | Median render | Maximum render | Maximum PNG | Output |
|---|---:|---:|---:|---:|
| Card | 0.240 s | 0.398 s | 181,767 bytes | 900×600 |
| Columns | 0.213 s | 0.257 s | 80,994 bytes | 900×841 |

The measured scope peak was 182,935,552 bytes memory and 127 tasks. CPU accounting is recorded in `measurements.json`. The Kitty transfer used 303,886 protocol bytes for a 227,386-byte PNG; write and flush took 0.00056 s and the matching protocol acknowledgement arrived 0.0105 s after flush. The acknowledgement proves receipt, not display latency.

Selected ceilings are: 256 KiB combined HTML/CSS; 1 MiB and 1,048,576 pixels per approved PNG; 4 MiB aggregate assets; 1600×4096 output; 32 MiB PNG; 48 MiB framed response; 10 seconds for the full cold path; 3 seconds warm rendering; one active plus one replaceable pending request; 1 GiB scope memory; zero swap; 256 tasks; two CPUs; 128 MiB private `/tmp`; and 64 MiB private `/dev/shm`. Observed values fit these ceilings without widening fidelity or isolation.


## Historical support table
| Environment | Outcome |
|---|---|
| Kitty 0.48.2 on the tested Linux/Sway setup | Tested graphical path |
| Text and `--graphics off` on this Linux setup | Tested readable fallback; no Chromium launch |
| Missing renderer | Tested readable fallback |
| Alacritty 0.17.0 | Tested readable fallback after capability negotiation returned no Kitty acknowledgement |
| Multiplexers, remote transport, other Linux terminals | Unverified; readable fallback intended |
| macOS 26.4.1 with Kitty 0.48.2 | Kitty protocol and the WebKit helper were tested; the helper is functionally viable under its documented no-arbitrary-network policy, but text fallback remains the current default pending production integration. |
## Historical lifecycle and trust consequences

On the approved Linux path, text-only startup does not launch Chromium. A successful browser is reused for sequential renders and retired after 30 seconds idle, explicit exit, or failure. Each render disposes its BrowserContext. Exit removes the owned Kitty placement and image, closes pipes, terminates and reaps the owned scope, and removes private profiles. The observed normal exit left no `rich_content_probe`, `stcli-rich-probe`, or private-profile process.

The approved Linux renderer never receives Session data, credentials, Extension authority, a user browser profile, arbitrary paths, network access, or generic CDP commands. The macOS WebKit experiment now records a viable CLI-started helper with accepted footprint, but it does not grant rendered HTML network access. ADR 0010's Brokered HTTPS Egress remains the deliberate live-effect path for Extension `fetch` and secondary inference; renderer resources use explicit verified references. Replay and headless use remain renderer-free. This experiment introduces no production rendering or user-facing availability.
