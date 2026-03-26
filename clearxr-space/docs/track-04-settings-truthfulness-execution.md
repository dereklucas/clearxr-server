# Track 04 Execution: Settings Truthfulness

## Purpose

This is the working companion to [Track 04: Settings Truthfulness](C:/Apps/clearxr-server/clearxr-space/docs/track-04-settings-truthfulness.md).

Use this file for:

- settings audit progress
- visible-vs-real control tracking
- startup-restore evidence
- remaining truth gaps

## Current Status

- The settings surface has reportedly been reduced to a smaller real set of controls.
- Startup/default restoration needs explicit evidence, not just claims.
- This track is mostly about honesty, not feature count.

## Evidence And Code Touchpoints

- `src/config/**`
- `src/shell/mod.rs`
- `ui/settings.html`

## What Looks Done

- Settings scope appears smaller than before.
- The plan now explicitly treats fake settings as unacceptable.

## What Still Needs Proof

- Which settings are still visible today?
- Which of those apply immediately?
- Which apply on next launch?
- Which startup defaults are actually honored?

## Open Questions

- Is panel opacity truly restored and applied?
- Is startup view honored?
- Are any legacy controls still visible in HTML but ignored in Rust?

## Suggested Next Tasks

1. Add a visible-settings inventory:
   - setting name
   - visible?
   - backend effect?
   - immediate / next launch / not implemented
2. Add startup-restore evidence with file references.
3. Confirm that no leftover settings UI implies unsupported features.

## Exit Criteria

- Visible settings inventory is complete
- Every visible control has an explicit behavior class
- Startup/default restoration is evidenced
