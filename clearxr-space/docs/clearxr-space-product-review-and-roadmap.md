# clearxr-space Product Review And Roadmap

## Current Status

All 6 MVP tracks have been addressed. Key status:

- **74 tests passing**
- **Key features shipped:** launcher (search, alphabetical, honest categories), desktop capture with input injection, toolbar, grab bar, settings (4 real controls), notifications, screenshots
- **Preview runtime removed:** the old `--desktop` winit-based preview runtime is gone. XR is the sole runtime, but the in-shell desktop viewer remains part of product scope.
- **What's NOT done:** mirror path layout audit, queue-family robustness audit, hand tracking (1.0 scope)

---

## Purpose

This document turns the `clearxr-space` review into a working execution plan.

It is written to support multiple agents or contributors working in parallel. It separates:

- what is broken or misleading now
- what is absolutely required for a credible MVP
- what belongs in 1.0
- what belongs in 2.0
- what stack changes are worth making to productionize the product

This document is intentionally opinionated. The goal is not to preserve every existing idea. The goal is to help us ship something real.

See also:

- [Docs Index](C:/Apps/clearxr-server/clearxr-space/docs/README.md)
- [Track 01: Runtime Foundation](C:/Apps/clearxr-server/clearxr-space/docs/track-01-runtime-foundation.md)
- [Track 02: Interaction Model](C:/Apps/clearxr-server/clearxr-space/docs/track-02-interaction-model.md)
- [Track 03: Launcher And App Model](C:/Apps/clearxr-server/clearxr-space/docs/track-03-launcher-and-app-model.md)
- [Track 04: Settings Truthfulness](C:/Apps/clearxr-server/clearxr-space/docs/track-04-settings-truthfulness.md)
- [Track 05: UI Stack And Surface Strategy](C:/Apps/clearxr-server/clearxr-space/docs/track-05-ui-stack-and-surface-strategy.md)
- [Track 06: Product Framing And Acceptance](C:/Apps/clearxr-server/clearxr-space/docs/track-06-product-framing-and-acceptance.md)

---

## Executive Summary

`clearxr-space` has the bones of an ambitious XR shell, but right now it is closer to a promising prototype than a trustworthy product.

The strongest current qualities are:

- ambitious native XR/runtime architecture
- a workable panel metaphor
- a meaningful launcher-plus-desktop direction
- enough existing surface area to prove there is a real product opportunity

The biggest problems are:

- interaction correctness is not trustworthy
- the UI often overpromises what the backend actually does
- the launcher taxonomy is more cosmetic than reliable
- the shell's identity is muddy
- the runtime layer still contains fragile Vulkan/OpenXR behavior
- too much basic UI behavior is hand-rolled in places where we should be standardizing

The short version:

- MVP should be a **trustworthy XR launcher with a light desktop companion**
- 1.0 should be a **coherent spatial shell with honest settings, hardened runtime behavior, persistent state, and a unified interaction model**
- 2.0 should only expand into "full XR shell/platform" territory if the team deliberately chooses that bet

---

## Hard Truths

### 1. The shell is not trustworthy enough yet

The most serious issue is interaction correctness. A held trigger can produce repeated clicks across launcher, desktop, settings, tray, and keyboard-style surfaces. That alone is enough to make the shell feel unsafe.

If the user cannot trust that one press equals one action, the rest of the product cannot be called an MVP.

### 2. Too much of the UI is placebo

Several visible controls imply persistent or live functionality that is not fully wired through runtime behavior. A product loses trust much faster from misleading controls than from missing controls.

The correct response is not "wire everything eventually." The correct response is "hide, defer, or relabel anything we cannot honor."

### 3. The product identity is still mushy

Right now `clearxr-space` borrows patterns from three different products:

- an XR game launcher
- a spatial desktop companion
- a full XR shell / XR operating environment

Those are not the same product. We need to pick one for MVP.

### 4. The current UI stack is acceptable for prototyping, not ideal for production

The Rust + OpenXR + Vulkan foundation makes sense. The long-term question is the shell UI layer.

The current stack combines:

- low-level custom rendering
- browser-like HTML panels
- custom JS-to-Rust bridging
- hand-built utility UI in Rust
- duplicated input logic

That is too fragile to scale without a deliberate productionization pass.

---

## Product North Star Recommendation

### Recommended MVP identity

**XR launcher with a light desktop companion**

That means:

- the main job is to help the user enter the shell, find content, launch it reliably, and optionally access a controlled desktop panel
- the desktop should be useful, but not the primary product promise
- the shell should not pretend to be a complete spatial operating environment yet

### Why this is the right call

- it matches the current implementation better than "full shell"
- it gives us a narrower trust bar
- it lets us cut confusing or premature surfaces
- it makes the roadmap sharper

### Explicitly not the MVP identity

- "the Vision Pro / SteamVR shell competitor"
- "a full productivity workspace platform"
- "a browser-hosted XR app platform"

Those may be 2.0 directions. They are not the right promise now.

---

## Critical Concerns By Area

### Interaction And Input

- Click semantics are not safe enough.
- The shell currently mixes a structured input abstraction with direct per-frame hand-written panel logic.
- Core actions are hidden behind non-obvious behaviors.
- Desktop interaction semantics are still less clearly defined than the launcher flow.

### Launcher And Product Utility

- The launcher currently looks more trustworthy than its data model is.
- "VR" classification is too heuristic-driven.
- "Recent" style organization is not credible unless actually implemented.
- A grid of cards is not enough; the launcher needs a reliable content model.

### Settings And Product Truthfulness

- A number of settings look shipping-grade while being only partially applied or not applied live.
- Startup defaults are not consistently honored.
- There is too much surface area for an MVP settings panel.

### Runtime And Platform Reliability

- There are known correctness risks in the mirror/swapchain path.
- Queue-family and extent handling are not robust enough.
- Core runtime orchestration is too monolithic.
- Build/runtime requirements are still too brittle for a clean MVP experience.

### Design And UX

- The visual system is fragmented.
- Launcher, settings, tray, toolbar, and keyboard do not feel like the same product.
- The shell is visually more mature than its behavioral truth, which creates mismatch and distrust.

---

## MVP Definition

## MVP Gate

`clearxr-space` is a credible MVP only when all of the following are true:

- a user can enter the shell and understand its purpose within 30 seconds
- pointing, clicking, panel movement, mode switching, and content launch all behave reliably
- one user action results in one product action
- the shell does not lie about what works
- common runtime/setup failures are detected and explained clearly
- the launcher is useful in a narrow, honest way

If those conditions are not met, this is still a prototype.

---

## MVP Workstreams

These workstreams are designed so multiple agents can operate in parallel with minimal overlap.

## Workstream A: Input And Interaction Correctness

### Mission

Make the shell safe and predictable to interact with.

### MVP requirements

1. Fix click semantics everywhere.
   Done means:
   - one trigger press produces one logical click
   - launcher, desktop, settings, tray, notifications, and keyboard all use the same activation semantics
   - no repeated actions while a trigger is simply held

2. Consolidate onto one input pipeline.
   Done means:
   - one event model owns hover, press, release, drag, and optional scroll/resize
   - duplicated panel-specific click behavior is removed
   - input thresholds and deadzones are configured centrally

3. Make interactions discoverable.
   Done means:
   - visible hints for point/click, move panel, switch surfaces, and menu/tray entry
   - obvious feedback when anchor or mode changes

4. Make desktop interaction consistent.`r`n   Done means:`r`n   - desktop panel interaction follows the same predictable press/release model as the rest of the shell`r`n   - desktop cursor movement and click behavior are understandable`r`n   - desktop interaction does not bypass the shared shell interaction model

### MVP non-goals

- advanced gesture vocabulary
- hand-tracking-first UX
- rich focus graph
- complex shortcut systems

### Suggested ownership

- Shell input model
- Panel event routing
- Desktop safety mode
- Interaction affordances

---

## Workstream B: Launcher Scope And Data Trustworthiness

### Mission

Turn the launcher from a nice-looking grid into a narrow, dependable core flow.

### MVP requirements

1. Narrow the launcher promise.
   Done means:
   - remove or disable unsupported categories
   - no fake "recent" state
   - no authoritative-looking VR labels unless backed by real data
   - product copy matches reality

2. Make content launch dependable.
   Done means:
   - one click launches one title
   - launch success/failure is communicated clearly
   - previous-app replacement or killing is surfaced explicitly

3. Add minimum library usefulness.
   Done means:
   - search works
   - ordering is deterministic
   - at least one genuinely useful organization mode exists:
     - installed only, or
     - recently launched, or
     - favorites
   - empty states are truthful and actionable

### MVP non-goals

- storefront-grade metadata
- cloud sync
- editorial discovery
- rich artwork ingestion
- social features

### Suggested ownership

- Game scanner and metadata shaping
- Launcher information architecture
- Launch flow and app status UX

---

## Workstream C: Settings Truthfulness

### Mission

Shrink the settings surface until every visible control is honest.

### MVP requirements

1. Eliminate placebo settings.
   Done means:
   - every visible setting either:
     - applies immediately,
     - explicitly says it applies on next launch, or
     - is removed

2. Respect saved defaults on startup.
   Done means:
   - startup view is honored
   - panel defaults that remain visible are honored
   - anchor defaults that remain visible are honored
   - logs explain fallback behavior where needed

3. Shrink settings to MVP size.
   Keep only if fully real:
   - startup view
   - panel opacity
   - show/hide FPS
   - desktop interaction preference only if it maps to real behavior
   - maybe anchor default

   Remove or defer unless fully wired:
   - output device picker
   - mic toggles
   - theme packs
   - advanced theater tuning

### MVP non-goals

- full system-settings parity
- audio routing control
- import/export profiles
- advanced theming

### Suggested ownership

- Config model
- Startup-state restoration
- Settings UI reduction
- Truthfulness audit

---

## Workstream D: Runtime Reliability And Platform Hardening

### Mission

Make the runtime boring enough to trust.

### MVP requirements

1. Fix known correctness issues.
   Done means:
   - mirror path image-layout handling is valid
   - queue-family selection is correct or fails clearly
   - swapchain extent logic respects real surface constraints
   - validation-layer passes are clean enough in critical flows

2. Add environment preflight.
   Done means:
   - startup checks explain missing OpenXR loader/runtime
   - startup checks explain missing Vulkan support
   - shader/resource requirements are validated cleanly
   - error messages are actionable, not cryptic

3. Reduce frame-loop fragility.
   Done means:
   - session lifecycle, rendering, shell tick, and input are split into clearer units
   - logging is structured enough to diagnose failures
   - the runtime can be reasoned about without reading a giant god-function

4. Runtime scope.`r`n   Done: the old `--desktop` preview runtime has been removed. XR is the sole runtime. The in-shell desktop viewer remains part of the product.

### MVP non-goals

- full cross-vendor optimization
- advanced compositor tuning
- perfect multi-runtime compatibility
- large engine rewrite

### Suggested ownership

- Vulkan correctness
- OpenXR startup and preflight
- Runtime refactoring
- Diagnostics/logging

---

## Workstream E: UI Stack And Productionization

### Mission

Decide whether the current HTML/Ultralight-heavy shell is transitional or strategic.

### MVP requirements

1. Make an explicit UI-stack decision.
   Done means:
   - one written decision says:
     - which surfaces remain HTML
     - why they remain HTML
     - how JS-to-Rust messaging works
     - what production risks are accepted

2. Reduce dead-end UI surfaces.
   Done means:
   - remove or hide virtual keyboard unless it works end-to-end
   - remove or hide fake tray controls
   - remove or hide unsupported anchor modes in user-facing flows

3. Improve shell engineering hygiene.
   Done means:
   - move toward typed error/reporting surfaces
   - move toward stronger structured logging
   - stop generating production shell chrome in brittle, one-off ways where avoidable

### MVP non-goals

- full UI rewrite
- motion/design-token system
- engine migration

### Suggested ownership

- UI bridge contract
- HTML-surface audit
- shell chrome simplification
- productionization plan

---

## Workstream F: UX Scope Discipline

### Mission

Cut aggressively enough that the product becomes believable.

### MVP requirements

1. Commit to one product identity.
   Recommendation:
   - XR launcher with light desktop companion

2. Cut dead features aggressively.
   Hide or remove unless finished:
   - virtual keyboard
   - fake volume controls
   - fake theme system
   - fake VR categorization
   - unsupported anchor states in visible controls

3. Define MVP acceptance criteria.
   Human-verifiable acceptance list:
   - shell starts cleanly in XR on supported Windows hardware
   - launcher opens and search works
   - one press equals one launch
   - toolbar switching works without accidental repeats
   - panel grab/move works predictably
   - desktop panel interaction behaves predictably and consistently
   - visible settings behave as promised
   - prerequisite errors are understandable

### MVP non-goals

- social features
- multi-monitor desktop workflows
- voice input
- cloud sync
- workspace persistence across many apps
- plugin ecosystems

### Suggested ownership

- Product framing
- Feature cuts
- Acceptance test checklist
- Documentation/README alignment

---

## MVP Release Bar

The MVP should not ship until all of the following are complete:

- Workstream A complete
- Workstream B complete at narrow scope
- Workstream C complete
- Workstream D complete at correctness/preflight level
- Workstream F complete

Workstream E can be partially complete for MVP if the team chooses to keep Ultralight temporarily, but it must at minimum produce a clear productionization decision and remove misleading surfaces.

---

## 1.0 Requirements

## 1.0 Product Goal

Ship a coherent, trustworthy spatial shell for one clear job, not a grab-bag prototype.

The strongest 1.0 positioning options are:

- `Option A: XR Launcher + Spatial Control Layer`
  - primary job: launch XR titles reliably, manage panels, and provide a minimal desktop/control surface
  - best if we want to stay close to the current architecture

- `Option B: Spatial Desktop Companion`
  - primary job: provide a high-quality floating desktop, system tray, notifications, quick launch, and panel management
  - best if CloudXR or mirrored-desktop utility is the real long-term value

Do not market 1.0 as a full operating environment unless we deliberately choose the bigger platform roadmap.

### 1.0 requirements by theme

### Product

- pick one product north star and cut features that conflict with it
- establish persistent shell state:
  - saved panel state
  - saved anchor mode
  - saved default view
  - real recents / last-used state
- add a clear session lifecycle UX:
  - startup
  - library ready
  - launch in progress
  - running app
  - exit / recovery

### UX And Interaction

- standardize interaction grammar across the shell
- add in-world onboarding and recovery affordances
- replace hidden combos with explicit UI
- formalize comfort rules for panel distance, sizing, recentering, and theater behavior
- add explicit desktop-control mode semantics

### System Design

- split orchestration into explicit subsystems:
  - `ShellState`
  - `InputSystem`
  - `PanelSystem`
  - `UiBridge`
  - `AppRuntime`
- stop bypassing structured input
- define a stable domain model for surfaces
- add typed intent/state exchange between UI and shell
- reduce reliance on hidden JS globals or ad hoc polling semantics

### Runtime And Platform

- move beyond fully serialized frame submission
- introduce at least basic frames-in-flight
- add capability probing/reporting
- harden screen capture and OS input injection
- improve build reproducibility
- add meaningful runtime diagnostics

### Launcher And Data

- replace name-based VR detection with real metadata where possible
- implement real recent/favorites/pinned content
- add app lifecycle status tracking
- add minimal curation and junk filtering

### Design System

- create one visual language
- define shell primitives:
  - buttons
  - tabs
  - toggles
  - sliders
  - toasts
  - panel headers / grab affordances
- make the product readable at headset distance first

---

## 2.0 Requirements

## 2.0 Product Goal

Expand from a reliable shell into a differentiated spatial platform layer.

2.0 is where the team can justify a bigger identity, but only after 1.0 credibility exists.

### Valid 2.0 directions

#### Direction 1: Spatial Game Console

- curated XR library
- rich metadata
- per-title launch profiles
- social or presence features if strategically relevant

#### Direction 2: CloudXR Workspace Shell

- multiple desktops or app surfaces
- spatial multitasking
- room-aware placement
- layout persistence and workspace restore

#### Direction 3: Spatial Command Center

- notifications
- automation surfaces
- system monitoring
- launch orchestration
- scene presets

### 2.0 requirements

### UX And Interaction Maturation

- multi-panel workflows
- snapping and grouping
- saved layouts
- true wrist/head/controller anchors with comfort rules
- real keyboard/text entry
- accessibility modes
- richer scene management and reset behaviors

### Runtime And Platform Maturation

- broader runtime compatibility
- better controller profile coverage
- performance architecture improvements
- telemetry and crash diagnostics
- runtime packaging and validation
- stronger automated coverage for runtime seams

### Product Features

- favorites and collections
- per-app launch recipes
- actionable notifications
- richer history / session restore
- optional automation hooks or companion experiences

---

## Parallel Execution Map

This is the suggested multi-agent/contributor split.

## Lane 1: Runtime Foundation

Scope:

- Vulkan correctness
- OpenXR preflight
- queue-family and swapchain handling
- runtime diagnostics

Files likely touched:

- `src/xr_session.rs`
- `src/renderer.rs`
- `src/vk_backend.rs`
- `src/mirror_window.rs`
- build/runtime docs

Dependencies:

- none for initial correctness work

Blocks:

- Workstream D

---

## Lane 2: Interaction Model

Scope:

- click semantics
- unified input pipeline
- panel event flow
- desktop safety mode
- hover/press/release UX

Files likely touched:

- `src/shell/mod.rs`
- `src/input/mod.rs`
- `src/panel/mod.rs`
- related UI bridge code

Dependencies:

- minimal dependency on runtime correctness

Blocks:

- Workstream A

---

## Lane 3: Launcher And App Model

Scope:

- game scanner shaping
- launch flow state
- launcher filtering and organization
- honest library taxonomy

Files likely touched:

- `src/app/**`
- `src/shell/mod.rs`
- `ui/launcher-v2.html`
- related config/state surfaces

Dependencies:

- interaction fixes for final polish

Blocks:

- Workstream B

---

## Lane 4: Settings Truthfulness

Scope:

- settings audit
- remove or disable fake controls
- startup restore behavior
- smaller MVP settings surface

Files likely touched:

- `src/config/**`
- `src/shell/mod.rs`
- `ui/settings.html`

Dependencies:

- some overlap with product framing

Blocks:

- Workstream C

---

## Lane 5: UI Stack And Surface Strategy

Scope:

- decide HTML vs native UI role
- document the bridge contract
- identify surfaces to migrate or kill
- standardize shell chrome direction

Files likely touched:

- planning docs
- `src/ui/**`
- `src/launcher_panel.rs`
- HTML surfaces as needed

Dependencies:

- low for documentation
- higher for actual migration work

Blocks:

- Workstream E

---

## Lane 6: Product Framing And Acceptance

Scope:

- README positioning
- MVP definition
- feature cuts
- acceptance checklist
- demo flow / test scenarios

Files likely touched:

- this roadmap
- README or product docs
- QA / checklist docs

Dependencies:

- none

Blocks:

- Workstream F

---

## UI Library / Crate Recommendations

## Recommendation summary

### Keep

- `openxr`
- `ash`
- `glam`
- `serde` / `toml`

### Add or use more aggressively

- `tracing` + `tracing-subscriber` for structured diagnostics
- `thiserror` where stronger typed error surfaces improve clarity

### Short-term UI recommendation

**Keep Ultralight only as a bounded transitional layer.**

Use it for:

- mockups
- content-like panels
- short-term existing launcher/settings continuity if needed

Do not keep expanding it as the default shell strategy unless we intentionally want a browser-driven XR shell.

### Best pragmatic productionization move

**Adopt `egui` for core shell controls and truth-critical product surfaces.**

Use `egui` first for:

- settings
- launcher shell chrome
- notifications
- debug/admin overlays
- desktop-mode fallback surfaces

Why:

- fast iteration
- tight Rust integration
- less JS bridge fragility
- easier to keep product state truthful

Risk:

- it will need styling discipline so the product does not look like a dev tool

### Strategic long-term bet

**Consider a Bevy-style shell architecture only if we commit to becoming a true XR platform.**

Use this path if we want:

- a real shell scene graph
- reusable interaction systems
- panel/layout persistence as a first-class concept
- richer animation and spatial orchestration

Do not adopt this casually. It is a platform decision, not a cleanup task.

### Lower-priority options

- `iced`: interesting, but weaker fit here than `egui`
- `taffy`: useful as a supporting layout engine if we build our own retained-mode XR UI toolkit
- `xilem` / `masonry`: worth monitoring, not worth standardizing on yet for this product

### Explicit recommendation

#### MVP

- keep current renderer
- keep current panel composition concept
- keep Ultralight only where necessary
- stop adding new shell-critical HTML surfaces

#### 1.0

- move shell-critical UI toward `egui` if and when Ultralight is reduced
- make the Rust-side UI/state boundary much more explicit

#### 2.0

- if the product proves it wants to be a platform, consider the bigger Bevy-style architectural shift

---

## Big Swings Worth Considering

### Big Swing 1: Stop treating HTML panels as temporary mockups that accidentally became product

If a surface matters to the shell, it should have:

- a real component model
- a typed state contract
- a reusable design language
- proper lifecycle ownership

Right now too many surfaces feel like mockups that got promoted into runtime.

### Big Swing 2: Split "shell runtime" from "shell experience"

Create a cleaner architecture boundary:

- runtime core:
  - OpenXR
  - Vulkan
  - panel transforms
  - haptics
  - capture
  - runtime capability negotiation

- shell experience:
  - launcher
  - settings
  - notifications
  - toolbar/tray
  - content lifecycle
  - onboarding

That would reduce the current over-concentration in `src/shell/mod.rs`.

### Big Swing 3: Pick one flagship surface

If launcher is the hero:

- invest heavily in trust, launch flow, metadata, organization, and curation

If desktop is the hero:

- invest heavily in safety, fidelity, pointer model, and workspace control

If neither is clearly the hero:

- the product will continue to feel like a prototype with many side quests

### Big Swing 4: Cut more than feels comfortable

The fastest route to a product is probably:

- fewer settings
- fewer surfaces
- fewer fake categories
- fewer hidden behaviors
- more trust

This is a product where subtraction is likely to create more value than addition in the near term.

---

## Recommended Sequencing

## Phase 0: Stabilization

- fix interaction correctness
- eliminate placebo UI
- fix runtime correctness issues
- define product framing
- define MVP acceptance checklist

## Phase 1: Credible MVP

- honest launcher
- safe desktop companion mode
- truthful settings
- clear onboarding affordances
- environment preflight
- small but dependable product loop

## Phase 2: 1.0

- persistent shell state
- unified design language
- real app lifecycle UX
- more deliberate UI platform choice
- stronger runtime diagnostics and robustness

## Phase 3: 2.0

- broader platform identity
- richer spatial workflows
- stronger layout/workspace model
- optional strategic architecture shift

---

## Final Recommendation

If we want something real soon:

1. define MVP as **XR launcher with light desktop companion**
2. fix interaction correctness before adding anything
3. remove or hide every lying control
4. narrow the launcher promise until it is honest
5. harden the runtime enough that setup and rendering stop feeling fragile
6. reduce Ultralight's role instead of expanding it
7. plan a migration of shell-critical UI toward `egui` if and when the MVP shell is otherwise stable

If we want a true spatial platform later:

- do that after 1.0 credibility exists
- and only with a deliberate architectural decision, not by letting the current shell accrete more responsibilities

The core message:

`clearxr-space` does not need more features first. It needs more truth, more safety, and more discipline.

