# Track 03: Launcher And App Model

## Mission

Turn the launcher into a narrow, dependable core flow instead of a nice-looking but partially misleading grid.

## Why This Track Exists

The launcher is currently the most visible product surface, but its taxonomy and data model are not yet trustworthy enough to carry the product.

## MVP Requirements

### 1. Narrow the launcher promise

Status: **Done** — removed fake categories and badges.

- [x] unsupported categories are removed or disabled
- [x] fake or stubbed "recent" behavior is gone
- [x] heuristic VR classification is not presented as authoritative
- [x] the product copy matches what the launcher actually knows

### 2. Make content launching dependable

Status: **Done** — edge detection, double-launch prevention, status notifications.

- [x] one click launches one title
- [x] launch success and failure are communicated clearly
- [x] prior app replacement or forced exit is surfaced explicitly
- [x] the user is not left guessing what happened after launch

### 3. Add minimum library usefulness

Status: **Done** — search works, alphabetical ordering, honest empty state.

- [x] search works reliably
- [x] ordering is deterministic
- [x] at least one real organization mode exists (alphabetical, installed only)
- [x] empty states are honest and helpful

## 1.0 Requirements

- stronger metadata model
- real recents/favorites/pinned content
- app lifecycle status tracking
- basic curation and junk filtering

## 2.0 Requirements

- richer metadata
- per-app launch recipes
- collections and richer curation
- history and session restore

## Suggested File Ownership

- `src/app/**`
- `src/shell/mod.rs`
- `ui/launcher-v2.html`

## Dependencies

- interaction track for final input polish
- product framing track for scope decisions

## Risks

- trying to preserve the current taxonomy even if the data cannot support it
- spending time on card aesthetics before fixing launch truthfulness

## Explicit Non-Goals For MVP

- storefront-grade metadata
- cloud sync
- editorial discovery systems
- rich artwork ingestion pipelines

## Acceptance Checklist

- [x] launcher categories are honest
- [x] one press launches once
- [x] search works
- [x] the user can tell whether launch succeeded or failed


