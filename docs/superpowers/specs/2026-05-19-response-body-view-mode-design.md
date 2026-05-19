# Response Body View Mode Selector

## Overview

Add a dropdown to the response body panel that lets users switch between three rendering modes: JSON tree viewer, Pretty (formatted JSON text), and Raw (plain text). The dropdown sits in a thin toolbar row between the ResponseBar and the body content.

## Data Model

Add `bodyViewMode: "json" | "pretty" | "raw"` to the `Tab` type in `App.tsx`. Default is `"json"`. Stored in React state per tab; no persistence to localStorage or DB.

```ts
type Tab = {
  // ... existing fields
  bodyViewMode: "json" | "pretty" | "raw";
};
```

`newTab()` sets `bodyViewMode: "json"` in its defaults.

## Auto-Detection

In `sendRequest()`, after the response body is received, auto-set `bodyViewMode`:
- If `JSON.parse(body)` succeeds → `"json"`
- Otherwise → `"raw"`

The user can override at any time via the dropdown.

## BodyToolbar Component

A thin toolbar row rendered between `ResponseBar` and the body content area. Visible only when `responseTab === "body"` and a response exists.

Contains a shadcn `<Select>` component (left-aligned, compact):

```
[ JSON ▾ ]
```

Options:
| Value | Label |
|-------|-------|
| `json` | JSON |
| `pretty` | Pretty |
| `raw` | Raw |

Props:
```ts
function BodyToolbar({
  mode,
  onChange,
}: {
  mode: "json" | "pretty" | "raw";
  onChange: (m: "json" | "pretty" | "raw") => void;
})
```

## ResponseView Modes

`ResponseView` receives a `mode` prop in addition to `raw`.

| Mode | Behavior |
|------|----------|
| `json` | Renders `<JsonViewer>`. Falls back silently to Raw if `JSON.parse` fails. |
| `pretty` | Renders `JSON.stringify(parsed, null, 2)` in a `<pre>`. Falls back to Raw if not valid JSON. |
| `raw` | Renders raw string in a `<pre>` with `whitespace-pre-wrap break-words`. |

## Affected Files

- `src/App.tsx` — Tab type, newTab defaults, sendRequest auto-detection, BodyToolbar component, ResponseView updated signature, render wiring.

No new files required.

## Out of Scope

- Persisting view mode across sessions
- Syntax highlighting in Pretty mode
- HTML preview mode
