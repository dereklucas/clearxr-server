# Track 03 Execution: Launcher And App Model

## Purpose

This is the working companion to [Track 03: Launcher And App Model](C:/Apps/clearxr-server/clearxr-space/docs/track-03-launcher-and-app-model.md).

Use this file for:

- launcher status
- metadata-quality findings
- launch-flow evidence
- open product/data questions

## Current Status

- Launcher remains the hero product surface.
- Search and basic usefulness appear implemented.
- Taxonomy honesty and launch feedback still need explicit evidence and quality review.

## Evidence And Code Touchpoints

- `src/app/**`
  - game scanning and app launch behavior
- `src/shell/mod.rs`
  - shell-side launch orchestration and notifications
- `ui/launcher-v2.html`
  - launcher presentation and category behavior

## What Looks Done

- Search exists.
- Launcher grid exists.
- Launch path exists.

## What Still Needs Proof

- Which categories are truly honest now?
- Is "recent" implemented, removed, or still cosmetic?
- Is VR classification still heuristic?
- Does launch feedback fully cover success/failure/replacement state?

## Open Questions

- What is the exact current launcher taxonomy?
- What is the minimum credible metadata model for MVP?
- Are we still showing any category or badge that looks more authoritative than the data actually is?

## Suggested Next Tasks

1. Add a launcher-audit section:
   - shown categories
   - source of truth for each
   - honest / heuristic / stubbed
2. Add explicit launch-flow evidence:
   - where success is shown
   - where failure is shown
   - how replacement of prior app is communicated
3. Document the current sorting model and whether it is intentional.

## Exit Criteria

- Launcher categories are documented and honest
- Launch feedback is evidenced end-to-end
- Remaining metadata gaps are explicitly listed
