# ClearXR Space Architecture

## System Overview

```
OpenXR Runtime
    |
xr_session.rs  (session lifecycle, controller input, frame timing)
    |
Shell  (shell/mod.rs -- InputDispatcher + Dashboard wrapper)
    |
Dashboard  (shell/dashboard.rs -- one egui frame per tick)
    |
EguiGpuRenderer -> LauncherPanel texture -> scene render pass
```

The Shell is a thin orchestrator. It owns one `Dashboard` (all UI) and one `InputDispatcher` (controller hit-testing). Each frame, `Shell::tick()` clears pointer state, runs input dispatch, forwards events to Dashboard, then calls `Dashboard::render()` which produces the egui frame and writes the result into a Vulkan texture via `EguiGpuRenderer`.

## Key Types

| Type | File | Purpose |
|------|------|---------|
| `Shell` | `src/shell/mod.rs` | Top-level wrapper: Dashboard + InputDispatcher |
| `Dashboard` | `src/shell/dashboard.rs` | All UI state, egui rendering, screen capture |
| `DashboardTab` | `src/shell/dashboard.rs` | Enum: `Launcher`, `Desktop`, `Settings` |
| `DashboardAction` | `src/shell/dashboard.rs` | Enum: `LaunchGame(u32)`, `SaveConfig`, `Screenshot`, etc. |
| `InputDispatcher` | `src/input/mod.rs` | Ray-panel hit-test, trigger edge detection |
| `InputEvent` | `src/input/mod.rs` | Enum: `PointerMove`, `PointerDown`, `PointerUp`, `GrabStart`, `GrabMove`, `GrabEnd`, `ButtonPress`, `ButtonRelease`, `ThumbstickMove`, `TextInput` |
| `HandState` | `src/input/mod.rs` | Per-hand controller state (grip_pos, aim_dir, trigger, squeeze, buttons, touch) |
| `ControllerState` | `src/input/mod.rs` | Both hands: `left: HandState`, `right: HandState` |
| `PanelId` | `src/panel/mod.rs` | Newtype `u64` identifier for hit-testing |
| `PanelTransform` | `src/panel/mod.rs` | center, right_dir, up_dir, width, height, opacity, anchor, grabbable |
| `PanelAnchor` | `src/panel/mod.rs` | Enum: `World`, `Controller`, `Wrist`, `Head`, `Theater` |
| `EguiGpuRenderer` | `src/ui/egui_gpu_renderer.rs` | GPU-accelerated egui: tessellate -> Vulkan render pass |
| `EguiRenderer` | `src/ui/egui_renderer.rs` | CPU software rasterizer for small panels |
| `LauncherPanel` | `src/launcher_panel.rs` | Vulkan textured quad: texture, staging buffer, pipeline, draw |
| `ScreenCapture` | `src/capture/screen_capture.rs` | DXGI background thread capture + SendInput injection |
| `CaptureFrame` | `src/capture/screen_capture.rs` | Raw BGRA pixels from capture thread |
| `Config` | `src/config/mod.rs` | Top-level TOML config (PanelConfig, AudioConfig, DisplayConfig, ShellConfig) |
| `ShellFrame` | `src/shell/mod.rs` | Per-frame output: ray-hit distances + haptic pulse requests |

## Input Flow

```
Controller -> ControllerState (built in xr_session.rs)
    -> InputDispatcher.process(state, panels)   [src/input/mod.rs]
    -> Vec<(PanelId, InputEvent)>
    -> Shell::tick() dispatches:
         PointerMove  -> dashboard.pointer_move(u, v)
         PointerDown  -> dashboard.click()
         GrabStart    -> grab_offset / grab_hand stored on Dashboard
    -> Dashboard.render(vk) runs egui with click_pending flag
    -> egui Button.clicked() / Slider.changed() responses
```

**Edge detection**: `InputDispatcher` tracks `prev_trigger: [bool; 2]` and only emits `PointerDown` on the rising edge (`trigger >= 0.5` and was previously below). This ensures one press = one action.

**pointer_leave() every frame**: `Shell::tick()` calls `dashboard.pointer_leave()` at the top of each frame (line 87 of `shell/mod.rs`). `InputDispatcher` re-establishes hover via `PointerMove` events if the ray still intersects a panel. This prevents stale hover state in egui.

**Hit-testing**: `InputDispatcher.process()` calls `PanelTransform::hit_test(aim_pos, aim_dir)` for each panel, sorts hits by distance (closest first), and emits events for the frontmost panel hit. Grab events are only emitted when the hit point is in the edge margin zone (`in_grab_margin()`, 15% from edges).

## Rendering Flow

```
Dashboard.render(vk)
    -> egui::Context::run() produces shapes via layout closure
    -> EguiGpuRenderer.run(vk, texture, format, click, closure)
       -> tessellates egui output
       -> uploads vertex/index buffers to GPU
       -> Vulkan render pass writes to LauncherPanel.texture
    -> panel.texture_initialized = true
    -> Main render pass: scene.frag + panel.frag overlays
```

**Screen capture (desktop tab)**:
```
DXGI Desktop Duplication (background thread)
    -> CaptureFrame { data: Vec<u8>, width, height } via mpsc channel
    -> Dashboard.render() calls screen_capture.try_get_frame()
    -> screen_panel.stage_pixels(&frame.data)
    -> record_upload() copies staging -> texture before render pass
    -> screen_panel drawn behind dashboard panel via alpha blend
```

The dashboard texture format is `R8G8B8A8_SRGB` (egui output). The screen capture texture is `B8G8R8A8_SRGB` (native DXGI format -- no CPU conversion needed).

## How to Add a Dashboard Tab

1. **Add variant to `DashboardTab`** in `src/shell/dashboard.rs` (line 29):
   ```rust
   pub enum DashboardTab {
       Launcher,
       Desktop,
       Settings,
       YourNewTab,  // add here
   }
   ```

2. **Add tab button** in the tab bar section of `Dashboard::render()` (around line 250):
   ```rust
   let tabs = [
       (DashboardTab::Launcher, "LAUNCHER"),
       (DashboardTab::Desktop, "DESKTOP"),
       (DashboardTab::Settings, "SETTINGS"),
       (DashboardTab::YourNewTab, "YOUR TAB"),  // add here
   ];
   ```

3. **Add content match arm** in the `CentralPanel` match (around line 323):
   ```rust
   DashboardTab::YourNewTab => {
       render_your_tab_content(ui, &mut your_state, &mut your_action);
   }
   ```

4. **Write a `render_your_tab_content()` helper** at the bottom of `dashboard.rs`, following the pattern of `render_launcher_content()` or `render_settings_content()`. Use egui widgets (Button, Slider, Label, ScrollArea, Grid) for all interaction.

5. **Add state fields** to the `Dashboard` struct (line 45) for any persistent state your tab needs. Initialize them in `Dashboard::new()`.

6. **If your tab produces actions**, add a variant to `DashboardAction` (line 36) and handle it in `Shell::tick()` (line 289).

7. **If your tab needs cleanup**, add destruction logic in `Dashboard::destroy()` (line 416).

## How to Add a New Input Action

1. **Define the event** in the `InputEvent` enum in `src/input/mod.rs` (line 21):
   ```rust
   pub enum InputEvent {
       // ... existing variants ...
       YourNewEvent { hand: Hand, /* fields */ },
   }
   ```

2. **Detect in `InputDispatcher::process()`** in `src/input/mod.rs` (line 121). Add edge detection state to the `InputDispatcher` struct if needed (e.g., `prev_your_button: [bool; 2]`). Emit the event when conditions are met.

3. **Dispatch in `Shell::tick()`** in `src/shell/mod.rs` (line 138). Add a match arm in the event dispatch loop:
   ```rust
   InputEvent::YourNewEvent { hand, .. } => {
       self.dashboard.handle_your_event(/* args */);
   }
   ```

4. **Handle in Dashboard** -- either in `Dashboard::render()` via a pending flag (like `click_pending`), or as a direct method on `Dashboard`.

## How to Add a Config Setting

1. **Add field** to the appropriate config struct in `src/config/mod.rs` (PanelConfig, AudioConfig, DisplayConfig, or ShellConfig). Add a default value in the corresponding `Default` impl.

2. **Add UI** in `render_settings_content()` in `src/shell/dashboard.rs` (line 599). Use egui widgets: `ui.checkbox()`, `ui.add(egui::Slider::new())`, `egui::ComboBox`, etc.

3. **Use the value** wherever needed via `dashboard.config.section.field`. The config is accessible as `self.config` inside Dashboard and `self.dashboard.config` from Shell.

4. **Save** is already handled: the Settings tab has a Save button that calls `config.save()`, which writes TOML to `~/.clearxr/config.toml`.

## File Ownership

| Task | Files to touch |
|------|----------------|
| Add dashboard tab | `src/shell/dashboard.rs` (DashboardTab enum + tab bar + content match + render helper + state fields) |
| Change grab behavior | `src/shell/mod.rs` (grab continue/release section, lines 226-283) |
| Add controller button | `src/input/mod.rs` (HandState fields + InputDispatcher edge detection), `src/xr_session.rs` (read from OpenXR action) |
| Change panel shader | `shaders/panel.frag`, `shaders/panel.vert` (recompile to .spv via `build.rs`) |
| Change egui shader | `shaders/egui.frag`, `shaders/egui.vert` (recompile to .spv via `build.rs`) |
| Add config setting | `src/config/mod.rs` (struct + Default), `src/shell/dashboard.rs` (settings tab UI) |
| Change screen capture | `src/capture/screen_capture.rs` |
| Add new panel type | `src/launcher_panel.rs` (or new file), `src/shell/mod.rs` (add to panels list) |
| Change input dispatch | `src/input/mod.rs` (InputDispatcher::process), `src/shell/mod.rs` (event dispatch loop) |
| Add notification | Call `dashboard.notifications.push(Notification::info("title", "body"))` from Shell |
| Change panel transform | `src/panel/mod.rs` (PanelTransform, hit_test) |
| Add Vulkan resource | `src/vk_backend.rs` (VkBackend), remember to add cleanup in `destroy()` |

## Constants

| Constant | Location | Value |
|----------|----------|-------|
| `DASHBOARD_PANEL_ID` | `src/shell/dashboard.rs` | `PanelId::new(1)` |
| `SCREEN_PANEL_ID` | `src/shell/dashboard.rs` | `PanelId::new(2)` |
| `TRIGGER_THRESHOLD` | `src/input/mod.rs` | `0.5` |
| `GRIP_THRESHOLD` | `src/input/mod.rs` | `0.7` |
| `GRAB_MARGIN` | `src/input/mod.rs` | `0.15` (15% from panel edge) |
| Dashboard texture size | `src/shell/dashboard.rs` | 2048 x 1280 px |
| Default panel position | `src/config/mod.rs` | `[0.0, 1.6, -2.5]` (eye height, 2.5m forward) |
