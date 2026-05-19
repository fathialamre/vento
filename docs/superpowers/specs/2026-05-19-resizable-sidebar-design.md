# Resizable sidebar with two-direction scroll

## Goal

Make the sidebar content panel (the wider panel holding Collections /
Environments / History) resizable against the main request area, and let its
contents scroll both vertically and horizontally when items are wider than the
current panel width.

## Scope

In scope:

- Resizable boundary between the sidebar content panel and the main `SidebarInset`.
- Two-direction scrolling inside Collections, Environments, and History lists.
- Apply only to the desktop layout (`md:` breakpoint and up). The mobile sheet
  variant is unchanged.

Out of scope:

- Resizing the narrow icon-rail (it stays fixed at `--sidebar-width-icon`).
- Persisting the resized width across app restarts (resets each session).
- Touch / pointer gestures beyond mouse drag.

## Design

### Width state

- Hold the sidebar width in `useState<number>` in `App.tsx`, default `350`.
- Bounds: min `280`, max `800`. Clamp on every update.
- Pass to `SidebarProvider` via inline style:
  `style={{ "--sidebar-width": ${width}px }}`. The shadcn sidebar reads
  `--sidebar-width` for its track width; because the icon rail is sized off the
  separate `--sidebar-width-icon`, only the content panel grows when width
  changes.

### Drag handle

- A 4px-wide vertical hit strip sits flush against the right edge of the outer
  `Sidebar` element in `app-sidebar.tsx`. It is positioned absolute so it does
  not push layout, and it sits above the sidebar border with a small invisible
  margin on each side to make grabbing easier.
- Visuals: transparent by default, picks up `bg-border` on hover and
  `bg-primary/40` while dragging, matching the look of
  `src/components/ui/resizable.tsx`.
- Cursor: `col-resize` on hover and while dragging.
- Drag logic lives in `App.tsx` (since width state is there):
  - `onMouseDown` captures the starting clientX and starting width, sets a
    `dragging` flag.
  - A `mousemove` listener (added to `window` for the duration of the drag)
    computes `next = clamp(start + (e.clientX - startX), 280, 800)` and updates
    state.
  - `mouseup` removes listeners and clears the flag.
  - While dragging, set `document.body.style.cursor = "col-resize"` and
    `userSelect = "none"`; restore on release.
- A render prop or callback is passed from `App.tsx` down into `AppSidebar` to
  attach the `onMouseDown` handler to the strip.

### Two-direction scroll

The shadcn `SidebarContent` already uses `overflow-auto`, which yields both
scroll axes when the inner content is large enough. The current panels prevent
horizontal overflow by truncating or hard-breaking text. The fix is to relax
those constraints so natural word wrap handles normal cases, and unbreakable
long strings (e.g. URLs without separators) push horizontal overflow.

Changes:

- `app-sidebar.tsx`
  - Outer `Sidebar` currently has `overflow-hidden`. Keep `overflow-hidden` on
    the icon-rail child only; the content-panel child must allow inner
    horizontal scroll.
  - History row (`app-sidebar.tsx:202`): drop `break-all` and `line-clamp-2`
    from the URL `<span>`; rely on default `overflow-wrap: anywhere`-equivalent
    wrapping. Leave the row container as `w-full`.
- `collections-panel.tsx`
  - Line 122: drop `overflow-hidden` on the row span wrapper.
  - Line 126: drop `truncate` from the item name; allow wrap.
- `environments-panel.tsx`
  - Line 161: drop `truncate` on the environment name span.

Together these mean: normal-length names wrap inside the current width; an
abnormally long token forces horizontal scroll on `SidebarContent`, which the
user can scrub with the trackpad/scrollbar.

### Touch points summary

| File | Change |
|------|--------|
| `src/App.tsx` | Add `sidebarWidth` state + drag handlers; pass `--sidebar-width` style to `SidebarProvider`; pass `onResizeStart` prop to `AppSidebar`. |
| `src/components/app-sidebar.tsx` | Accept `onResizeStart` prop; render drag-handle strip on the outer `Sidebar`; relax `overflow-hidden` so the content panel can scroll horizontally; relax history row text constraints. |
| `src/components/collections-panel.tsx` | Drop `overflow-hidden` + `truncate` on the row name. |
| `src/components/environments-panel.tsx` | Drop `truncate` on the env name. |

## Testing

Manual (this is a UI change; no automated tests exist for these components):

- Drag the handle left and right — sidebar width updates smoothly, clamps at
  280 and 800, main panel reflows.
- During drag, the cursor stays `col-resize` and text doesn't get selected
  accidentally.
- Add a collection / request / environment / history entry with an absurdly
  long unbroken URL — horizontal scrollbar appears inside the sidebar, normal
  vertical scroll still works.
- Normal-length names wrap to a second line instead of truncating.
- Collapse the sidebar to icon-only via the existing `SidebarTrigger` — the
  drag handle hides along with the content panel, and re-expanding restores
  the last width.
- App restart — width resets to 350 (no persistence by design).

## Risks / Trade-offs

- Removing `truncate` means lists with very long names will take more vertical
  space. Acceptable because horizontal scroll is now an escape hatch and the
  user explicitly asked for "smart wrap".
- The custom drag handle bypasses the shadcn `Resizable` primitive because the
  shadcn `Sidebar` already controls its width via `--sidebar-width`; wiring a
  `ResizablePanelGroup` around it would fight that mechanism. The hand-rolled
  handler is small (~30 lines) and mirrors the visual treatment of the
  existing `ResizableHandle`.
