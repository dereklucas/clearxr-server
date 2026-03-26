# Track 05: UI Stack and Surface Strategy

## Decision

**Replace Ultralight with egui. Start with plain egui, evaluate egui-shadcn for theming later.**

This is a decided strategy. The current hybrid approach -- Ultralight HTML panels plus pixel-generated Rust chrome -- served MVP exploration, but the polling bridge, CPU rendering overhead, ~50MB SDK dependency, and niche `ul-next` crate are liabilities going forward. egui eliminates all four while unifying the UI stack into a single system.

## Mission

Migrate all UI surfaces from Ultralight (HTML) and hand-rolled pixel generation to egui, rendered directly into the existing Vulkan panel texture pipeline.

## Why egui

### What we're solving

1. **JS-to-Rust polling bridge** -- fragile, no type safety, no error propagation, events lost on missed polls. egui is native Rust: no bridge, no serialization, no string evaluation.
2. **CPU-rendered HTML** -- Ultralight rasterizes to a CPU buffer, BGRA-to-RGBA converts, then uploads to Vulkan staging. egui can render directly to Vulkan textures via `egui-ash` or to an RGBA buffer with `egui`'s built-in software rasterizer.
3. **~50MB SDK dependency** -- `ul-next-sys` downloads the full Ultralight SDK (ICU data, certificate bundles, browser engine) at build time. egui is pure Rust, compiles from source.
4. **Niche crate, low bus factor** -- `ul-next` has limited maintenance. egui is one of the most actively maintained Rust UI libraries with a large contributor base.
5. **Two rendering systems** -- HTML for complex surfaces, hand-rolled pixels for simple chrome. egui replaces both with a single immediate-mode system.

### Why egui specifically

- **Immediate-mode fits the XR frame loop.** We already call `update()` every frame. egui's `ui.button()` / `ui.label()` model maps directly to this.
- **Vulkan-native rendering.** `egui-ash` or manual texture extraction — either way, no CPU bitmap conversion step.
- **Replaces procedural pixel generators.** FPS counter, toolbar, notifications, grab bar become trivial egui widgets instead of hand-rolled bitmap font rendering.
- **Pure Rust, no external runtime.** Compiles with `cargo build`. No SDK downloads, no binary blobs.
- **Large widget ecosystem.** Scrollable containers, grids, text input, sliders, combo boxes — all built in. Third-party crates cover everything else (see Component Strategy below).

### What we give up

- **HTML/CSS hot-reload.** Ultralight's file watcher let us edit HTML and see changes in-headset. With egui, UI changes require `cargo run`. This is acceptable — the iteration loop is still fast, and we gain type safety.
- **CSS layout flexibility.** HTML/CSS is more expressive for complex responsive layouts. egui's layout system is simpler. For our panel sizes (1024x640 and smaller), this is not a meaningful limitation.
- **Web inspector / devtools.** Ultralight surfaces could be debugged with familiar browser tools. egui has no equivalent, but bugs are Rust compiler errors, not runtime JS surprises.

## Current Surface Inventory and Migration Plan

### Phase 1: Replace pixel-generated surfaces with egui

These are the simplest migrations — replace hand-rolled bitmap code with egui widgets. No Ultralight dependency involved.

| Surface | Current generator | Migration |
|---------|-------------------|-----------|
| FPS counter | `generate_fps_pixels()` in `src/launcher_panel.rs` | `ui.label()` with monospace font |
| Grab bar | `generate_grab_bar_pixels()` in `src/launcher_panel.rs` | `egui::Frame` with rounded rect |
| Toolbar | `generate_toolbar_pixels()` in `src/launcher_panel.rs` | `ui.horizontal()` with icon buttons |
| Notifications | `generate_notification_pixels()` in `src/shell/mod.rs` | `ui.colored_label()` or `egui-notify` crate |

**Why first:** Zero risk to Ultralight surfaces. Proves the egui → Vulkan texture pipeline works. Deletes the bitmap font system (`draw_text()` in `src/launcher_panel.rs`).

### Phase 2: Replace settings and system tray

| Surface | Current file | Migration |
|---------|-------------|-----------|
| Settings | `ui/settings.html` | egui sliders, checkboxes, combo boxes, toggles |
| System tray | `ui/system-tray.html` | egui buttons in a radial or grid layout |

**Why second:** Forms and menus are egui's sweet spot. Direct Rust data binding replaces the entire `__clearxr_pending_save` / `__clearxr_tray_pending` polling bridge.

### Phase 3: Replace keyboard

| Surface | Current file | Migration |
|---------|-------------|-----------|
| Virtual keyboard | `ui/keyboard.html` | egui grid of key buttons with `ui.button()` |

**Why third:** Moderate complexity (many buttons, shift/caps state), but straightforward layout.

### Phase 4: Replace launcher

| Surface | Current file | Migration |
|---------|-------------|-----------|
| Game launcher | `ui/launcher-v2.html` | egui `ScrollArea` + grid of game cards with images |

**Why last:** Most complex surface. Scrollable card grid with search, filtering, images, text wrapping. This is where egui's layout limitations are most visible. By this phase we'll have enough egui experience to know if we need `egui-shadcn` or `egui_taffy` for richer layout.

## Vulkan Integration Architecture

### Rendering path

egui renders to the same `LauncherPanel` Vulkan quad pipeline that Ultralight and pixel generators use today. The panel system does not change — only the pixel source.

Two integration options:

**Option A: egui software rasterizer → pixel buffer upload (recommended for Phase 1)**

```
egui::Context::run()
  → egui tessellates to ClippedPrimitive meshes
  → egui_extras::image or epaint rasterizes to RGBA pixels
  → upload to Vulkan staging buffer (same path as today)
```

Simplest to implement. Reuses the existing staging buffer upload in `LauncherPanel`. Good enough for all panel sizes we currently use.

**Option B: egui-ash direct Vulkan rendering (evaluate for Phase 4)**

```
egui::Context::run()
  → egui-ash renders directly to a Vulkan render target
  → render target is the panel texture (no CPU copy)
```

Better performance for the launcher's scrollable grid. Requires a separate Vulkan render pass per egui panel. Evaluate once Phase 1 proves the input mapping works.

### Input mapping

VR controller ray-casts already produce 2D UV coordinates on panel surfaces (`src/panel/mod.rs`). These map to egui input:

```rust
// In Shell::tick(), after ray-cast hit detection:
let pointer_pos = egui::pos2(hit_uv.x * panel_width, hit_uv.y * panel_height);
raw_input.events.push(egui::Event::PointerMoved(pointer_pos));

// Trigger pull → click:
if trigger_just_pressed {
    raw_input.events.push(egui::Event::PointerButton {
        pos: pointer_pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
}
```

This replaces the current `UiRenderer::inject_mouse_move()` / `inject_mouse_click()` Ultralight calls.

## Component Strategy

Start with **plain egui**. The built-in widgets cover every surface we have today.

**Evaluate later (after Phase 2):**

- **egui-shadcn** -- shadcn/ui-inspired button, input, select, checkbox, switch, toggle, popover components. Adds visual polish without architectural changes. Drop-in upgrade: swap `ui.button("x")` with `ShadcnButton::new("x").render(ui)`.
- **egui-material3** -- Material Design 3 components. Alternative to egui-shadcn if we prefer that aesthetic.
- **egui-notify** -- Toast notifications. May replace our custom notification panel or complement it.
- **egui_taffy** -- CSS flexbox/grid layout via the taffy engine. Useful if the launcher grid needs more layout control than egui's built-in `Grid` provides.
- **catppuccin-egui** or **egui-aesthetix** -- Theming. Apply once the UI structure is stable.

The upgrade path from plain egui to any of these is mechanical (swap widget calls, add theme). No architectural changes required. This is why we start plain.

## Dependency Changes

### Add

```toml
egui = "0.31"           # Core immediate-mode UI
epaint = "0.31"         # Software rasterizer for pixel buffer output
egui_extras = "0.31"    # Image loading, tables
```

### Remove (after Phase 4 complete)

```toml
ul-next = "0.5"         # Ultralight HTML engine
notify = "7"            # File watcher (only used for HTML hot-reload)
```

### Keep

```toml
image = "0.25"          # Still needed for PNG screenshots + egui image loading
```

## File Changes

### New files

| Path | Purpose |
|------|---------|
| `src/ui/egui_panels.rs` | egui panel definitions (one function per surface) |
| `src/ui/egui_renderer.rs` | egui context management, input injection, pixel buffer extraction |

### Modified files

| Path | Change |
|------|--------|
| `src/shell/mod.rs` | Replace `UiRenderer` instances with egui context. Remove bridge polling. Remove `generate_notification_pixels()`. |
| `src/launcher_panel.rs` | Remove `generate_fps_pixels()`, `generate_grab_bar_pixels()`, `generate_toolbar_pixels()`, `draw_text()`. Pixel upload path stays. |
| `Cargo.toml` | Add `egui`, `epaint`, `egui_extras`. Eventually remove `ul-next`, `notify`. |

### Removed files (after Phase 4)

| Path | Reason |
|------|--------|
| `src/ui/ui_renderer.rs` | Ultralight wrapper — fully replaced |
| `ui/launcher-v2.html` | HTML launcher — replaced by egui |
| `ui/settings.html` | HTML settings — replaced by egui |
| `ui/system-tray.html` | HTML system tray — replaced by egui |
| `ui/keyboard.html` | HTML keyboard — replaced by egui |
| `ui/mockup-*.html` | Design exploration mockups — no longer needed |
| `ui/launcher.html` | Original launcher — superseded |

## Accepted Risks

1. **No hot-reload.** UI iteration requires recompile. Mitigated by fast `cargo run` cycle and the desktop feature flag for quick testing without a headset.
2. **egui layout limits.** The launcher card grid is the stress test. If egui's built-in `Grid` + `ScrollArea` can't handle it, `egui_taffy` adds CSS-grade layout. We'll know by Phase 4.
3. **egui aesthetics.** Plain egui looks utilitarian. Acceptable for now — egui-shadcn or a custom theme can be applied later without architectural changes.
4. **No accessibility.** Same as Ultralight — egui does not provide screen reader support. This remains a hard requirement before any public release, regardless of UI stack.

## Requirements

### 1. egui → Vulkan texture pipeline works

Definition of done:

- An egui context renders to an RGBA pixel buffer
- That buffer uploads to a `LauncherPanel` Vulkan texture
- The panel displays in-headset (or in desktop mode) with correct content

### 2. VR input maps to egui

Definition of done:

- Controller ray-cast UV coordinates drive egui pointer events
- Trigger press/release maps to egui click events
- egui hover states and button clicks work in-headset

### 3. All pixel-generated surfaces replaced (Phase 1)

Definition of done:

- `generate_fps_pixels()`, `generate_grab_bar_pixels()`, `generate_toolbar_pixels()`, `generate_notification_pixels()` deleted
- `draw_text()` bitmap font system deleted
- All four surfaces render via egui

### 4. All Ultralight surfaces replaced (Phases 2-4)

Definition of done:

- `ul-next` removed from `Cargo.toml`
- `src/ui/ui_renderer.rs` deleted
- No `evaluate_js()` calls remain in the codebase
- No `__clearxr_pending_*` bridge variables remain
- All HTML files in `ui/` either deleted or archived

### 5. Component library evaluated

Definition of done:

- After Phase 2, a written decision on whether to adopt egui-shadcn, egui-material3, or stay plain
- Decision based on actual experience building settings and system tray in plain egui

## File Ownership

| Path | Owner | Notes |
|------|-------|-------|
| `src/ui/egui_renderer.rs` | UI stack | egui context, input injection, pixel extraction |
| `src/ui/egui_panels.rs` | UI stack | Per-surface egui layout functions |
| `src/launcher_panel.rs` | Rendering | Vulkan quad pipeline, texture upload (unchanged) |
| `src/shell/mod.rs` | Shell | Panel orchestration, egui context per surface |

## Dependencies

- Interaction model track (input mapping from controllers/hands to egui events)
- Settings track (settings UI is Phase 2 — needs data model finalized first)
- Launcher track (launcher UI is Phase 4 — needs game discovery data model)

## Acceptance Checklist

- [x] egui renders to Vulkan panel texture in-headset — software rasterizer → RGBA → LauncherPanel upload
- [x] VR controller input drives egui interaction — InputDispatcher PointerMove/Down → egui events
- [x] Phase 1 complete: pixel-generated surfaces replaced — FPS, toolbar, grab bar, notifications
- [x] Phase 2 complete: settings and system tray replaced — direct Config binding, no JS bridge
- [x] Phase 3 complete: keyboard replaced — egui QWERTY with shift/space/backspace
- [x] Phase 4 complete: launcher replaced — ScrollArea + Grid, search, click-to-launch
- [x] `ul-next` dependency removed — along with notify crate and all 17 HTML files
- [ ] Component library decision documented — deferred until visual polish pass
- [ ] No regression in frame timing — needs in-headset testing to verify
