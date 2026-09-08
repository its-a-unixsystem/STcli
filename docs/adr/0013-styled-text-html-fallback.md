# Candidate content renders as styled terminal text through a two-stage HTML pipeline

## Status

accepted

## Context

The TUI must display all Candidate content through one terminal-native presentation path. Real Candidate content, after Core applies display-eligible regex scripts (`apply_display_scripts`), is Markdown prose (`*emphasis*`, quotes) intermixed with regex-generated and model-generated HTML carrying inline CSS: `<details>/<summary>` trees, `<div style>`, `<font color>`, `<b style>`.

The prior production path (`crates/stcli-tui/src/markdown.rs`) rendered this with pulldown-cmark and a naive `strip_tags` that removed only `<...>` delimiters. That path leaked `<script>`/`<style>` body text and terminal-control characters into the terminal, and lost all HTML structure. Ticket 14 research confirmed pulldown-cmark is not an HTML parser or sanitizer, and that stdlib subprocess parsing is not HTML5-compliant. Ticket 14's original "supplied-Markdown-first, no HTML parser" recommendation was superseded once the real content shape (HTML-with-inline-CSS) was established: automatic HTML rendering is required, not optional.

## Decision

Render Candidate content to styled terminal text with a **two-stage pipeline that mirrors SillyTavern's browser rendering** (marked.js → DOM), preserving Compatibility Profile Parity:

1. **Stage 1 — Markdown → HTML.** `pulldown_cmark::html::push_html` with GFM extensions (strikethrough, tables, autolinks), matching marked.js. Raw HTML passes through. The crate is repurposed, not dropped.
2. **Stage 2 — HTML → terminal spans.** A `scraper`/`html5ever` DOM walk emits Ratatui spans, applying Concealed Content suppression and inline-CSS color.

`markdown.rs`'s span-emitting body and `strip_tags` are replaced by a single `content.rs` converter. It is the sole audited Candidate HTML-to-terminal-text path.

Boundaries and policy:

- **Dependency scope.** `scraper` is added to `stcli-tui` only. `stcli-core` stays free of HTML/CSS/rendering dependencies, consistent with ADR 0008.
- **Concealed Content (trust boundary).** Stage 2 suppresses rather than extracts: `<script>`/`<style>`/comment nodes; `hidden`/`aria-hidden`; inline `display:none`/`visibility:hidden`; and foreground==background color (exact RGB equality, both declared inline on the same element). CSS-selector-based hiding from `<style>` blocks is a documented follow-up, not in this decision.
- **Control-character safety.** Content-originated C0/C1 control bytes and ANSI/OSC escape sequences are stripped before any span is emitted. This is a hard requirement, not best-effort.
- **Bounds.** Sources above 256 KiB or DOM nesting deeper than 128 elements (excluding the fragment root) fall back to control-neutralized literal source, not partially converted HTML. Tags remain visible in this fallback; tabs become spaces and newlines remain line boundaries. Both limits apply per `render()` call, leaving the surrounding TUI usable.
- **Color mode.** A `preserve | semantic | contrast` mode enum is defined; only `preserve` is implemented, and it is the default. The other modes are follow-up tasks.
- **`<details>` divergence.** A browser renders `<details>` collapsed by default; terminal text cannot collapse it, so summaries render as headings with bodies always shown flattened. This is a deliberate, documented lossy-layout item; interactive collapse is separate future work.

## Consequences

- `scraper` adds 24 net-new transitive crates (Servo-project: `html5ever`, `markup5ever`, `tendril`, `selectors`, `cssparser`, `ego-tree`, and plumbing), in-process, no subprocess. This is the accepted floor for parsing real AI HTML with a defensible Concealed Content boundary.
- The two-stage topology is the only shape that stays in Parity with ST's marked.js + browser rendering. Parsing Candidate content as HTML-first would diverge from marked.js emphasis and block-interruption semantics.
- ADR 0012's retired graphical renderer experiment now points here as the sole styled/structured Candidate presentation path.
