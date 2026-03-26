# Track 02: Interaction Model

## Mission

Make shell interaction safe, predictable, and teachable.

## Why This Track Exists

This is the current biggest product blocker. If one trigger press can launch a title multiple times or repeatedly click the real desktop, the shell is not trustworthy.

## MVP Requirements

### 1. Fix click semantics everywhere

Status: **Done**

- [x] one trigger press produces one logical click - per-hand edge detection on all 6 surfaces
- [x] launcher, desktop, settings, tray, notifications, and keyboard all share the same activation semantics
- [x] no surface fires repeated actions just because trigger is held

### 2. Consolidate onto one input pipeline

Status: **Done**

- [x] one event pipeline owns hover, press, release, drag — InputDispatcher::process() is the single entry point
- [x] duplicated per-surface click logic removed — Shell dispatches events, no direct hit_test calls
- [x] thresholds and deadzones configured centrally — TRIGGER_THRESHOLD, GRIP_THRESHOLD, GRAB_MARGIN in input/mod.rs
- [x] basic interaction sequences test-covered — 21 input tests including edge detection, priority, grab

### 3. Make interactions discoverable

Status: **Done**

- [x] users can tell how to click, move panels, switch surfaces, and open system controls - toolbar hover, grab bar highlight, haptics
- [x] anchor changes and mode changes have visible feedback
- [x] hidden interaction lore is reduced

### 4. Desktop panel interaction

The desktop panel is a real product surface. It does not need a separate safety-mode requirement, but it does need to obey the same predictable shared interaction semantics as the rest of the shell.

## 1.0 Requirements

- standardized interaction grammar across all surfaces
- clearer onboarding and recovery affordances
- explicit close, dismiss, and recenter behaviors
- better comfort rules for panel placement and movement

### Hand tracking and gaze input (1.0)

Hand tracking is a 1.0 requirement, not MVP, but the implementation cost is low because the infrastructure already exists:

- `XR_EXT_hand_tracking` is already detected, enabled, and reading 26 joints per hand every frame
- The scene shader already renders hand skeletons (spheres + capsules)
- Joint data is already in `HandData` UBO and flows through the render pipeline
- The `ControllerState` abstraction can accept hand-derived input alongside controller input

**Target interaction model: visionOS-style gaze + pinch**

- **Gaze ray**: head forward direction (from XR view pose), or `XR_EXT_eye_gaze_interaction` if the headset supports real eye tracking
- **Pinch to click**: distance(thumb_tip, index_tip) < ~2cm triggers a click. Pinch is detected from existing joint positions (thumb tip = joint 4, index fingertip = joint 8 in OpenXR hand joint convention)
- **Gaze determines WHERE, pinch determines WHEN**: the gaze ray hits panels, pinch acts as trigger pull
- **Feeds into existing ControllerState**: Shell processes gaze+pinch the same as controller input - no new event model needed

**What this is NOT:**

- Not a hand-tracking-first redesign - controllers remain the primary input
- Not index-finger pointing - gaze is the pointer, not the fingertip
- Not dwell-click - pinch is the activation, not staring
- Not sent to games - gaze+pinch interaction is shell-only; games receive standard OpenXR controller/hand data from the runtime

**Implementation sketch:**

1. Enable hand tracking alongside controllers (currently mutually exclusive - hand tracking only activates when controllers are inactive)
2. Detect pinch gesture from joint 4 (thumb tip) and joint 8 (index tip) distance
3. Build a gaze ray from the XR view pose (or eye gaze extension if available)
4. When hands are tracked and controllers are not active: populate `ControllerState` from gaze ray + pinch state
5. Shell processes it identically to controller input

**Estimated effort:** 2-3 hours. No new abstractions, no new event model, no shader changes.

**Decision: pinch detection in-app vs visionOS client:**

Detect pinch in the shell itself (not from the visionOS streaming client) because:
- We already have all 26 joint positions per hand from OpenXR
- Works with any OpenXR runtime that supports hand tracking (Quest, Varjo, Ultraleap), not just visionOS
- No dependency on client-side gesture recognition or extra data channels
- Simpler, more portable, easier to tune thresholds

## 2.0 Requirements

- richer focus management
- accessibility-aware interaction modes (hand tracking as primary, head gaze for reduced mobility)
- multi-hand gestures (two-hand pinch to scale panels)
- per-game input profile customization

## Suggested File Ownership

- `src/shell/mod.rs`
- `src/input/mod.rs`
- `src/panel/mod.rs`
- related UI bridge code

## Dependencies

- low dependency on runtime work for the initial fix set

## Risks

- patching repeated-click bugs one surface at a time instead of unifying the model
- preserving too many hidden gestures in the name of "power-user" behavior

## Explicit Non-Goals For MVP

- advanced gestures (two-hand scale, wrist flick, etc.)
- hand-tracking-first UX (controllers remain primary for MVP; gaze+pinch is 1.0)
- dwell-click or head-gaze-only interaction
- complex shortcut systems
- deep focus-tree infrastructure
- forwarding hand/gaze input to launched games (games use standard OpenXR input)

## Acceptance Checklist

- [x] one press equals one action everywhere
- [x] panel drag/move works predictably
- [x] toolbar switching no longer repeats
- [x] desktop panel interaction follows the shared interaction model end-to-end — all panels route through InputDispatcher
