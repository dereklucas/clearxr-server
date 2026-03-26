# Track 06: Product Framing And Acceptance

## Mission

Make the product easier to understand by tightening its promise, cutting confusing features, and defining what "good enough to ship" actually means.

## Why This Track Exists

The product currently borrows from:

- XR launcher
- spatial desktop companion
- full XR shell

That makes the experience feel broader than it really is. This track exists to sharpen scope and stop accidental ambition from making the product less believable.

## Recommended MVP Framing

**ClearXR Space is an XR launcher with a light desktop companion, built on OpenXR and Vulkan.**

What that means:

- the launcher is the hero surface
- the desktop exists as a controlled secondary utility
- the shell does not present itself as a full operating environment

## MVP Requirements

### 1. Align product language

Status: **Done** — "XR launcher with light desktop companion" is the agreed framing.

- [x] roadmap, README, UI copy, and demo language all describe the same product
- [x] features that do not support that framing are cut, hidden, or demoted

### 2. Cut dead features aggressively

Status: **Done** — keyboard hidden, volume removed, fake categories removed, fake badges removed.

- [x] virtual keyboard hidden
- [x] fake volume controls removed
- [x] fake theme system removed
- [x] fake VR categorization removed
- [x] every visible interaction path works end-to-end
- [x] no "future feature" feeling remains in the mainline product loop

### 3. Define MVP acceptance criteria

Definition of done:

- a short human-verifiable release checklist exists
- team members can use the same checklist to decide whether MVP is real

MVP acceptance checklist:

- [x] shell starts cleanly in XR on supported Windows hardware
- [x] launcher opens and search works
- [x] one press equals one launch
- [x] toolbar switching works without accidental repeats
- [x] panel grab/move works predictably
- [x] desktop capture and input work as standard panel features
- [x] visible settings behave as promised
- [x] prerequisite errors are understandable

## 1.0 Requirements

- coherent product language
- more explicit app lifecycle UX
- better onboarding and recovery language
- a stable release narrative that is not bigger than the product

## 2.0 Requirements

- choose whether to become:
  - a spatial game console,
  - a workspace shell,
  - or a command-center shell

## Suggested File Ownership

- `README.md`
- docs and release notes
- UI copy where needed
- acceptance checklists

## Dependencies

- all other tracks inform this one
- this track should resolve scope conflicts when they appear

## Risks

- leaving too many partially real features visible
- marketing the product as a shell platform before it earns that label

## Explicit Non-Goals For MVP

- social features
- cloud sync
- platform-level shell positioning
- broad productivity claims

## Acceptance Checklist

- [x] the team can explain the product in one sentence — "ClearXR Space is an XR launcher with a light desktop companion, built on OpenXR and Vulkan."
- [x] the docs and UI say the same thing
- [x] there is a stable, believable release bar


