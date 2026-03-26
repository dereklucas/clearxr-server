# clearxr-space Docs Index

ClearXR Space is an XR launcher with a light desktop companion, built on OpenXR and Vulkan.

This folder is split into one strategy document and several execution-track documents so multiple agents can work in parallel with less merge friction.

## Core Docs

- [Master Roadmap](C:/Apps/clearxr-server/clearxr-space/docs/clearxr-space-product-review-and-roadmap.md)
  - the main product critique, roadmap, and long-range recommendation set

- [Track 01: Runtime Foundation](C:/Apps/clearxr-server/clearxr-space/docs/track-01-runtime-foundation.md)
  - Vulkan, OpenXR, preflight, diagnostics, and runtime hardening
- [Track 01 Execution](C:/Apps/clearxr-server/clearxr-space/docs/track-01-runtime-foundation-execution.md)
  - working notes, evidence, audits, and next steps

- [Track 02: Interaction Model](C:/Apps/clearxr-server/clearxr-space/docs/track-02-interaction-model.md)
  - click semantics, unified input pipeline, panel interaction
- [Track 02 Execution](C:/Apps/clearxr-server/clearxr-space/docs/track-02-interaction-model-execution.md)
  - implementation evidence, interaction audit, and unresolved edge cases

- [Track 03: Launcher And App Model](C:/Apps/clearxr-server/clearxr-space/docs/track-03-launcher-and-app-model.md)
  - launcher scope, game metadata, launch lifecycle, content usefulness
- [Track 03 Execution](C:/Apps/clearxr-server/clearxr-space/docs/track-03-launcher-and-app-model-execution.md)
  - launcher audit, metadata truthfulness, and launch-flow evidence

- [Track 04: Settings Truthfulness](C:/Apps/clearxr-server/clearxr-space/docs/track-04-settings-truthfulness.md)
  - config truthfulness, startup restore, settings reduction, UX honesty
- [Track 04 Execution](C:/Apps/clearxr-server/clearxr-space/docs/track-04-settings-truthfulness-execution.md)
  - visible-settings inventory, startup-restore evidence, and remaining truth gaps

- [Track 05: UI Stack And Surface Strategy](C:/Apps/clearxr-server/clearxr-space/docs/track-05-ui-stack-and-surface-strategy.md)
  - Ultralight vs native UI, productionization, migration options, shell chrome direction
- [Track 05 Execution](C:/Apps/clearxr-server/clearxr-space/docs/track-05-ui-stack-and-surface-strategy-execution.md)
  - migration status, integration notes, and surface-by-surface transition work

- [Track 06: Product Framing And Acceptance](C:/Apps/clearxr-server/clearxr-space/docs/track-06-product-framing-and-acceptance.md)
  - MVP framing, cuts, acceptance criteria, product language, release gates
- [Track 06 Execution](C:/Apps/clearxr-server/clearxr-space/docs/track-06-product-framing-and-acceptance-execution.md)
  - framing audit, scope-cut log, and release-readiness evidence

## Working Rules

- The master roadmap is the source of truth for overall direction.
- Track docs are the source of truth for execution within each lane.
- If a track decision changes product scope, update the master roadmap too.
- If two tracks conflict, resolve it in the master roadmap, not just in a track doc.

## Current Decisions

- The old `--desktop` winit-based preview runtime is removed.
- The in-shell desktop viewer is still part of product scope.
- Desktop-control safety gating is not currently a product requirement.
- Ultralight was useful for exploration, but the current UI strategy is moving toward `egui`.
- `Slint` is out.
- `egui` is the primary native UI candidate for shell-critical surfaces.

## Suggested Parallel Ownership

- Track 01 can proceed immediately on correctness and preflight work.
- Track 02 can proceed immediately on input and panel event cleanup.
- Track 03 can proceed on launcher truthfulness and launch lifecycle.
- Track 04 can proceed on settings reduction and startup restore.
- Track 05 can proceed in docs/design mode first, then implementation later.
- Track 06 can proceed immediately and should stay aligned with any scope cuts.

