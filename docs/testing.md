# Test strategy

This document defines how STcli is tested: the layers, where a new test belongs, the workstreams that close the current gaps, and how the suites keep future frontends (TUI, daemon, browser) honest without retesting the engine. It implements the evaluation strategy in [PRD §3](../PRD.md) and defends the invariants in [ADRs 0001–0005](adr/).

## 1. Principles

**Determinism is the test harness.** Canonical JSON identity (`identity.rs`), hash-pinned artifact revisions, a deterministic PRNG and clock, and a mock provider whose response is a pure function of the request (`fixture-response:<canonical request hash>`) mean a single hash comparison covers what would otherwise take dozens of field assertions. The highest-value layer is therefore not unit tests but replayable fixture corpora and projection-hash checks.

**Placement rule.** If the expected value comes from SillyTavern, it belongs in the compat corpus (L4/L5). If it comes from STcli's own design (ADRs, PRD), it belongs in Rust tests (L1–L3). This keeps `compat/fixtures/` a portable compatibility contract that future frontends — or other implementations — can run unchanged.

**Every ADR invariant has a named test.**

| ADR | Invariant | Gated by |
|---|---|---|
| 0001 | Trace is authority; projections rebuild exactly | Replay/rebuild tests in `turn_transactions.rs`, migration harness, e2e loop replay assertion, and STscript outcome/state tests in `stscript.rs` |
| 0002 | Revision identity includes source bytes; profile bounds compatibility claims | `artifact.rs` unit tests, `compat verify` corpus |
| 0003 | Plugins are pure Wasm declarative effects, digest-pinned | `stcli-core/tests/plugins.rs`, `stcli-cli/tests/plugins.rs`, `plugin-wasm` CI job |
| 0004 | Preset settings precedence; embedded scripts execute only under grants | Preset settings/grant tests in `turn_transactions.rs`, `regex_scripts.rs` |
| 0005 | Deletion is tombstones; compaction is reference-safe | Compaction tests in `turn_transactions.rs`, migration fixtures reproducing the 20a7fa1 ancestry shape |

## 2. The five layers

| Layer | Location | Scope | Conventions |
|---|---|---|---|
| **L1 — Inline unit** | `#[cfg(test)]` in `crates/stcli-core/src/*.rs` | Pure functions: macro parsing, canonical JSON and hash invariants, regex-script parsing, lore arithmetic, tokenizer counts. No SQLite; no tokio unless the unit under test is async. | `fn <behavior>_<condition>()`, e.g. `setvar_nested_braces_parse` |
| **L2 — Core integration** | `crates/stcli-core/tests/*.rs` | Engine flows through the public `stcli_core` API with a real SQLite store (tempdir) and real wasmtime: turns, capsules, compaction, migrations, provider failures, lore. | One file per subsystem: `turn_transactions.rs`, `plugins.rs`, `storage_migrations.rs`, `provider_failures.rs`, `capsule.rs`, `lore.rs` |
| **L3 — Binary e2e** | `crates/stcli-cli/tests/*.rs` via `CARGO_BIN_EXE_stcli` + `STCLI_HOME` tempdir | clap parsing, exit codes, JSON envelopes, JSONL event streams, multi-process behavior (`provider-test serve`, the regex worker). | One file per user-visible workflow: `cli_session_loop.rs`, `protocol_contracts.rs`, `protocol_samples.rs` |
| **L4 — Compat corpus** | `compat/fixtures/*.json`, run by `stcli compat verify` | Behavior claimed by [`compat/profiles/sillytavern-1.18-core.json`](../compat/profiles/sillytavern-1.18-core.json): per-macro, per-preset-field, lore evaluation, hard-unsupported rejections. | One suite file per manifest area; case ids `macro.<name>.<variant>`, `preset.<field>.<variant>`, `lore.<feature>.<variant>` |
| **L5 — Oracle parity** | `provider-request-parity` cases in `phase4-preset-parity.json` | Parity against Nanobear v2.1, a redistributable third-party complex preset, and its recorded provider-request transcript; both are digest-pinned and committed in-repo. | Pinned expectations stay exact across normal, continue, regenerate, and swipe requests. |

Where a new test goes, in decision order (mirrors ARCHITECTURE.md "where to add new code"):

1. New compatibility behavior → an L4 fixture first; a Rust test only for STcli-design aspects of it (grants, trace events, errors).
2. New engine behavior touching the store, plugins, or the provider → L2, in the subsystem's file.
3. New CLI surface or envelope/event shape → L3 (`protocol_contracts.rs` for shape, workflow file for semantics).
4. A pure function's edge cases → L1, next to the code.

## 3. Workstreams

Priority order. Effort estimates assume the testkit (B) exists for everything after it.

### A. Oracle files in-repo and strict verification

The four `provider-request-parity` cases use the CC BY 4.0-licensed Nanobear v2.1 preset and a recorded transcript tied to the pinned preset revision and SillyTavern 1.18.0 compatibility reference. Both files are committed under `compat/external/`; verification never depends on separately acquired copyrighted inputs.

- The suite pins the preset and transcript SHA-256 digests. Each source records its upstream repository, exact revision, license, and source paths.
- `path_environment` remains an optional local override for re-recording. `repository_path` is the default and resolves from the workspace root.
- `FixtureReport::is_success` fails when `not_run > 0`.
- `checked_in_oracle_matches_all_dry_run_generation_types` runs in the normal test suite.

Acceptance: `cargo test` and `compat verify` both fail if an oracle file is missing or its digest drifts.

### B. Shared test-support crate: `crates/stcli-testkit`

The `const CARD` literal and `configuration()` builder are duplicated verbatim across four test files, and env-var mutation is serialized by three separate per-file static mutexes. Kill the duplication before the e2e and corpus workstreams multiply it.

New non-published crate (`publish = false`), dev-dependency of both crates:

- `fixtures` — canonical cards/presets/lorebooks loaded from [`examples/`](../examples/README.md) (which makes the documented examples load-bearing for the first time), plus the existing minimal card for tests that need a tiny one; the `configuration()` builder migrates here.
- `TestHome` — tempdir + `STCLI_HOME`. For L3, env is passed explicitly on `Command` (no `set_var`). For the few L2 tests that must mutate process env, one process-global panic-safe mutex replaces the per-file statics.
- `mock_provider` — spawn / await-ready / shutdown helpers wrapping the in-process axum router (L2) and the `provider-test serve` subprocess (L3).
- `stcli_cmd(&TestHome) -> Command` — wraps `CARGO_BIN_EXE_stcli` and sets `STCLI_REGEX_WORKER`, so library-level tests can drive the real regex worker.

No new dependencies. Deliberately no `assert_cmd`: the wrapper delivers the same ergonomics in ~30 lines. Effort: ~1 day, including migrating the four existing helper blocks.

#### Pinned real-world Extension fixtures

The L2 `st_bridge` target uses two compact fixtures under
`crates/stcli-core/tests/fixtures/real_extensions/`:

- `metamorph-lifecycle` is derived from `dajected/metamorph`. It retains native manifest,
  lifecycle, prompt, settings, asynchronous Secondary Inference, and headless-degradation
  patterns.
- `request-monitor-wire` is derived from
  `haveagoodday1205-png/st-request-monitor`. It retains fetch, settings, localStorage, and
  response-handling patterns and adds the supported `$.ajax` and slash-command forms.

Both fixtures are modified/derived MIT-licensed test inputs, not upstream copies. Imports, panel
rendering, styles, and unrelated browser behavior are excluded. Each fixture's `provenance.json`
records the repository URL, full commit SHA, upstream paths, SPDX license, derivation notes,
update instructions, and the SHA-256 digest of every other committed file in its directory.
`real_extension_fixture_provenance` reads only committed files and fails with the affected path
when a file is missing, unlisted, or changed.

Fixture updates are explicit reviewed changes:

1. Open every recorded upstream path at the recorded full commit on GitHub. If moving the pin,
   review the upstream commit and license before copying any pattern.
2. Re-derive only the public API and control-flow patterns named in `provenance.json`. Do not
   restore UI-only code or module imports.
3. Run `node --check` for both `index.js` files.
4. Recalculate SHA-256 for every non-sidecar file and update the sidecar in the same change.
5. Run
   `cargo test -p stcli-core --test st_bridge real_extension_fixture_provenance --locked`.

Tests never download or refresh these fixtures.

### C. Protocol and schema conformance — the frontend seam

The schemas in [`schemas/`](../schemas/) plus `wit/plugin.wit` are the public protocol every future UI binds to (PRD Success Criterion 8). Today nothing validates real binary output against them.

`crates/stcli-cli/tests/protocol_contracts.rs` (schema and shape validation):

- An invocation table — `artifact import`, `session create`, `message send`, `turn run` (against the mock provider), `turn dry-run`, `compat verify`, `plugin list`, plus one deliberately invalid invocation per envelope family. Each entry: run the binary, parse stdout, validate against `cli-envelope.schema.json` with the existing `jsonschema` dev-dependency.
- Stream a full turn; validate every JSONL line against `cli-event.schema.json`; assert ordering invariants: start precedes deltas precedes terminal, exactly one terminal event, nothing after it.
- Export a capsule from a completed turn and validate it against `turn-capsule.schema.json`. Validate `plugins/proof/manifest.json` against `plugin-manifest.schema.json`.
- A schema-version guard asserting each schema file's `$id`, so any schema change is a conscious, reviewable diff.

Golden samples in `compat/protocol-samples/` (`protocol_samples.rs`): one canonical envelope, event stream, and capsule per kind, generated from the real binary — byte-exact thanks to canonical JSON. A test regenerates and byte-compares; intentional changes regenerate via a documented env flag. These files are what a TUI or browser test suite consumes offline, without running the engine. No snapshot library is needed or wanted (see §5).

Effort: 1–2 days.

### D. Binary e2e loop suite

About 50 subcommands exist; only `plugin` and the regex tests exercise the real binary. Cover the surface along real user paths, not per-subcommand.

`crates/stcli-cli/tests/cli_session_loop.rs` — one long happy path: import `examples/character.json` + `lorebook.json` + `preset.json` → create session → send → swipe → regenerate → continue → branch → send on branch → compact → export capsule → offline replay → **assert projection-hash equality before and after replay** (ADR 0001 as an e2e assertion). The deterministic mock provider makes exact response text assertable at every step.

Targeted scenarios as separate tests on the same pattern:

- **Branching**: create a branch mid-history, send on both branches, verify the fork point is shared and the divergent turns are not; select candidates per branch.
- **Deletion** (ADR 0005, and the known-partial in-place deletion area of issues #17/#18): delete a message, a candidate, and a branch via tombstones; verify projections hide them, the trace retains them, replay still works, and a subsequent `session compact` reaps only what nothing references — including the fork-point case that compaction must never reap.
- Archive/purge with shared revisions, artifact attach/list, prompt inspect and dry-run.

All assertions are on exit codes and parsed JSON envelopes — never raw stdout strings — so this suite doubles as protocol regression coverage. Explicit non-goal: a per-flag matrix; the loop covers semantics, §C covers shapes. Effort: 2–3 days.

### E. Compat corpus expansion behind a coverage ratchet

The corpus is thin against the PRD's own requirement of positive, negative, boundary, ordering, and failure cases per manifest feature: 6 macro-render cases vs 73 exact macros, 2 lore cases for the 864-line `lore.rs`, 4 preset-outcome cases vs 53 classified fields. Do not hand-write 300 cases up front — make completeness a CI-enforced ratchet and fill incrementally.

**Ratchet test** (`crates/stcli-core/tests/compat_coverage.rs`): load the profile and every suite; fail listing any of the following with zero fixture cases, seeded with an allowlist of known-missing entries that may only shrink:

- Each of the **73 `macros.exact`** names — target ≥3 variants each: plain render, a nested/argument edge, and a state-interaction or boundary case (e.g. `setvar`/`getvar` ordering, `random`/`pick`/`roll` under the deterministic PRNG, `date`/`time`/`timeDiff` under the pinned clock, `outlet`/`original` in preset context).
- Each of the **5 `macros.hard_unsupported`** (`input`, `banned`, `notChar`, `isMobile`, `systemPrompt`) — one rejection/diagnostic case each.
- Each of the **53 `preset_fields`** — at least one case proving its classification: `assembly-behavior` fields change the provider request, `provider-behavior` fields pass through to settings, `documented-fallback` fields produce the documented warning, `preserved-metadata` fields round-trip untouched, `hard-unsupported` fields produce the documented outcome.
- Each of the **8 `scope.hard_unsupported`** entries — one negative case each.
- The lore boundary checklist below.

**Suite density**: extend the self-owned fixture-suite schema (version bump to `stcli.fixture-suite/v2`) with a compact table form for `macro-render` — shared `context`/`initial_*` defaults per group, per-case `{id, input, expected_text, expected_local?, expected_global?, expected_warnings?}` — so 73×3 cases stay reviewable. Extend the runner in `crates/stcli-core/src/fixture.rs` accordingly.

**Lore boundaries** (new `phase3-lore` cases): recursion depth at and over the limit, budget exactly-at and one-over, key matching (case sensitivity, whole-word, regex keys), `selective` secondary-key logic, insertion-order ties and position classes, scan-depth window edges, disabled entries, probability 0 and 100 under the deterministic PRNG.

**Regex scripts**: L4 cases for placement classes and `substituteRegex` modes where ST 1.18 behavior is documented at the pinned commit; grant-gating itself stays L2 (it is STcli design, ADR 0004).

Expected values come from SillyTavern 1.18 documented behavior at the pinned commit (`51ad27fb`), with the profile's `source_files` as the reference per macro group. Anything only observable through the oracle is marked oracle-tier (L5). Effort: the largest workstream, 4–6 days spread out behind the ratchet.

### F. Storage migration harness

The most recent migration bug (`20a7fa1`, candidate ancestry lost during migration) shipped with no harness to catch it. The trace-is-authority ADR provides a perfect equivalence check. Implemented in `crates/stcli-core/tests/storage_migrations.rs`.

- Fixtures: `crates/stcli-core/tests/fixtures/db/v{5,6,7}.sql` — **SQL dumps, not binary databases** (diffable, loadable via `rusqlite` batch execute). Each dump is a small populated store carrying the same authoritative trace: a session with several turns/attempts/candidates and a fork branch (`forked_from_turn_id`), including the ancestry shape from 20a7fa1 — a generated candidate with a `parent_candidate_id`. In the v6 dump that candidate sits under the old `attempt_id NOT NULL` candidates shape that forces the migration table-rebuild (the path 20a7fa1 broke); v7 carries the same ancestry under the nullable-`attempt_id` shape, which the migration leaves in place. `expected.json` records each fixture's rebuilt projection hash and ancestry.
- The harness:
  1. Load each dump → open through the engine → assert migration to the current `SCHEMA_VERSION` → assert the migration alone (before any rebuild) preserved the candidate ancestry → rebuild projections from the trace → compare against the recorded projection hash.
  2. Downgrade refusal: a store whose recorded schema version is newer than the binary supports fails cleanly with `StorageError::SchemaTooNew`.
  3. Idempotence: opening (migrating) an already-migrated store and rebuilding again leaves the hash unchanged. All three fixtures rebuild to one canonical projection.
- Process rule, enforced in-test: fail if `SCHEMA_VERSION > max fixture version + 1`. Every schema bump must add a dump for the previous version.

The fixtures and manifest are regenerated from the current engine (a canonical session built through the public API, then dumped at each historical schema shape). There is no prior release binary to trim; regenerate with `STCLI_REGENERATE_DB_FIXTURES=1 cargo test -p stcli-core --test storage_migrations`, review the SQL and hash diffs, then commit.

### G. Provider failure modes

PRD §3 explicitly lists non-2xx bodies, malformed responses, malformed streams, timeouts, and connection failures; today only one split-SSE cancellation case exists.

- Extend the shipped mock (`crates/stcli-cli/src/provider_test.rs`) in its existing request-driven knob style (`fixture_delay_ms`, `fixture_echo_header`), keeping the mock a pure function of the request: `fixture_status` (429 with and without `Retry-After`, 500, 503, non-JSON body), `fixture_sse_malformed` (`bad-json` | `missing-done` | `truncate-mid-event`), `fixture_disconnect_after_chunks: N`.
- L2 (`crates/stcli-core/tests/provider_failures.rs`): attempt status recorded in the trace per failure class; no partial-turn corruption — the projection hash is unchanged after a failed turn; the timeout path; no automatic retry (the profile declares retries hard-unsupported by design).
- L3: two or three cases asserting the error envelope and event shape per failure class (feeds §C).

Decision recorded here: **`provider-test serve` stays in the release binary.** It is load-bearing for users, for the fixture runner, and for future frontend e2e. Note it in ARCHITECTURE.md rather than feature-gating it — feature-gated test infrastructure rots. Effort: 1–2 days.

### H. Property-based tests

`proptest` is the **single new test dependency in this entire strategy**. Justification: these are parser/serializer surfaces consuming untrusted wild content (cards, presets, regex scripts), fuzz-shaped bugs are exactly what example-based tests miss, and proptest's shrinking makes failures actionable. Cap runtime with `PROPTEST_CASES=64` in CI. No `cargo-fuzz`: nightly plus corpus infrastructure for perhaps 20% more value.

Targets, and only these until one of them pays off:

1. `identity.rs` — canonical-JSON fixpoint (serialize → parse → serialize), hash stability under key reordering, domain separation.
2. `macros.rs` — arbitrary input never panics; balanced-brace inputs round-trip under an identity environment.
3. `regex_script.rs` / `ecma_regex.rs` — arbitrary patterns never panic the host; worker-crash containment; valid scripts round-trip.
4. `lore.rs` — for arbitrary entry sets, the selected set never exceeds the budget and selection is order-stable.

Effort: 1–2 days.

### I. CI evolution

Target layout for `.github/workflows/ci.yml`:

```yaml
lint:          # ubuntu: fmt --check, clippy -D warnings, cargo-deny (advisories + licenses + bans)
test-linux:    # ubuntu: cargo test --workspace --locked, compat verify (strict after A)
plugin-wasm:   # rebuild plugins/proof from source (wasm32-unknown-unknown + wasm-tools
               # component new), hash-compare against the checked-in component.wasm;
               # paths-filtered to plugins/proof/** + weekly cron
coverage:      # main + weekly cron: cargo-llvm-cov --workspace --lcov,
               # upload artifact + PR summary; no threshold gate
live-smoke:    # weekly cron + workflow_dispatch only, secret-gated, non-required:
               # 1-2 turn live roleplay against a cheap real model (workstream J)
```

- **Caching**: `Swatinem/rust-cache@v2` in every job, keyed on `Cargo.lock` and the toolchain. The biggest wall-clock win — wasmtime and bundled-SQLite currently rebuild from scratch on every run.
- **`cargo-deny`** in `lint`: an AGPL project on top of the wasmtime/axum/reqwest trees has real license- and advisory-drift risk, and the cost is one `deny.toml`. No separate `cargo-audit` (a subset of deny).
- **Coverage is informational**: collect and display, never gate. On a codebase whose strongest checks are hash comparisons, a percentage threshold produces noise and gaming, not quality. Use the report to find under-tested modules (`capsule.rs` and `lore.rs` are the current standouts).
- **No nextest**: per-test process isolation would *mask* the env-var mutation problem that workstream B fixes properly, and `cargo test` parallelism is adequate at this scale. Revisit if suite wall time exceeds ~5 minutes post-caching.
- **Windows is a pre-v1.0 release gate**, not a current job. When it lands: `windows-latest`, same commands, non-required until green for a week. Expected breakage points: the bundled-SQLite build, subprocess regex-worker paths, and path/permission semantics per the PRD risk table.
- **Plugin reproducibility recipe** (`plugin-wasm`, lives in its own `plugin.yml` because native path filtering is per-workflow, keeping it non-required): pinned `rustc 1.89.0` builds the core module for `wasm32-unknown-unknown`, then pinned `wasm-tools 1.236.1` (`wit-component 0.227.1`) runs `component new`. This reproduces the checked-in `component.wasm` byte-for-byte; `wasm32-wasip2` is wrong here because it links WASI imports the closed `plugin` world (export-only) can never satisfy. Bump the checked-in component and `manifest.json` `component_sha256` together whenever the source or either pinned version changes.
- Weekly `schedule:` cron on `test-linux`, `plugin-wasm`, `coverage`, and `live-smoke` to catch toolchain, dependency, and provider drift between pushes.

### J. Live-provider smoke test (opt-in)

The one deliberate exception to "determinism is the harness". Everything else in this strategy runs against the mock, which by construction cannot catch real-world provider behavior: actual TLS stacks, provider-specific SSE framing quirks, auth handling, rate-limit responses from a real endpoint.

- `crates/stcli-cli/tests/live_smoke.rs`, self-skipping unless `STCLI_LIVE_BASE_URL` / `STCLI_LIVE_API_KEY` / `STCLI_LIVE_MODEL` are set (point them at any cheap, fast model — the test is provider-agnostic and must not hardcode a vendor).
- One short roleplay: import the example card → create session → send → assert a non-empty streamed candidate → swipe or continue → assert the second turn. **Assert structure only, never content**: the SSE stream completes with a terminal event, candidate text is non-empty, usage and attempt status land in the trace, projection rebuild still matches — a live model's output is non-deterministic, so content assertions would only produce flakes.
- CI: the `live-smoke` job runs on the weekly cron and `workflow_dispatch` only — never on PRs (secret exposure, cost, flake blast-radius). A repository secret holds the key; the job is non-required.
- Budget guard: cap `max_tokens` low in the pinned settings so a run costs fractions of a cent.

Effort: ~½ day once D exists (it reuses the loop suite's helpers with the mock swapped for the live endpoint).

## 4. Roadmap-proofing: future frontends

The v0.2 TUI and v1.x daemon/browser frontends bind to exactly the surface §C pins:

- **Schemas are the API.** §C proves the engine's real output conforms; `compat/protocol-samples/` gives frontends byte-exact fixtures to develop against offline; integration runs go against `stcli --output json` or, later, the daemon socket. Engine behavior — macros, lore, turns — is never retested per-frontend: frontends test rendering and input only, trusting L2–L5 here.
- **The shipped mock provider is the frontend test backend too.** A browser e2e run (e.g. Playwright) drives frontend → daemon → engine → `provider-test serve`; the whole stack is deterministic, so frontend e2e can assert exact rendered text. This is the payoff of keeping the mock in the release binary.
- **Versioning discipline.** The schema `$id` guard and the golden samples mean any protocol change breaks this repository's CI first, forcing a conscious version bump that frontends can gate on. When the daemon lands, only transport-level tests (socket framing, concurrent clients) are new; every payload-shape test already exists.

## 5. What we deliberately do not do

- **No `insta` / snapshot library.** Canonical hashing plus checked-in goldens are snapshots with better provenance and no review-tool dependency. Snapshotting free-text output would freeze incidental formatting.
- **No `assert_cmd`, `predicates`, `wiremock`, or `rstest`.** Each is replaced by ≤30 lines in the testkit, the shipped mock provider, or a plain table respectively.
- **No `criterion` or benchmarks** until a latency SLO exists (the daemon era).
- **No per-subcommand flag matrix.** The loop suite covers semantics; the protocol suite covers shapes; clap parsing errors are cheap to spot.
- **No coverage threshold.**
- **No mocking of SQLite or wasmtime.** Tempdir plus real dependencies are fast enough, and the ADR invariants only mean anything against the real store.
- **`provider-test` stays in the binary** (see workstream G).

## 6. Traceability: PRD success criteria → suites

| # | PRD success criterion | Gated by |
|---|---|---|
| 1 | Usable end-to-end loop | D (e2e loop suite) |
| 2 | 100% profile parity fixtures pass | L4 corpus + E ratchet, strict `compat verify` in CI |
| 3 | Byte-identical artifact re-export | Existing `artifact.rs` revision tests + D |
| 4 | Deterministic replay to identical projection hash | D replay assertion + F migration harness |
| 5 | Pure-plugin proof | Existing plugin suites + `plugin-wasm` CI job |
| 6 | Secret exclusion | Existing secret-redaction tests + G error paths |
| 7 | Crash consistency | Existing storage recovery tests + F |
| 8 | Protocol stability | C conformance suite + golden samples + schema `$id` guard |

## 7. Phasing

| Phase | Contents | Rationale |
|---|---|---|
| **P1** | A (oracle in-repo + strict) + B (testkit) + CI caching/job-split/cargo-deny | Kills the false-green, unblocks everything else, makes iteration cheap |
| **P2** | C (protocol suite + goldens) + F (migration harness) | The roadmap seam is cheap and foundational; F guards the bug class with proven history and user-data stakes |
| **P3** | D (e2e loop) + G (provider failures) | Share the testkit/mock work; cover the untested binary surface and the PRD error paths |
| **P4** | E (ratchet lands first; corpus fills incrementally) | Largest effort, de-risked by the ratchet |
| **P5** | H (proptest) + J (live smoke) + coverage job + plugin-wasm job + cron wiring; Windows as a pre-v1.0 gate | Hardening, no urgency |

Net new test dependencies across the whole strategy: **`proptest`**. New CI tooling: `Swatinem/rust-cache`, `cargo-deny`, `cargo-llvm-cov`, and the wasm target for the rebuild job.

## 8. Conventions appendix

- **Naming**: L1 `fn <behavior>_<condition>()`; L2 file = subsystem, test = scenario; L3 file = workflow; L4 case ids `<area>.<name>.<variant>`.
- **New test placement**: follow the decision list at the end of §2.
- **Re-recording oracle expectations**: set `STCLI_NANOBEAR_PRESET` and `STCLI_NANOBEAR_ORACLE` to replacement source files, update the pinned counts and SHA-256 digests in `phase4-preset-parity.json`, replace the files under `compat/external/`, and record the upstream source and revision in both the suite and transcript.
- **Regenerating protocol goldens**: set `STCLI_REGENERATE_PROTOCOL_SAMPLES=1` and run `cargo test -p stcli-cli --test protocol_samples`; review the byte diff like any API change; bump the affected schema `$id` if the shape changed.
- **Adding a storage schema version**: after bumping `SCHEMA_VERSION` and writing the migration, add the previous version to `FIXTURE_VERSIONS` in `storage_migrations.rs`, teach `historical_ddl`/`populated_tables` its column shape, then regenerate with `STCLI_REGENERATE_DB_FIXTURES=1 cargo test -p stcli-core --test storage_migrations`, review the SQL and hash diffs, and commit the updated `tests/fixtures/db/` — the ratchet fails the build otherwise.
