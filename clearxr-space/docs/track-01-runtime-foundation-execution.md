# Track 01 Execution: Runtime Foundation

## Purpose

This is the working companion to [Track 01: Runtime Foundation](C:/Apps/clearxr-server/clearxr-space/docs/track-01-runtime-foundation.md).

Use this file for:

- current implementation status
- evidence and code references
- open questions
- next steps

Do not rewrite product scope here. Scope belongs in the planning doc.

## Current Status

- XR is the only runtime mode.
- The old `--desktop` preview runtime is removed.
- The in-shell desktop viewer remains part of product scope.
- Environment preflight appears partially addressed.
- Mirror-path and queue-family robustness still need explicit verification.

## Evidence And Code Touchpoints

- `src/main.rs`
  - runtime mode selection and startup messaging
- `src/xr_session.rs`
  - OpenXR lifecycle, view configuration, shell integration
- `src/renderer.rs`
  - swapchain rendering, frame submission, screenshot flow
- `src/vk_backend.rs`
  - Vulkan instance/device/queue-family setup
- `src/mirror_window.rs`
  - mirror window swapchain handling

## What Looks Done

- The app no longer depends on the removed `--desktop` preview runtime.
- Runtime startup now appears more explicit than before.
- Preflight/error messaging has been improved enough to claim partial progress.

## What Still Needs Proof

- Mirror path image-layout correctness
- Queue-family robustness across different hardware
- Swapchain extent behavior on more than one machine/setup
- Whether validation-layer output is clean on the critical paths

## Open Questions

- Is mirror-path behavior validated on a strict Vulkan validation-layer run?
- Is current queue-family logic acceptable on systems where present and graphics differ?
- Is there any remaining code path that still assumes desktop preview mode exists?

## Suggested Next Tasks

1. Run or document a mirror-path audit with explicit findings.
2. Audit queue-family selection and document exact supported assumptions.
3. Add a short "runtime environment matrix" note:
   - tested runtime
   - tested GPU/vendor
   - tested headset/runtime combo
4. Add evidence links or notes for any item marked done in the planning doc.

## Exit Criteria

- Mirror path audited and documented
- Queue-family behavior audited and documented
- Runtime preflight status backed by code references
- Validation/runtime confidence recorded in this file
