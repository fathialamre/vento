# Response Body View Mode Selector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a dropdown toolbar below the ResponseBar that lets users switch the body rendering between JSON tree viewer, Pretty (indented JSON text), and Raw (plain text).

**Architecture:** All changes are confined to `src/App.tsx`. A new `BodyViewMode` type alias is added near the existing type definitions. A new `BodyToolbar` component renders the `<Select>` dropdown. `ResponseView` gains a `mode` prop and branches on it. `sendRequest()` auto-detects the mode from the response body.

**Tech Stack:** React, TypeScript, shadcn/ui Select (`src/components/ui/select.tsx`)

---

## File Map

| File | Change |
|------|--------|
| `src/App.tsx` | Add `BodyViewMode` type, update `Tab` type, update `newTab()`, update `sendRequest()`, add `BodyToolbar` component, update `ResponseView` signature and body, wire `BodyToolbar` + `ResponseView` in response panel JSX |

No new files.

---

### Task 1: Add `BodyViewMode` type and extend `Tab`

**Files:**
- Modify: `src/App.tsx:100-123` (Tab type) and `src/App.tsx:126-146` (newTab)

- [ ] **Step 1: Add `BodyViewMode` type alias**

In `src/App.tsx`, find the line `type ResponseTab = "body" | "headers";` (currently line 100) and add the new type alias directly after it:

```ts
type ResponseTab = "body" | "headers";

type BodyViewMode = "json" | "pretty" | "raw";
```

- [ ] **Step 2: Add `bodyViewMode` field to `Tab` type**

In the `Tab` type definition, add the field after `responseTab`:

```ts
type Tab = {
  id: string;
  kind: TabKind;
  name: string;
  requestId?: string;
  environmentId?: number;
  isGlobals?: boolean;
  method: HttpMethod;
  url: string;
  params: QueryParam[];
  bottomTab: BottomTab;
  response: string;
  responseHeaders: [string, string][];
  responseSize: number | null;
  responseTab: ResponseTab;
  bodyViewMode: BodyViewMode;   // ← add this line
  error: string;
  loading: boolean;
  status: number | null;
  duration_ms: number | null;
};
```

- [ ] **Step 3: Set default in `newTab()`**

In `newTab()`, add `bodyViewMode: "json"` to the returned object:

```ts
function newTab(overrides?: Partial<Tab>): Tab {
  tabCounter += 1;
  return {
    id: `tab-${Date.now().toString(36)}-${tabCounter}`,
    kind: "request",
    name: "New Request",
    method: "GET",
    url: "",
    params: [],
    bottomTab: "params",
    response: "",
    responseHeaders: [],
    responseSize: null,
    responseTab: "body",
    bodyViewMode: "json",   // ← add this line
    error: "",
    loading: false,
    status: null,
    duration_ms: null,
    ...overrides,
  };
}
```

- [ ] **Step 4: Verify TypeScript compiles**

```bash
cd /Users/alamre/dev/vento && npx tsc --noEmit
```

Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add src/App.tsx
git commit -m "feat: add BodyViewMode type and bodyViewMode field to Tab"
```

---

### Task 2: Auto-detect view mode in `sendRequest()`

**Files:**
- Modify: `src/App.tsx` — `sendRequest()` success branch (the first `setTabs` call after `invoke`)

- [ ] **Step 1: Detect JSON and set `bodyViewMode` in the success `setTabs` call**

Find the `setTabs` call inside the `try` block of `sendRequest()` (after the `invoke` call). It currently sets `loading`, `response`, `responseHeaders`, `responseSize`, `status`, `duration_ms`. Replace that block with:

```ts
let bodyViewMode: BodyViewMode = "raw";
try {
  JSON.parse(res.body);
  bodyViewMode = "json";
} catch {
  // not JSON — leave as "raw"
}
setTabs((prev) =>
  prev.map((t) =>
    t.id === activeId
      ? {
          ...t,
          loading: false,
          response: res.body,
          responseHeaders: res.headers,
          responseSize: res.size_bytes,
          status,
          duration_ms: duration,
          bodyViewMode,
        }
      : t,
  ),
);
```

- [ ] **Step 2: Verify TypeScript compiles**

```bash
npx tsc --noEmit
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add src/App.tsx
git commit -m "feat: auto-detect body view mode after response"
```

---

### Task 3: Update `ResponseView` to render based on mode

**Files:**
- Modify: `src/App.tsx` — `ResponseView` function (currently lines 911–927)

- [ ] **Step 1: Replace the entire `ResponseView` function**

Find and replace the current `ResponseView` function with:

```tsx
function ResponseView({ raw, mode }: { raw: string; mode: BodyViewMode }) {
  if (mode === "json") {
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      return (
        <pre className="overflow-auto rounded-md border bg-card p-4 font-mono text-sm whitespace-pre-wrap break-words text-foreground">
          {raw}
        </pre>
      );
    }
    return (
      <div className="overflow-auto rounded-md border bg-card p-4">
        <JsonViewer data={parsed} />
      </div>
    );
  }

  if (mode === "pretty") {
    let formatted = raw;
    try {
      formatted = JSON.stringify(JSON.parse(raw), null, 2);
    } catch {
      // not JSON — show raw as-is
    }
    return (
      <pre className="overflow-auto rounded-md border bg-card p-4 font-mono text-sm whitespace-pre-wrap break-words text-foreground">
        {formatted}
      </pre>
    );
  }

  return (
    <pre className="overflow-auto rounded-md border bg-card p-4 font-mono text-sm whitespace-pre-wrap break-words text-foreground">
      {raw}
    </pre>
  );
}
```

- [ ] **Step 2: Verify TypeScript compiles**

```bash
npx tsc --noEmit
```

Expected: error on the `ResponseView` call site (it still passes only `raw`). That is expected — Task 5 fixes the call site.

- [ ] **Step 3: Commit**

```bash
git add src/App.tsx
git commit -m "feat: ResponseView renders json/pretty/raw modes"
```

---

### Task 4: Add `BodyToolbar` component and Select imports

**Files:**
- Modify: `src/App.tsx` — imports section and add new component after `ResponseBar`

- [ ] **Step 1: Add Select imports**

Find the existing imports at the top of `App.tsx`. Add the Select imports after the existing shadcn imports:

```ts
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
```

- [ ] **Step 2: Add `BodyToolbar` component**

Add this component directly after the closing brace of `ResponseBar` (before `HeadersView`):

```tsx
function BodyToolbar({
  mode,
  onChange,
}: {
  mode: BodyViewMode;
  onChange: (m: BodyViewMode) => void;
}) {
  return (
    <div className="flex items-center py-1">
      <Select value={mode} onValueChange={(v) => onChange(v as BodyViewMode)}>
        <SelectTrigger size="sm" className="w-[90px]">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="json">JSON</SelectItem>
          <SelectItem value="pretty">Pretty</SelectItem>
          <SelectItem value="raw">Raw</SelectItem>
        </SelectContent>
      </Select>
    </div>
  );
}
```

- [ ] **Step 3: Verify TypeScript compiles**

```bash
npx tsc --noEmit
```

Expected: still the call-site error from Task 3. No new errors.

- [ ] **Step 4: Commit**

```bash
git add src/App.tsx
git commit -m "feat: add BodyToolbar component with view mode select"
```

---

### Task 5: Wire `BodyToolbar` and updated `ResponseView` into the response panel

**Files:**
- Modify: `src/App.tsx` — response panel JSX inside the `activeTab.kind === "request"` render block

- [ ] **Step 1: Update the response panel JSX**

Find the block (inside the ResizablePanel for the response side):

```tsx
{activeTab.response && (
  <>
    <ResponseBar
      status={activeTab.status}
      durationMs={activeTab.duration_ms}
      sizeBytes={activeTab.responseSize}
      active={activeTab.responseTab ?? "body"}
      headersCount={activeTab.responseHeaders.length}
      onChange={(t) => updateActive({ responseTab: t })}
    />
    {(activeTab.responseTab ?? "body") === "body" ? (
      <ResponseView raw={activeTab.response} />
    ) : (
      <HeadersView headers={activeTab.responseHeaders} />
    )}
  </>
)}
```

Replace with:

```tsx
{activeTab.response && (
  <>
    <ResponseBar
      status={activeTab.status}
      durationMs={activeTab.duration_ms}
      sizeBytes={activeTab.responseSize}
      active={activeTab.responseTab ?? "body"}
      headersCount={activeTab.responseHeaders.length}
      onChange={(t) => updateActive({ responseTab: t })}
    />
    {(activeTab.responseTab ?? "body") === "body" ? (
      <>
        <BodyToolbar
          mode={activeTab.bodyViewMode}
          onChange={(m) => updateActive({ bodyViewMode: m })}
        />
        <ResponseView raw={activeTab.response} mode={activeTab.bodyViewMode} />
      </>
    ) : (
      <HeadersView headers={activeTab.responseHeaders} />
    )}
  </>
)}
```

- [ ] **Step 2: Verify TypeScript compiles with zero errors**

```bash
npx tsc --noEmit
```

Expected: no errors.

- [ ] **Step 3: Start the dev server and manually verify**

```bash
npm run tauri dev
```

Verify:
1. Send a request to a JSON API (e.g. `https://httpbin.org/get`). Dropdown shows **JSON**. Tree viewer renders.
2. Switch dropdown to **Pretty**. Body shows indented JSON in a `<pre>`.
3. Switch dropdown to **Raw**. Body shows raw JSON string in a `<pre>`.
4. Send a request to a plain-text endpoint. Dropdown auto-selects **Raw**. Pre renders plain text.
5. Manually switch to **JSON** on a non-JSON response — tree viewer falls back to plain pre without crashing.
6. Switch to **Headers** tab — toolbar disappears. Switch back to **Body** — toolbar reappears.
7. Open a second tab, send a different request — each tab maintains its own view mode independently.

- [ ] **Step 4: Commit**

```bash
git add src/App.tsx
git commit -m "feat: wire body view mode toolbar into response panel"
```
