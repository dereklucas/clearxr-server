# Track 01: Runtime Foundation

## Mission

Make the runtime boring enough to trust.

This track owns the low-level foundation that determines whether `clearxr-space` feels like a product or like a fragile prototype.

## Why This Track Exists

Right now there are known risks in:

- mirror swapchain image-layout handling
- queue-family assumptions
- swapchain extent handling
- monolithic runtime orchestration
- setup and preflight clarity

No amount of UX polish makes up for a runtime that fails unpredictably.

## MVP Requirements

### 1. Fix known Vulkan and OpenXR correctness issues

Definition of done:

- [x] mirror path tracks swapchain image layouts correctly - audited: UNDEFINED initial_layout with CLEAR load_op is correct per Vulkan spec (discard + clear). Mirror blit transitions are properly sequenced (COLOR_ATTACHMENT → TRANSFER_SRC, then restore).
- [x] queue-family selection supports presentation correctly - audited: mirror_window.rs checks surface support (line 179-187) and bails with clear error. XR swapchain is managed by the OpenXR runtime.
- [x] swapchain extent selection uses actual window and surface constraints - audited: clamped to surface capabilities, 0x0 skipped (minimized).
- validation-layer results are clean enough on the critical paths to trust them

### 2. Add environment preflight

Status: **Done**

- [x] startup checks for OpenXR loader/runtime presence - system info logging, actionable errors implemented
- [x] startup checks for required Vulkan capability - OpenXR/Vulkan error messages implemented
- [x] shader/resource requirements are validated cleanly
- [x] missing prerequisites produce actionable error messages

### 3. Reduce runtime fragility

Definition of done:

- session lifecycle, rendering, shell tick, and input polling are split into clearer units
- runtime logging is structured enough to diagnose startup and frame-loop failures
- critical paths are understandable without reading one giant orchestration function

### 4. Runtime scope

The old `--desktop` preview runtime has been removed. XR is the sole runtime mode. The in-shell desktop viewer remains part of the product, but it is not a separate runtime.

## 1.0 Requirements

- move beyond fully serialized frame submission
- introduce at least basic frames-in-flight
- improve capability reporting and runtime diagnostics
- harden screen capture and OS injection guardrails
- improve build reproducibility

## 2.0 Requirements

- broader runtime compatibility matrix
- better controller profile handling
- stronger crash diagnostics and telemetry
- runtime packaging and self-check tooling
- deeper automated coverage for runtime seams

## Suggested File Ownership

- `src/xr_session.rs`
- `src/renderer.rs`
- `src/vk_backend.rs`
- `src/mirror_window.rs`
- build and setup docs

## Dependencies

- none for the first correctness pass

## Risks

- over-refactoring before correctness fixes land
- optimizing frame architecture before setup and correctness are trustworthy

## Explicit Non-Goals For MVP

- large engine migration
- full cross-vendor optimization
- perfect multi-runtime coverage
- advanced compositor tuning

## Acceptance Checklist

- XR shell starts cleanly on supported Windows/OpenXR hardware
- mirror path no longer relies on invalid layout assumptions
- bad environment setup fails early with clear messaging
- queue-family and swapchain behavior are no longer obviously fragile


