# Track 04: Settings Truthfulness

## Mission

Reduce the settings surface until every visible control is truthful.

## Why This Track Exists

Misleading settings are one of the fastest ways to destroy trust. If the product says "saved" while behavior does not change, the shell feels fake.

## MVP Requirements

### 1. Eliminate placebo settings

Status: **Done** — stripped to 4 real controls.

- [x] every visible setting either:
  - applies immediately,
  - clearly says it applies on next launch, or
  - is removed

### 2. Respect saved defaults on startup

Status: **Done** — default_view, opacity, FPS, haptics all wired.

- [x] saved startup view is honored
- [x] retained panel defaults that remain user-facing are honored
- [x] retained anchor defaults that remain user-facing are honored
- [x] fallback behavior is understandable

### 3. Shrink settings to MVP size

Status: **Done** — 4 controls remain: default_view, opacity, FPS, haptics.

- [x] startup view
- [x] panel opacity
- [x] show/hide FPS
- [x] haptics toggle
- Removed: output device picker, mic toggles, theme packs, advanced theater controls

## 1.0 Requirements

- persistent shell state beyond the minimum
- more deliberate organization of settings
- clearer session and startup preferences

## 2.0 Requirements

- profiles and presets
- richer per-app defaults
- accessibility and comfort preference systems

## Suggested File Ownership

- `src/config/**`
- `src/shell/mod.rs`
- `ui/settings.html`

## Dependencies

- product framing for deciding what stays visible
- interaction track for making settings interaction safe

## Risks

- keeping too many controls because they "might be useful later"
- preserving the current settings panel shape even if it is too large for MVP

## Explicit Non-Goals For MVP

- full OS-like settings breadth
- audio routing control
- theme system expansion
- import/export profiles

## Acceptance Checklist

- [x] no visible setting lies
- [x] startup restore behaves as promised
- [x] settings surface is smaller, sharper, and obviously real


