# Resizable sidebar with two-direction scroll — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user drag the boundary between the sidebar content panel
(Collections / Environments / History) and the main request area, and let those
lists scroll both vertically and horizontally when items exceed the current
panel width.

**Architecture:** Lift the sidebar width into React state on `App.tsx` and feed
it through the existing shadcn `SidebarProvider` via the
`--sidebar-width` CSS variable. A custom 4px drag-handle absolutely positioned
on the right edge of the outer `Sidebar` wires `mousedown` / `mousemove` /
`mouseup` to update that state, clamped to `[280, 800]`. Independently, relax
the `truncate` / `overflow-hidden` / `break-all` utilities in the three list
panels so natural word wrap handles normal text while pathological-length
strings push horizontal overflow on the already-`overflow-auto` `SidebarContent`.

**Tech Stack:** React 19, TypeScript, Tailwind CSS v4, shadcn UI primitives
(`Sidebar`, `SidebarProvider`). No new dependencies. No automated tests in
the repo — verification is `npm run build` (tsc + vite build) plus manual
browser smoke-test via `npm run dev`.

**Spec:** `docs/superpowers/specs/2026-05-19-resizable-sidebar-design.md`

---

## File map

| File | Role in this change |
|------|--------------------|
| `src/App.tsx` | Holds `sidebarWidth` state and drag handlers; passes width into `SidebarProvider` and `onResizeStart` callback into `AppSidebar`. |
| `src/components/app-sidebar.tsx` | Accepts `onResizeStart` prop; renders the drag-handle strip; relaxes `overflow-hidden` on the inner content sidebar; relaxes history-row text utilities. |
| `src/components/collections-panel.tsx` | Drops `overflow-hidden` + `truncate` on the row name so long names wrap / overflow. |
| `src/components/environments-panel.tsx` | Drops `truncate` on the environment name. |

No new files. No moves.

---

### Task 1: Hold sidebar width in App state and drive the CSS variable

**Files:**
- Modify: `src/App.tsx`

- [ ] **Step 1: Add width state and clamp helper near the other `useState` calls in `App()`**

  Insert just after the existing `useState` block (the one defining `layoutMode`), around `src/App.tsx:159`:

  ```tsx
  const MIN_SIDEBAR_WIDTH = 280;
  const MAX_SIDEBAR_WIDTH = 800;
  const DEFAULT_SIDEBAR_WIDTH = 350;

  const [sidebarWidth, setSidebarWidth] = useState<number>(DEFAULT_SIDEBAR_WIDTH);
  ```

  Place the three `const` declarations at module scope (above `export default function App()`) — they are constants and shouldn't recreate per render. Move them above the component, then keep the `useState` line inside the component.

- [ ] **Step 2: Pass the live width into `SidebarProvider`**

  Replace the existing provider opening tag at `src/App.tsx:435`:

  ```tsx
  <SidebarProvider style={{ "--sidebar-width": "350px" } as React.CSSProperties}>
  ```

  with:

  ```tsx
  <SidebarProvider
    style={{ "--sidebar-width": `${sidebarWidth}px` } as React.CSSProperties}
  >
  ```

- [ ] **Step 3: Type-check**

  Run: `npm run build`
  Expected: build succeeds; no TS errors.

- [ ] **Step 4: Commit**

  ```bash
  git add src/App.tsx
  git commit -m "App: drive sidebar width from state"
  ```

---

### Task 2: Add the drag handler in App and pass it down

**Files:**
- Modify: `src/App.tsx`
- Modify: `src/components/app-sidebar.tsx`

- [ ] **Step 1: Add the drag handler in `App()`**

  Insert just after the `sidebarWidth` state added in Task 1:

  ```tsx
  const handleSidebarResizeStart = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      e.preventDefault();
      const startX = e.clientX;
      const startWidth = sidebarWidth;

      const prevCursor = document.body.style.cursor;
      const prevUserSelect = document.body.style.userSelect;
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";

      function onMove(ev: MouseEvent) {
        const next = Math.min(
          MAX_SIDEBAR_WIDTH,
          Math.max(MIN_SIDEBAR_WIDTH, startWidth + (ev.clientX - startX)),
        );
        setSidebarWidth(next);
      }

      function onUp() {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
        document.body.style.cursor = prevCursor;
        document.body.style.userSelect = prevUserSelect;
      }

      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [sidebarWidth],
  );
  ```

- [ ] **Step 2: Pass the handler into `AppSidebar`**

  Update the `<AppSidebar … />` JSX at `src/App.tsx:436` to add one prop:

  ```tsx
  <AppSidebar
    history={history}
    methodColor={METHOD_COLOR}
    onSelectHistory={handleSelectHistory}
    onLoadRequest={handleLoadRequest}
    openRequestIds={new Set(tabs.map((t) => t.requestId).filter(Boolean) as string[])}
    activeRequestId={activeTab?.requestId}
    collectionsRevision={collectionsRevision}
    environmentsRevision={environmentsRevision}
    onOpenSettings={openSettings}
    onOpenEnvironment={handleOpenEnvironment}
    activeEnvId={activeEnvId}
    activeNavTitle={sidebarNav}
    onActiveNavChange={setSidebarNav}
    onResizeStart={handleSidebarResizeStart}
  />
  ```

- [ ] **Step 3: Add the prop to `AppSidebarProps` and render the drag handle**

  In `src/components/app-sidebar.tsx`:

  Add to the `AppSidebarProps` type (currently `app-sidebar.tsx:36-50`):

  ```tsx
  onResizeStart: (e: React.MouseEvent<HTMLDivElement>) => void;
  ```

  Destructure it in the `AppSidebar({ ... })` signature (currently `app-sidebar.tsx:52-67`), adding `onResizeStart` to the list of destructured props.

  Then add a drag-handle `<div>` as the **last child** inside the outermost `<Sidebar … >` (the one opening at `app-sidebar.tsx:89`), just before its closing `</Sidebar>` tag at `app-sidebar.tsx:233`:

  ```tsx
  <div
    role="separator"
    aria-orientation="vertical"
    onMouseDown={onResizeStart}
    className="absolute inset-y-0 -right-0.5 z-20 hidden w-1 cursor-col-resize bg-transparent transition-colors hover:bg-border active:bg-primary/40 md:block group-data-[collapsible=icon]:hidden"
  />
  ```

  Then, for the absolute positioning to work, add `relative` to the outermost `<Sidebar>`'s className. Update line `app-sidebar.tsx:91`:

  ```tsx
  className="relative overflow-hidden *:data-[sidebar=sidebar]:flex-row"
  ```

  (Just prepend `relative ` to the existing class string.)

- [ ] **Step 4: Type-check and run the dev server**

  Run: `npm run build`
  Expected: build succeeds.

  Run: `npm run dev`
  Expected: app loads. Hover the right edge of the sidebar — cursor turns into `col-resize`, edge highlights. Drag — sidebar resizes, clamps at ~280 and ~800. Release — cursor restores, text selection works again. Toggle the sidebar collapsed via the existing `SidebarTrigger` — the handle disappears.

- [ ] **Step 5: Commit**

  ```bash
  git add src/App.tsx src/components/app-sidebar.tsx
  git commit -m "sidebar: drag handle to resize content panel"
  ```

---

### Task 3: Allow horizontal overflow inside the content panel

**Files:**
- Modify: `src/components/app-sidebar.tsx`

- [ ] **Step 1: Remove the outer `overflow-hidden` that traps horizontal scroll**

  In `src/components/app-sidebar.tsx:91`, the outer `<Sidebar>` currently has `overflow-hidden`, which prevents the inner `SidebarContent`'s horizontal scrollbar from showing through. Replace:

  ```tsx
  className="relative overflow-hidden *:data-[sidebar=sidebar]:flex-row"
  ```

  with:

  ```tsx
  className="relative *:data-[sidebar=sidebar]:flex-row"
  ```

  (Reasoning: the inner icon-rail and content-panel `<Sidebar collapsible="none">` children already manage their own widths; the outer wrapper does not need to clip. The fixed icon rail stays fixed because its own width class is `w-[calc(var(--sidebar-width-icon)+1px)]!`.)

- [ ] **Step 2: Relax the History row text constraints**

  In `src/components/app-sidebar.tsx`, find the URL span at line `214`:

  ```tsx
  <span className="line-clamp-2 w-full break-all text-xs">
    {item.url}
  </span>
  ```

  Replace with:

  ```tsx
  <span className="w-full text-xs">
    {item.url}
  </span>
  ```

  (Drop `line-clamp-2` so very long URLs wrap to as many lines as needed, and drop `break-all` so the browser only forces a mid-token break when truly necessary — pathological single-token URLs now push horizontal overflow instead of being chopped invisibly.)

- [ ] **Step 3: Type-check + manual smoke**

  Run: `npm run build`
  Expected: build succeeds.

  In `npm run dev`: switch the sidebar to History, drag the sidebar narrow, confirm long URLs wrap onto multiple lines and that an absurdly long single-token URL (e.g. one without `/`, `?`, `&`) triggers a horizontal scrollbar at the bottom of the content panel.

- [ ] **Step 4: Commit**

  ```bash
  git add src/components/app-sidebar.tsx
  git commit -m "sidebar: allow horizontal scroll and wrap history urls"
  ```

---

### Task 4: Relax truncation in CollectionsPanel

**Files:**
- Modify: `src/components/collections-panel.tsx`

- [ ] **Step 1: Drop `overflow-hidden` on the row wrapper and `truncate` on the name**

  In `src/components/collections-panel.tsx:122`:

  ```tsx
  <span className="flex flex-1 items-center gap-1.5 overflow-hidden">
  ```

  becomes:

  ```tsx
  <span className="flex flex-1 items-center gap-1.5">
  ```

  And `src/components/collections-panel.tsx:126`:

  ```tsx
  <span className="flex-1 truncate text-sm font-normal">{item.name}</span>
  ```

  becomes:

  ```tsx
  <span className="flex-1 text-sm font-normal">{item.name}</span>
  ```

- [ ] **Step 2: Type-check + manual smoke**

  Run: `npm run build`
  Expected: build succeeds.

  In `npm run dev`: open Collections, confirm normal-length collection / request names wrap when the sidebar is narrow rather than being cut with `…`. Drag the sidebar wider — names lay out on one line again.

- [ ] **Step 3: Commit**

  ```bash
  git add src/components/collections-panel.tsx
  git commit -m "collections: wrap names instead of truncating"
  ```

---

### Task 5: Relax truncation in EnvironmentsPanel

**Files:**
- Modify: `src/components/environments-panel.tsx`

- [ ] **Step 1: Drop `truncate` on the env name button**

  In `src/components/environments-panel.tsx:161`:

  ```tsx
  className="flex-1 truncate text-left"
  ```

  becomes:

  ```tsx
  className="flex-1 text-left"
  ```

- [ ] **Step 2: Type-check + manual smoke**

  Run: `npm run build`
  Expected: build succeeds.

  In `npm run dev`: open Environments, confirm long environment names wrap; the count + "Active" badge stay aligned to the right.

- [ ] **Step 3: Commit**

  ```bash
  git add src/components/environments-panel.tsx
  git commit -m "environments: wrap names instead of truncating"
  ```

---

### Task 6: End-to-end verification

**Files:** none (manual)

- [ ] **Step 1: Full build**

  Run: `npm run build`
  Expected: succeeds.

- [ ] **Step 2: Manual checklist in `npm run dev`**

  Walk through each item:

  - Drag the sidebar handle left — width shrinks, clamps near 280px; main panel expands.
  - Drag right — width grows, clamps near 800px.
  - While dragging, cursor is `col-resize` everywhere on the page, and dragging over text doesn't select it.
  - Release the drag — cursor and text-selection behavior return to normal.
  - Collapse the sidebar (click the icon-rail `SidebarTrigger`) — drag handle hides; re-expand restores the most recent width.
  - In Collections: add a request with a very long name — it wraps onto multiple lines; the row remains clickable; an absurdly long unbreakable token forces a horizontal scrollbar inside the content panel only (not the whole page).
  - In Environments: same check with a long env name.
  - In History: send a request with a very long URL; the row wraps; an unbreakable URL triggers horizontal scroll.
  - Reload the app — sidebar width resets to 350 (no persistence by design).

- [ ] **Step 3: If anything in step 2 fails, file the deviation in the task notes and fix before declaring done.**
