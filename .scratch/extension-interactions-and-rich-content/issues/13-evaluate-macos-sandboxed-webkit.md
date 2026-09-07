# 13: Evaluate an App Sandbox and WebKit renderer on macOS

**What to build:** A disposable, separately confined macOS helper using WKWebView to render static HTML/CSS into an image for the existing Kitty display path. Verify the controlling helper and WebKit processes are restricted, not merely that page scripts are disabled.

**Blocked by:** None. Execute on a Mac; tickets 11 and 12 are independent. This is the recommended first graphical alternative, not an approved backend.

**Status:** ready-for-human

## Goal and boundaries

The earlier evaluation demonstrated Mac terminal graphics but rejected its renderer: the outer Seatbelt profile crashed Chrome before CDP, while Chrome's built-in sandbox left the browser parent with host-file authority. This ticket evaluates a different platform-supported helper boundary; embedding WKWebView in the unrestricted main TUI process does not solve that problem.

Keep Core, Candidate content, Session state, QuickJS execution, the production TUI, and the approved Linux backend unchanged. Use a disposable example/helper with the same input/output meaning: document bytes, verified assets, viewport, bounded PNG or failure. No second authoritative runtime, generic automation commands, network service, or production renderer framework. A prototype helper bundle is permitted; a second user-facing frontend is not.

## Mac prerequisites and execution

1. Read `CONTEXT.md`, ADRs 0009, 0010, and 0012, the epic isolation requirements, and the previous Mac evidence. Record macOS build, CPU architecture, checkout revision, Rust/Xcode/SDK versions, and pre-existing work.
2. Verify the APIs and distribution model against current Apple documentation: [App Sandbox](https://developer.apple.com/documentation/security/app-sandbox), [WKWebView snapshots](https://developer.apple.com/documentation/webkit/wkwebview/takesnapshot(with:completionhandler:)), and [content JavaScript control](https://developer.apple.com/documentation/webkit/wkwebpagepreferences/allowscontentjavascript). Document the minimum macOS version actually exercised, not just API availability.
3. Determine a viable signed helper bundle or XPC-service launch arrangement from the CLI. Prove its effective entitlements and sandbox at runtime before rendering hostile inputs. Do not assume an entitlement declaration, an XPC process, ad-hoc signing, or WebKit child isolation automatically confines the parent. If a signing identity, provisioning, tooling, or permission is unavailable, record the prerequisite and stop the dependent step rather than weakening the boundary.
4. Use a private, narrow request/reply channel and an ephemeral web data store. Disable content JavaScript before loading any content; deny navigation, new windows, downloads, remote resources, arbitrary file URLs, and unexpected messages. Resolve approved assets through exact verified references. Inspect WebKit network/content/GPU processes, not just the helper PID. No user-selected-file grants, host home mounts, or inherited broad rights may be used to rescue the experiment.
5. Render unchanged existing card, columns, approved asset, and literal fixtures. Verify WKWebView layout readiness and snapshot completion rather than using an arbitrary sleep. A bounded offscreen view/window is allowed; record if foreground application state, window visibility, a logged-in GUI session, or permissions are required. Never capture unrelated desktop content as a substitute for a web-view snapshot.
6. Execute the acceptance scenarios below and capture real Kitty output. Record exact build/sign/launch/run commands in the ledger; the Chromium-specific Python/CDP runner is not presumed to support WebKit. Stop at an evidence-backed decision, not production integration.

## acceptance criteria:

- [x] AC1 — **Pass with documented residual authority.** `macos-webkit-entitlements.json` and `macos-webkit-isolation.json` prove the signed helper and attributed services denied owned host-sentinel reads/writes. Positive controls passed. `com.apple.security.network.client` is required for WKWebView and leaves process-level outbound authority; rendered documents still have no arbitrary network access. Service attribution used controlled-run timing correlation rather than a definitive ownership API.
- [x] AC2 — **Pass for observed storage and input authority.** Two fresh views returned empty cookies and created no persistent cookie/cache marker. The helper received bounded document bytes over pipes and no Session API, credentials, browser profile, shell interface, or user-file entitlement. Evidence: `macos-webkit-isolation.json`.
- [x] AC3 — **Pass.** The hostile marker stayed `original`, nested marker count was zero, external navigation was cancelled, the owned listener received zero document requests, and the digest-bound scheme asset loaded at `naturalWidth=128`. Evidence: `macos-webkit-isolation.json`.
- [x] AC4 — **Pass with intentional asset-policy limitation.** Valid card/columns PNGs and a non-stale viewport-change render passed. Literal content never invoked the worker. The card's unchanged external HTTPS emblem was not fetched because renderer documents have no direct web access. The trusted controller instead supplies verified/content-addressed asset bytes over the local `stcli-probe://` scheme. DejaVu Sans was substituted. Evidence: `macos-webkit-measurements.json` and rendered PNGs.
- [x] AC5 — **Pass for the accepted experiment scope.** Input/output errors and sub-750 ms SIGTERM passed. The measured aggregate footprint is accepted for this macOS helper; macOS resource use is measured rather than cgroup-enforced.
- [x] AC6 — **Measured; accepted.** Three cold paths and ten warm renders per fixture, protocol bytes, CPU counters, physical footprint, RSS, process count, threads, sampling method, and attribution caveats are recorded. Endpoint aggregate physical footprint was 418,108,048 bytes; RSS was 490,569,728 bytes; threads were 64. Evidence: `macos-webkit-measurements.json` and `terminal-transfer.json`.
- [x] AC7 — **Pass with non-TTY gap recorded.** Real Kitty exercised card, controls, popup, source, removal/restoration, literal, worker crash, graphics-off, missing-renderer, and exit cleanup. The non-TTY shell could not enable raw mode for an additional non-graphics run. Evidence: `reproduce.txt` and terminal captures.
- [x] AC8 — **Pass for the CLI launch model.** Direct terminal launch worked without an IDE and native snapshots needed no Screen Recording permission. Gatekeeper acceptance, Developer ID signing, and notarization are outside the terminal experiment's requirements. Only macOS 26.4.1 arm64 was exercised. Evidence: `macos-webkit-entitlements.json` and `reproduce.txt`.
- [x] AC9 — **Pass as an experiment; production integration remains separate.** `results.json` records `backend_approved=true`: footprint and direct CLI execution are accepted, residual `network.client` authority is documented, and renderer documents intentionally deny direct web fetches. ADR 0010's broker remains limited to Extension `fetch` and secondary inference; renderer assets use trusted-controller resolution of verified/content-addressed bytes over `stcli-probe://`. Text fallback remains the current default until production integration is separately selected.

#### documentation update

- [x] Documentation update: `docs/experiments/rich-content-renderer/macos-webkit/` contains results, reproduction ledger, entitlement/isolation/measurement JSON, fixture hashes, rendered PNGs, terminal captures, and transfer evidence. ADR 0012 records the clarified network/asset policy, accepted footprint, and CLI launch model without claiming shipped production availability.

#### test update

- [x] Test update: the real helper self-test, isolation/hostile runs, bounded-input measurements, real Kitty scenarios, focused example build, and workspace gates provide the requested proof. No generic framework or mock-only safety test was added.

## Handoff

Use a dedicated terminal/window and synthetic content; never inspect real secrets or change user security settings as a workaround. Ask only for prerequisites the executor cannot supply safely. Set this ticket to `ready-for-human` with per-criterion pass/fail/blocked evidence and a concrete recommendation. Completed evaluation is not synonymous with approved backend. Do not commit or push unless separately requested. Transfer this local/ignored `.scratch/` ticket to the Mac explicitly.

## Comments

Executed 2026-09-06 on macOS 26.4.1 (25E253), arm64, checkout `db14f10f9e9da18a676cdb361b56c79fcb26708a`, Rust 1.89.0, Swift 6.2.4 / SDK 26.2, Python 3.14.7, and Kitty 0.48.2. Pre-existing work was preserved and no commit or push was made.

Recommendation: treat the helper as a viable experimental macOS renderer. Its measured footprint is accepted, and direct execution of the ad-hoc-signed bundle Mach-O from the terminal is the supported experiment model. Keep the renderer's no-direct-web-fetch rule: ADR 0010 Brokered HTTPS Egress covers deliberate Extension `fetch` and secondary-inference/provider calls, not arbitrary HTML resources. The trusted controller should resolve verified/content-addressed assets and expose their bytes through the local `stcli-probe://` scheme. The unchanged HTTPS emblem is therefore an intentional fidelity limitation, not evidence that a required proxy path failed. Ticket 11/12 outcomes remain independent.
