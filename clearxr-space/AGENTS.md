# AGENTS.md

## Purpose

`clearxr-space` is the **home environment host** for ClearXR.

It is currently the place where the shell/dashboard experience is developed, but the long-term product direction is:

- `clearxr-space` hosts the environment/home world
- the **dashboard UI itself** should eventually be reusable outside of `space`
- the **primary shipping dashboard host** is expected to be `clearxr-layer`, not a second normal OpenXR app

In other words: treat `clearxr-space` as the best place to build and iterate on the dashboard UX, but **do not assume its host model is the final system architecture**.

## Current Goal

The near-term goal is to keep `clearxr-space` productive and buildable while we:

- continue developing the dashboard/app-switcher UX here
- keep the environment and home-space behavior working
- avoid coupling `space` too tightly to runtime/layer-specific overlay plumbing

The dashboard code in `space` should move toward being extractable, but `space` itself should remain a solid standalone home experience.

## What Is Proven Now

The runtime/layer-side dashboard direction is no longer hypothetical.

The following has been proven already:

- the second-app OpenXR dashboard host model does not work on this runtime
- the `clearxr-layer` host model does work
- a visible GPU egui overlay from `clearxr-layer` has been seen in-headset

So when working in `clearxr-space`, assume:

- `space` remains the home/environment host
- `layer` is the real direction for the persistent dashboard-over-game host
- the remaining problem is reuse/extraction and productization, not host-model uncertainty

## Non-Goals

Do not turn `clearxr-space` into the production overlay host.

Why:

- `clearxr-space` is a normal OpenXR app
- normal second-app overlay hosting was already disproven with the CloudXR runtime
- the runtime only allows one real session/compositor path at a time
- the persistent dashboard-over-game behavior must come from `clearxr-layer` / runtime-side hosting

If you are tempted to “just keep `space` alive on top of another OpenXR app,” stop and verify the host model first. We already did, and it failed with multi-session/runtime limits.

## Architectural Guardrails

### 1. Reuse the dashboard product logic, not the host plumbing

What should eventually be shared:

- dashboard state
- app-switcher state
- notifications/settings models
- egui widget/layout code

What should **not** be assumed shareable as-is:

- OpenXR session ownership
- swapchain lifecycle
- Vulkan image layout conventions
- compositor submission flow
- input hookup details that depend on host context

`clearxr-space` and `clearxr-layer` are different hosts. Keep that distinction clear.

### 2. Keep `space` buildable while layer work is messy

If runtime/layer work needs duplication, duplication is acceptable.

Do **not** destabilize `clearxr-space` just to make `clearxr-layer` cleaner today. The right tradeoff for now is:

- `space` remains stable
- `layer` can duplicate and experiment
- extraction/shared crates happen later, after the layer path is proven

### 3. Prefer pure egui/dashboard extraction later

If you extract code from `space`, prefer pulling out:

- dashboard state and actions
- egui view/layout code
- data models

Avoid early extraction of:

- `xr_session.rs`
- renderer/swapchain plumbing
- Vulkan-specific host code

## Known Footguns

### 1. `clearxr-space` is not the final overlay architecture

Biggest footgun in the repo right now:

- the dashboard UX lives in `clearxr-space`
- that makes it feel like `space` itself should become the persistent shell
- that is misleading

The UI built here is valuable, but the host model is different in `clearxr-layer`.

### 2. Reusing renderer code from `space` inside `layer` is not plug-and-play

Recent `space` changes moved more rendering to `egui-ash-renderer`, and that changed the renderer contract.

The `space` GPU renderer is built around the app rendering model:

- render into a panel texture
- sample that texture later in the scene
- use layouts/formats appropriate for app-owned panel rendering

That does **not** automatically match layer overlay rendering, where:

- the target is an OpenXR overlay swapchain image
- final image layout expectations differ
- format may differ
- the image is submitted to the compositor, not sampled by `space`

If something renders in `space`, do not assume it will render in `layer` without host-specific adaptation.

### 3. Input edge detection matters

For egui interaction:

- use edge-triggered input
- do not map raw held trigger state to repeated clicks
- keep `pointer_leave()` semantics intact

Breaking this will create sticky hover, repeated clicks, and “why is egui acting haunted?” bugs.

### 4. Large panels should stay GPU-rendered

The dashboard is a large panel and should remain on the GPU path.

Do not regress large dashboard rendering back to CPU rasterization just because it is simpler for a test. CPU rendering is okay for tiny utility panels, not for the main dashboard.

### 5. Do not casually mix host assumptions into dashboard code

Examples of assumptions that become a trap later:

- “menu button means toggle dashboard here in `space`”
- “dashboard always lives in world space”
- “dashboard always owns app launching directly”
- “dashboard always has access to `xr_session` internals”

Prefer modeling these as host-provided behavior, not hardwired `space` truths.

### 6. Build output confusion is real

We already lost significant time to stale build artifacts and duplicate manifests.

Important dev assumptions:

- the intended dev build output is `C:\Users\derek\cargo-build\debug`
- stale `target-local` artifacts can hijack behavior and make debugging lie
- if behavior makes no sense, verify the actually loaded module path before trusting symptoms

If a change “definitely compiled” but the headset shows old behavior, suspect artifact selection first.

## Safe Ways To Work In `clearxr-space`

Good changes:

- improve dashboard UX
- clean up egui structure
- make dashboard state/actions easier to extract later
- isolate environment-specific code from dashboard-specific code
- reduce direct coupling between dashboard widgets and XR/session internals

Risky changes:

- refactoring `space` to match a hypothetical future layer API before it exists
- moving renderer/session code into shared crates too early
- baking runtime-layer behavior assumptions into the `space` dashboard
- destabilizing the current home-space flow in pursuit of overlay architecture

## Practical Guidance For Future Agents

When touching `clearxr-space`, optimize for:

1. preserving the current home-space experience
2. making dashboard logic cleaner and more extractable
3. avoiding host-specific assumptions in pure UI/state code

When deciding whether code belongs here or in `clearxr-layer`:

- if it is about the environment, home-world presentation, or app-host behavior, it belongs in `space`
- if it is about persistent overlay-over-other-apps behavior, it belongs in `layer`
- if it is pure dashboard UI/state, it should eventually be shareable

## Short Version

`clearxr-space` is the **best current place to build the dashboard**, but **not the final place that dashboard will live**.

Keep `space` working.
Keep the dashboard code getting cleaner.
Do not confuse “where the UI is built today” with “what the production host model is.”
