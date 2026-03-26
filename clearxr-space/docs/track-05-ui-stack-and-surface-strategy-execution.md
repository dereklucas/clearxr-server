# Track 05 Execution: UI Stack And Surface Strategy

## Purpose

This is the working companion to [Track 05: UI Stack And Surface Strategy](C:/Apps/clearxr-server/clearxr-space/docs/track-05-ui-stack-and-surface-strategy.md).

Use this file for:

- migration status
- prototype spikes
- render-path decisions
- dependency and integration notes

## Current Status

- The current strategy has moved beyond "keep Ultralight for MVP."
- The active direction is toward `egui`.
- The planning doc should remain the decision source; this file should track execution evidence and migration work.

## Evidence And Code Touchpoints

- `docs/track-05-ui-stack-and-surface-strategy.md`
- `src/ui/**`
- `src/launcher_panel.rs`
- `src/shell/mod.rs`
- `Cargo.toml`

## What Looks Decided

- `Slint` is out.
- `egui` is the primary native UI candidate.
- The team is comfortable reducing reliance on Ultralight.

## What Still Needs Proof

- What exact rendering path will be used for `egui` inside the current Vulkan/panel pipeline?
- Which surfaces migrate first?
- Which Ultralight surfaces remain during transition?
- What third-party `egui` component/theme crates are actually acceptable?

## Open Questions

- Will `egui` render directly into Vulkan textures or through a software raster path first?
- Do we want plain `egui` first, or plain `egui` plus a component/theme crate?
- Is the launcher grid in phase 1, or only after simpler surfaces migrate?

## Suggested Next Tasks

1. Add a migration order table:
   - toolbar
   - notifications
   - settings
   - tray
   - keyboard
   - launcher
2. Add a dependency decision table:
   - core `egui`
   - `egui_extras`
   - optional component crates
3. Add a render-path note:
   - direct Vulkan path
   - software raster fallback
   - pros/cons for each
4. Add a rollback rule:
   - what would cause us to pause or reverse the migration?

## Exit Criteria

- Migration order is explicit
- `egui` integration path is explicit
- Surface-by-surface transition status is tracked
- Any remaining Ultralight dependency is intentional and documented
