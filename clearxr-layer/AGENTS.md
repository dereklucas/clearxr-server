# AGENTS.md

## Purpose

`clearxr-layer` is the **runtime-side dashboard host**.

Its job is to inject ClearXR behavior into the **single OpenXR session that already belongs to the running app**.

That means:

- no second OpenXR app/session
- no “dashboard app running beside the game”
- no assumption that `clearxr-space` stays alive when another OpenXR app takes over

The long-term product direction is that the dashboard shown over running OpenXR apps is hosted here.

## Current Goal

The current goal of `clearxr-layer` is to prove and then harden this host model:

- intercept the app’s OpenXR session
- create layer-owned overlay resources on that session
- render dashboard content into an overlay swapchain
- append the dashboard quad during `xrEndFrame`
- toggle/show the dashboard without needing a second app

This crate is allowed to be messier than `clearxr-space` for now if that helps prove the architecture.

## What Is Proven Now

The following has already been proven:

- the layer-based host model is viable on this runtime
- `clearxr-layer` can hook the app session and append an overlay quad in the app's `xrEndFrame`
- a **visible GPU egui overlay** from `clearxr-layer` has been seen in-headset

That means the question is no longer "can the layer show anything at all?"

The question now is:

- how do we evolve the visible layer-local test card into the real dashboard path
- how do we make that path robust and maintainable

## Non-Goals

Do not turn this crate into a second standalone OpenXR app.

Do not depend on multi-session behavior.

We already proved the runtime does not support the “run another OpenXR dashboard app on top” model.

If you are about to create another session from here, you are probably going in the wrong direction.

## Core Host Model

The correct mental model for this crate is:

- the game/app owns the OpenXR session
- `clearxr-layer` piggybacks on that session
- the layer contributes composition/input behavior inside the same session lifecycle

This is different from `clearxr-space`, which is a normal app host.

## Architectural Guardrails

### 1. Own the layer host logic locally

For now, `clearxr-layer` should own its own:

- swapchain handling
- Vulkan image/layout handling
- overlay composition logic
- host-specific input hookup
- render timing/frame-flow behavior

Do not assume `clearxr-space` host/render code can be reused directly.

### 2. Share dashboard product logic later, not host plumbing now

Eventually worth sharing:

- dashboard state
- egui view/layout code
- actions/models

Not worth forcing into shared code yet:

- Vulkan renderer setup
- OpenXR layer hooking
- image layout transitions
- overlay swapchain submission
- input wiring tied to opaque channel / session hooks

If duplication keeps the layer moving without destabilizing `clearxr-space`, duplication is acceptable.

### 3. The layer must work when `clearxr-space` is absent

This crate should not require `clearxr-space` to be running.

If the current implementation only works when `clearxr-space` is also visible, that is not success. The dashboard host must be independently valid inside any OpenXR app path that loads the layer.

## Known Footguns

### 1. “Compiled” does not mean “loaded”

This is the biggest footgun in the entire layer workflow.

You can successfully build a new DLL and still have the runtime use something else.

Always suspect artifact selection first when behavior makes no sense.

Important dev outputs:

- intended dev DLL: `C:\Users\derek\cargo-build\debug\clear_xr_layer.dll`
- intended dev manifest: `C:\Users\derek\cargo-build\debug\clear-xr-layer.json`

If the headset shows old behavior, verify the **loaded module path**, not just the build log.

### 2. Stale `target-local` artifacts are dangerous

We already lost a lot of time to stale `target-local` copies.

These files can hijack behavior:

- `target-local\debug\clear_xr_layer.dll`
- `target-local\debug\clear-xr-layer.json`

If behavior is inconsistent, check whether `target-local` artifacts still exist and whether something is resolving them unexpectedly.

Do not casually reintroduce `target-local` fallback behavior into build/staging logic.

### 3. OpenXR implicit-layer registry values are enable/disable flags

Windows OpenXR implicit layers are not ordered by a priority number here.

For the relevant registry entry:

- `0` means enabled
- nonzero means disabled

We already had a case where the manifest existed but the layer was not loading because the registry DWORD was `2`.

If the layer mysteriously stops loading, inspect the registry value before assuming the code is broken.

### 4. The manifest can be right while the process still loads no layer

Common failure modes:

- registry points at the right manifest, but the value is disabled
- the DLL path is right, but the wrong process is being inspected
- the app is running, but the layer is not actually mapped into memory
- an old output tree is still being staged somewhere else

Do not trust only:

- `cargo build`
- manifest contents
- file timestamps

Trust:

- actual loaded process modules
- actual layer logs
- actual on-headset behavior

Also: if you have just rebuilt the layer and the visuals still look stale, do a **fresh XR app restart** before going deeper. We already had a case where the new layer behavior only became visible after restarting `clearxr-space`.

### 5. `clearxr-space` renderer assumptions do not automatically apply here

Recent `clearxr-space` rendering moved around `egui-ash-renderer`.

That renderer flow is app-oriented:

- render panel texture
- sample it later
- use app-friendly image layouts/formats

The layer flow is different:

- render directly into an OpenXR overlay swapchain image
- release that image to the compositor
- respect overlay swapchain format/layout constraints

If a renderer works in `space`, that does **not** prove it works in `layer`.

### 6. One-time render/setup tricks are fragile

Rendering a panel once at startup and hoping it stays valid is not a robust overlay model.

Prefer explicit frame-flow ownership:

- acquire swapchain image
- render
- release
- append layer in `xrEndFrame`

If something is “static,” that is fine, but the lifecycle should still match how the compositor expects the image to be used.

### 7. Don’t accidentally prove the wrong thing

A frequent trap is thinking “I see a dashboard” means the layer works.

But if the visible dashboard is actually coming from `clearxr-space`, then the layer still may be broken.

When proving the layer path, make the layer overlay unmistakable:

- different title
- different color
- different position/size if needed

Never rely on ambiguous visuals when validating the layer path.

We now have one positive proof here already: a visible layer-owned egui card was seen after restarting `clearxr-space`.

## Safe Ways To Work In `clearxr-layer`

Good changes:

- improving hook reliability
- tightening session/overlay lifecycle handling
- making overlay rendering explicit and debuggable
- duplicating host/render code from `space` when necessary
- isolating pure dashboard state/view code for later extraction

Risky changes:

- trying to make the layer depend directly on `clearxr-space` internals
- adding a second session
- reusing render code without checking format/layout assumptions
- changing staging/registration logic without checking what DLL is actually loaded

## Build And Runtime Expectations

For dev, assume the desired path is:

- build outputs in `C:\Users\derek\cargo-build\debug`
- implicit layer manifest registered from that same folder
- runtime should load `clear_xr_layer.dll` from that same folder

If there is any doubt:

1. check the registry manifest path
2. check the registry DWORD value
3. check the live process module path
4. check the layer log

In that order.

## Relationship To `clearxr-space`

`clearxr-space` is currently the best UX lab for the dashboard.

`clearxr-layer` is the likely production host for the dashboard over games.

That means:

- `space` is allowed to be the nicer place to iterate on UX
- `layer` is allowed to duplicate renderer/host code temporarily
- extraction/shared crates should happen only after the layer host is solid

Do not make `space` worse just to make the layer cleaner today.

## Practical Guidance For Future Agents

When touching this crate, optimize for:

1. proving the overlay host model
2. ensuring the layer is actually loaded
3. making rendering behavior explicit and debuggable
4. keeping host-specific code local to the layer

If you are unsure whether a bug is architectural or just stale artifacts, assume stale artifacts first and verify the loaded DLL path before doing deeper refactors.

## Short Version

`clearxr-layer` is the **real dashboard host candidate**.

Its job is to render into the app’s existing OpenXR session, not to create its own.

The biggest risks are:

- stale outputs
- disabled registry entries
- wrong DLL being loaded
- assuming `clearxr-space` rendering code will work here unchanged

The host is now proven well enough to move to the next stage.

Trust live loaded modules more than build output.
Keep host-specific plumbing local until the layer path is solid.
