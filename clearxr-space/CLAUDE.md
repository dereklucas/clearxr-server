# ClearXR Space

VR shell (SteamVR replacement) built on OpenXR + Vulkan + egui.

## Build & Test
- Build: `cargo run --all-features`
- Test: `cargo test`
- Target: Windows, XR headset via CloudXR

## Key Rules
- **egui for ALL interaction** -- use egui Button/Slider/etc, never hardcoded UV hit zones
- **pointer_leave() every frame** -- clear all egui renderers at start of tick(), InputDispatcher re-establishes hover
- **One press = one action** -- trigger edge detection in InputDispatcher, never raw `trigger > 0.5` for clicks
- **GPU renderer for large panels** -- EguiGpuRenderer for dashboard (2048x1280), CPU EguiRenderer only for tiny panels
- **Screen capture stays zero-copy** -- DXGI -> staging -> Vulkan, no CPU pixel conversion

## Module Map
- `src/shell/mod.rs` -- Shell: thin wrapper around Dashboard + InputDispatcher
- `src/shell/dashboard.rs` -- Dashboard: unified panel with tabs (Launcher/Desktop/Settings), grab bar, FPS, notifications
- `src/ui/egui_gpu_renderer.rs` -- GPU egui renderer: tessellate -> Vulkan render pass -> panel texture
- `src/ui/egui_renderer.rs` -- CPU egui renderer: software rasterizer for small panels (FPS counter)
- `src/input/mod.rs` -- InputDispatcher: controller -> ray hit-test -> InputEvents (pointer/grab/click)
- `src/panel/mod.rs` -- PanelId, PanelTransform, PanelAnchor: geometry + hit-test math
- `src/launcher_panel.rs` -- LauncherPanel: Vulkan texture + staging + descriptor set + draw
- `src/capture/screen_capture.rs` -- DXGI background thread capture + mouse input injection
- `src/config/mod.rs` -- Config: TOML persistence at ~/.clearxr/config.toml
- `src/app/mod.rs` -- Game scanner (Steam) + app launcher
- `src/xr_session.rs` -- OpenXR lifecycle: session, swapchains, controller input, frame submission
- `src/renderer.rs` -- Vulkan scene renderer: swapchains, pipelines, composition layers

## Adding a Dashboard Tab
See ARCHITECTURE.md for the step-by-step recipe.
