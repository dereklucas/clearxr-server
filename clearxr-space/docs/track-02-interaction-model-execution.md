# Track 02 Execution: Interaction Model

## Purpose

This is the working companion to [Track 02: Interaction Model](C:/Apps/clearxr-server/clearxr-space/docs/track-02-interaction-model.md).

Use this file for:

- implementation status
- evidence for "done" claims
- unresolved interaction inconsistencies
- follow-up tasks

## Current Status

- Repeated-click behavior appears to have been addressed.
- The desktop panel remains a normal shell surface, not a separately gated mode.
- Shared interaction semantics are the target, but full consolidation onto one event model still needs proof.

## Evidence And Code Touchpoints

- `src/shell/mod.rs`
  - per-surface interaction behavior
- `src/input/mod.rs`
  - structured input model and event abstractions
- `src/panel/mod.rs`
  - panel hit testing and transforms

## What Looks Done

- One press equals one action appears to be implemented.
- Toolbar switching appears to be protected from repeat-fire.
- The planning docs now treat the desktop panel as a regular shell surface.

## What Still Needs Proof

- Are all surfaces truly using the same interaction pipeline, or just equivalent edge handling?
- Is desktop panel interaction fully aligned with launcher/settings/tray semantics?
- Are drag/move behaviors covered by tests or only manually validated?

## Open Questions

- Is the `InputDispatcher` now authoritative, or is `Shell::tick` still the true owner of interaction behavior?
- Which surfaces still use bespoke logic?
- Should the hand-tracking/gaze proposal move into a separate future-design note?

## Suggested Next Tasks

1. Add explicit evidence links for the "done" items in the planning doc.
2. Document which surfaces still bypass the structured input path.
3. Add a tiny interaction audit table:
   - launcher
   - desktop
   - settings
   - tray
   - keyboard
   - notifications
4. Decide whether the gaze-and-pinch proposal is approved roadmap or just a possible 1.0 design path.

## Exit Criteria

- Every interactive surface has the same click semantics
- Any remaining bespoke interaction logic is documented
- Desktop panel behavior is confirmed to follow the shared model
- "Done" claims have evidence attached
