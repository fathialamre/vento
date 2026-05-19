# Request Body Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add HTTP request body support to Vento — Raw JSON, Raw text, `application/x-www-form-urlencoded`, and `multipart/form-data` (with file uploads) — across the React UI, Rust backend, and SQLite schema.

**Architecture:** A discriminated-union `RequestBody` type lives on each tab; a `<BodyEditor>` dispatcher renders one of four specialized editor components based on the active variant. The Rust `send_request` command accepts an optional tagged-enum body and translates each variant into the appropriate `reqwest` call (body+header, `.form()`, or `.multipart()`). Saved requests gain `body_type` and `body` columns; multipart file rows store the absolute path (the file is re-read at send time).

**Tech Stack:** React 19 + TypeScript + Tailwind 4 (frontend), Rust + Tauri 2 + reqwest 0.12 (backend), SQLite via `tauri-plugin-sql` (storage), `@tauri-apps/plugin-dialog` for OS file picker (new dependency).

**Test posture:** The project has no JS/TS test framework today. Per the spec, none is added. Verification per task uses TypeScript build (`npm run build`), `cargo check`, and explicit manual smoke steps. A consolidated manual test plan runs at the end.

**Spec:** `docs/superpowers/specs/2026-05-19-request-body-design.md`

---

### Task 1: Body model and helpers

**Files:**
- Create: `src/lib/body.ts`

- [ ] **Step 1: Create the body model file**

Write `src/lib/body.ts`:

```ts
import { resolveVars, type VarMap } from "@/lib/interpolation";

export type BodyFieldKind = "text" | "file";

export type BodyFieldRow = {
  id: string;
  key: string;
  value: string;          // text value, or absolute file path when kind === "file"
  kind?: BodyFieldKind;   // multipart only; defaults to "text"
  enabled: boolean;
};

export type RequestBody =
  | { type: "none" }
  | { type: "json"; text: string }
  | { type: "text"; text: string; contentType: string }
  | { type: "form-urlencoded"; fields: BodyFieldRow[] }
  | { type: "multipart"; fields: BodyFieldRow[] };

export const EMPTY_BODY: RequestBody = { type: "none" };

let rowCounter = 0;
export function nextBodyRowId(): string {
  rowCounter += 1;
  return `b-${rowCounter}-${Date.now().toString(36)}`;
}

export function newBodyFieldRow(
  overrides?: Partial<BodyFieldRow>,
): BodyFieldRow {
  return {
    id: nextBodyRowId(),
    key: "",
    value: "",
    kind: "text",
    enabled: true,
    ...overrides,
  };
}

export function bodyHasContent(b: RequestBody): boolean {
  return b.type !== "none";
}

function resolveString(
  s: string,
  env: VarMap,
  globals: VarMap,
  collected: Set<string>,
): string {
  const r = resolveVars(s, env, globals);
  for (const u of r.unresolved) collected.add(u);
  return r.output;
}

export type ResolvedBody = {
  body: RequestBody;
  unresolved: string[];
};

export function resolveBody(
  body: RequestBody,
  env: VarMap,
  globals: VarMap,
): ResolvedBody {
  const unresolved = new Set<string>();
  let out: RequestBody;
  switch (body.type) {
    case "none":
      out = body;
      break;
    case "json":
      out = {
        type: "json",
        text: resolveString(body.text, env, globals, unresolved),
      };
      break;
    case "text":
      out = {
        type: "text",
        text: resolveString(body.text, env, globals, unresolved),
        contentType: resolveString(body.contentType, env, globals, unresolved),
      };
      break;
    case "form-urlencoded":
      out = {
        type: "form-urlencoded",
        fields: body.fields.map((f) => ({
          ...f,
          key: resolveString(f.key, env, globals, unresolved),
          value: resolveString(f.value, env, globals, unresolved),
        })),
      };
      break;
    case "multipart":
      out = {
        type: "multipart",
        fields: body.fields.map((f) => ({
          ...f,
          key: resolveString(f.key, env, globals, unresolved),
          // file paths are NOT interpolated
          value:
            f.kind === "file"
              ? f.value
              : resolveString(f.value, env, globals, unresolved),
        })),
      };
      break;
  }
  return { body: out, unresolved: Array.from(unresolved) };
}

// Serialization for DB. Strips React row ids on persist; regenerates on load.
type PersistedRow = Omit<BodyFieldRow, "id">;
type PersistedBody =
  | { type: "none" }
  | { type: "json"; text: string }
  | { type: "text"; text: string; contentType: string }
  | { type: "form-urlencoded"; fields: PersistedRow[] }
  | { type: "multipart"; fields: PersistedRow[] };

function toPersistedRow(r: BodyFieldRow): PersistedRow {
  const { id: _id, ...rest } = r;
  return rest;
}

export function toPersistedBody(body: RequestBody): PersistedBody {
  switch (body.type) {
    case "none":
    case "json":
    case "text":
      return body;
    case "form-urlencoded":
    case "multipart":
      return { ...body, fields: body.fields.map(toPersistedRow) };
  }
}

export function fromPersistedBody(input: unknown): RequestBody {
  if (!input || typeof input !== "object") return EMPTY_BODY;
  const v = input as { type?: unknown };
  switch (v.type) {
    case "none":
      return { type: "none" };
    case "json": {
      const p = input as { text?: unknown };
      return { type: "json", text: typeof p.text === "string" ? p.text : "" };
    }
    case "text": {
      const p = input as { text?: unknown; contentType?: unknown };
      return {
        type: "text",
        text: typeof p.text === "string" ? p.text : "",
        contentType:
          typeof p.contentType === "string" ? p.contentType : "text/plain",
      };
    }
    case "form-urlencoded":
    case "multipart": {
      const p = input as { fields?: unknown };
      const rawFields = Array.isArray(p.fields) ? p.fields : [];
      const fields: BodyFieldRow[] = rawFields.map((row: any) => ({
        id: nextBodyRowId(),
        key: typeof row?.key === "string" ? row.key : "",
        value: typeof row?.value === "string" ? row.value : "",
        kind: row?.kind === "file" ? "file" : "text",
        enabled: row?.enabled !== false,
      }));
      return v.type === "form-urlencoded"
        ? { type: "form-urlencoded", fields }
        : { type: "multipart", fields };
    }
    default:
      return EMPTY_BODY;
  }
}
```

- [ ] **Step 2: Type-check**

Run: `npm run build`
Expected: build succeeds (TypeScript reports 0 errors). If the existing build emits Vite output even with errors, look at the `tsc` line in the output — that's the gate.

- [ ] **Step 3: Commit**

```bash
git add src/lib/body.ts
git commit -m "feat(body): add RequestBody model, resolver, and persistence helpers"
```

---

### Task 2: Backend — RequestBody enum, `send_request` body argument, DB migration v5

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Add `multipart` to the reqwest feature list**

Open `src-tauri/Cargo.toml`. The `reqwest` line currently reads:

```toml
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "gzip", "brotli", "deflate"] }
```

Replace with:

```toml
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "gzip", "brotli", "deflate", "multipart"] }
```

- [ ] **Step 2: Add the `RequestBody` enum, helper types, and update `send_request`**

Open `src-tauri/src/lib.rs`.

Add the following types directly below the existing `QueryParam` block (around line 24):

```rust
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum FieldKind {
    Text,
    File,
}

impl Default for FieldKind {
    fn default() -> Self {
        FieldKind::Text
    }
}

#[derive(Deserialize)]
struct BodyField {
    key: String,
    value: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Deserialize)]
struct MultipartField {
    key: String,
    value: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    kind: FieldKind,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum RequestBody {
    None,
    Json {
        text: String,
    },
    Text {
        text: String,
        content_type: String,
    },
    FormUrlencoded {
        fields: Vec<BodyField>,
    },
    Multipart {
        fields: Vec<MultipartField>,
    },
}
```

Replace the `send_request` function (currently lines 63–99) with:

```rust
#[tauri::command]
async fn send_request(
    method: String,
    url: String,
    params: Option<Vec<QueryParam>>,
    body: Option<RequestBody>,
) -> Result<HttpResponse, String> {
    let method = Method::from_bytes(method.to_uppercase().as_bytes())
        .map_err(|e| format!("invalid method: {e}"))?;

    let final_url = match params.as_deref() {
        Some(p) if !p.is_empty() => build_url(&url, p),
        _ => url,
    };

    let client = reqwest::Client::builder()
        .user_agent("vento/0.1")
        .build()
        .map_err(|e| e.to_string())?;

    let mut req = client.request(method, &final_url);

    match body {
        None | Some(RequestBody::None) => {}
        Some(RequestBody::Json { text }) => {
            req = req
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(text);
        }
        Some(RequestBody::Text { text, content_type }) => {
            let ct = if content_type.is_empty() {
                "text/plain".to_string()
            } else {
                content_type
            };
            req = req.header(reqwest::header::CONTENT_TYPE, ct).body(text);
        }
        Some(RequestBody::FormUrlencoded { fields }) => {
            let kv: Vec<(String, String)> = fields
                .into_iter()
                .filter(|f| f.enabled && !f.key.is_empty())
                .map(|f| (f.key, f.value))
                .collect();
            req = req.form(&kv);
        }
        Some(RequestBody::Multipart { fields }) => {
            let mut form = reqwest::multipart::Form::new();
            for f in fields {
                if !f.enabled || f.key.is_empty() {
                    continue;
                }
                match f.kind {
                    FieldKind::Text => {
                        form = form.text(f.key, f.value);
                    }
                    FieldKind::File => {
                        if f.value.is_empty() {
                            continue;
                        }
                        let path = std::path::PathBuf::from(&f.value);
                        let file_name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("file")
                            .to_string();
                        let bytes = tokio::fs::read(&path).await.map_err(|e| {
                            if e.kind() == std::io::ErrorKind::NotFound {
                                format!("body file not found: {}", f.value)
                            } else {
                                format!("failed to read body file {}: {e}", f.value)
                            }
                        })?;
                        let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name);
                        form = form.part(f.key, part);
                    }
                }
            }
            req = req.multipart(form);
        }
    }

    let started = std::time::Instant::now();
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    let size_bytes = body.as_bytes().len() as u64;
    let duration_ms = started.elapsed().as_millis() as u64;

    Ok(HttpResponse {
        status,
        duration_ms,
        body,
        headers,
        size_bytes,
    })
}
```

- [ ] **Step 3: Add DB migration v5 for body columns**

Still in `src-tauri/src/lib.rs`, append a fifth `Migration` entry inside the `migrations` vec (at the end, just before the closing `];`):

```rust
        Migration {
            version: 5,
            description: "add_body_columns",
            sql: "ALTER TABLE saved_requests ADD COLUMN body_type TEXT;
            ALTER TABLE saved_requests ADD COLUMN body TEXT;",
            kind: MigrationKind::Up,
        },
```

- [ ] **Step 4: Verify Rust compiles**

Run: `cd src-tauri && cargo check 2>&1 | tail -30`
Expected: `Finished ... profile [...] target(s)` with no errors. Warnings about unused variants are acceptable until the frontend wires them up.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/lib.rs
git commit -m "feat(body): add RequestBody enum, send_request body arg, multipart support, migration v5"
```

---

### Task 3: Wire body into `Tab` state and the send path

**Files:**
- Modify: `src/App.tsx`

- [ ] **Step 1: Import body helpers**

In `src/App.tsx`, add these imports near the top (next to the other `@/lib` imports):

```tsx
import {
  EMPTY_BODY,
  resolveBody,
  type RequestBody,
} from "@/lib/body";
```

- [ ] **Step 2: Add `body` to the `Tab` type**

Find the `type Tab = { ... }` block (around line 113). Add a `body` field right after `params`:

```ts
  params: QueryParam[];
  body: RequestBody;
  bottomTab: BottomTab;
```

- [ ] **Step 3: Default body in `newTab`**

Find the `newTab` function (around line 140). Inside the returned object, add `body: EMPTY_BODY,` next to the `params: []` line:

```ts
    params: [],
    body: EMPTY_BODY,
    bottomTab: "params",
```

- [ ] **Step 4: Pass body to the Tauri command in the send path**

In the `try { ... }` block of the send handler (around line 422), after the existing `resolveVars` / `resolveVarsInList` calls and before the `invoke(...)` call, add:

```ts
      const { body: resolvedBody, unresolved: bodyUnresolved } = resolveBody(
        tab.body,
        envMap,
        globalsMap,
      );
```

Then extend the `unresolved` array to include body misses:

```ts
      const unresolved = Array.from(
        new Set([...urlUnresolved, ...paramUnresolved, ...bodyUnresolved]),
      );
```

Then update the `invoke` call to include the body:

```ts
      const res = await invoke<RustResponse>("send_request", {
        method: tab.method,
        url: resolvedUrl,
        params: activeParams.map((p, i) => ({
          key: p.key,
          value: resolvedValues[i],
          encode: p.encode,
        })),
        body: resolvedBody,
      });
```

- [ ] **Step 5: Type-check**

Run: `npm run build`
Expected: build succeeds. All existing `newTab(...)` callsites are still valid because `body` has a default.

- [ ] **Step 6: Commit**

```bash
git add src/App.tsx
git commit -m "feat(body): add body to Tab state, resolve vars and pass to send_request"
```

---

### Task 4: `BodyTypePills` selector component

**Files:**
- Create: `src/components/body/body-type-pills.tsx`

- [ ] **Step 1: Create the pills component**

Write `src/components/body/body-type-pills.tsx`:

```tsx
import { cn } from "@/lib/utils";
import type { RequestBody } from "@/lib/body";

export type BodyTypeId = RequestBody["type"];

const PILLS: { id: BodyTypeId; label: string }[] = [
  { id: "none", label: "None" },
  { id: "json", label: "JSON" },
  { id: "text", label: "Text" },
  { id: "form-urlencoded", label: "form-urlencoded" },
  { id: "multipart", label: "multipart" },
];

export type BodyTypePillsProps = {
  active: BodyTypeId;
  onChange: (id: BodyTypeId) => void;
};

export function BodyTypePills({ active, onChange }: BodyTypePillsProps) {
  return (
    <div className="flex items-center gap-0.5 border-b">
      {PILLS.map((p) => {
        const isActive = active === p.id;
        return (
          <button
            key={p.id}
            type="button"
            onClick={() => onChange(p.id)}
            className={cn(
              "relative flex h-9 items-center border-b-2 border-transparent px-3 text-xs font-medium transition-colors",
              isActive
                ? "border-primary text-primary"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {p.label}
          </button>
        );
      })}
    </div>
  );
}
```

- [ ] **Step 2: Type-check**

Run: `npm run build`
Expected: build succeeds.

- [ ] **Step 3: Commit**

```bash
git add src/components/body/body-type-pills.tsx
git commit -m "feat(body): add BodyTypePills selector component"
```

---

### Task 5: `JsonBodyEditor` + `BodyEditor` dispatcher + enable Body tab

**Files:**
- Create: `src/components/body/json-body-editor.tsx`
- Create: `src/components/body/body-editor.tsx`
- Modify: `src/components/request-tabs.tsx`
- Modify: `src/App.tsx`

- [ ] **Step 1: Create `JsonBodyEditor`**

Write `src/components/body/json-body-editor.tsx`:

```tsx
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";

export type JsonBodyEditorProps = {
  text: string;
  onChange: (text: string) => void;
};

type ParseStatus =
  | { kind: "empty" }
  | { kind: "valid" }
  | { kind: "invalid"; message: string };

function parse(text: string): ParseStatus {
  if (text.trim() === "") return { kind: "empty" };
  try {
    JSON.parse(text);
    return { kind: "valid" };
  } catch (e) {
    return { kind: "invalid", message: (e as Error).message };
  }
}

export function JsonBodyEditor({ text, onChange }: JsonBodyEditorProps) {
  const [status, setStatus] = useState<ParseStatus>(() => parse(text));

  useEffect(() => {
    const id = window.setTimeout(() => setStatus(parse(text)), 300);
    return () => window.clearTimeout(id);
  }, [text]);

  function handleFormat() {
    try {
      const obj = JSON.parse(text);
      onChange(JSON.stringify(obj, null, 2));
    } catch {
      // ignore — status line already shows the parse error
    }
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-center justify-end border-b px-2 py-1">
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="h-7 px-2 text-xs"
          onClick={handleFormat}
          disabled={status.kind !== "valid"}
        >
          Format
        </Button>
      </div>
      <textarea
        value={text}
        onChange={(e) => onChange(e.currentTarget.value)}
        spellCheck={false}
        autoComplete="off"
        placeholder='{ "key": "value" }'
        className="flex-1 resize-none border-0 bg-transparent p-3 font-mono text-xs leading-5 outline-none focus-visible:ring-0"
      />
      <div className="border-t px-3 py-1 text-[11px]">
        {status.kind === "empty" && (
          <span className="text-muted-foreground">Empty</span>
        )}
        {status.kind === "valid" && (
          <span className="text-emerald-600">Valid JSON</span>
        )}
        {status.kind === "invalid" && (
          <span className="text-rose-500">Invalid: {status.message}</span>
        )}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Create `BodyEditor` dispatcher (handles `none` and `json` only for now)**

Write `src/components/body/body-editor.tsx`:

```tsx
import { BodyTypePills, type BodyTypeId } from "@/components/body/body-type-pills";
import { JsonBodyEditor } from "@/components/body/json-body-editor";
import { EMPTY_BODY, type RequestBody } from "@/lib/body";

export type BodyEditorProps = {
  body: RequestBody;
  onChange: (next: RequestBody) => void;
};

function blankBody(type: BodyTypeId): RequestBody {
  switch (type) {
    case "none":
      return EMPTY_BODY;
    case "json":
      return { type: "json", text: "" };
    case "text":
      return { type: "text", text: "", contentType: "text/plain" };
    case "form-urlencoded":
      return { type: "form-urlencoded", fields: [] };
    case "multipart":
      return { type: "multipart", fields: [] };
  }
}

export function BodyEditor({ body, onChange }: BodyEditorProps) {
  return (
    <div className="flex h-full min-h-0 flex-col">
      <BodyTypePills
        active={body.type}
        onChange={(t) => {
          if (t === body.type) return;
          onChange(blankBody(t));
        }}
      />
      <div className="min-h-0 flex-1 overflow-hidden">
        {body.type === "none" && (
          <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
            No body. Pick a type above.
          </div>
        )}
        {body.type === "json" && (
          <JsonBodyEditor
            text={body.text}
            onChange={(text) => onChange({ type: "json", text })}
          />
        )}
        {body.type === "text" && (
          <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
            Text body editor — coming next task.
          </div>
        )}
        {body.type === "form-urlencoded" && (
          <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
            form-urlencoded editor — coming next task.
          </div>
        )}
        {body.type === "multipart" && (
          <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
            multipart editor — coming next task.
          </div>
        )}
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Enable the Body tab and add the indicator dot**

Open `src/components/request-tabs.tsx`. Replace its contents with:

```tsx
import { cn } from "@/lib/utils";

export type BottomTab = "params" | "headers" | "body" | "auth";

export type RequestTabsProps = {
  active: BottomTab;
  onChange: (tab: BottomTab) => void;
  paramsCount: number;
  bodyActive: boolean;
};

const TABS: { id: BottomTab; label: string; enabled: boolean }[] = [
  { id: "params", label: "Params", enabled: true },
  { id: "headers", label: "Headers", enabled: false },
  { id: "body", label: "Body", enabled: true },
  { id: "auth", label: "Auth", enabled: false },
];

export function RequestTabs({
  active,
  onChange,
  paramsCount,
  bodyActive,
}: RequestTabsProps) {
  return (
    <div className="flex items-center gap-0.5 border-b">
      {TABS.map((t) => {
        const isActive = active === t.id;
        const count = t.id === "params" ? paramsCount : 0;
        const showDot = t.id === "body" && bodyActive;
        return (
          <button
            key={t.id}
            type="button"
            onClick={() => t.enabled && onChange(t.id)}
            disabled={!t.enabled}
            className={cn(
              "relative flex h-9 items-center gap-1.5 border-b-2 border-transparent px-3 text-xs font-medium transition-colors",
              isActive
                ? "border-primary text-primary"
                : "text-muted-foreground hover:text-foreground",
              !t.enabled &&
                "cursor-not-allowed opacity-40 hover:text-muted-foreground",
            )}
          >
            {t.label}
            {count > 0 && (
              <span className="rounded bg-accent px-1 py-0.5 text-[10px] font-bold text-accent-foreground">
                {count}
              </span>
            )}
            {showDot && (
              <span
                aria-hidden
                className={cn(
                  "ml-0.5 inline-block size-1.5 rounded-full",
                  isActive ? "bg-primary" : "bg-muted-foreground",
                )}
              />
            )}
          </button>
        );
      })}
    </div>
  );
}
```

- [ ] **Step 4: Render `BodyEditor` in `App.tsx` when the body tab is active**

In `src/App.tsx`, add these imports near the existing `@/components/...` block:

```tsx
import { BodyEditor } from "@/components/body/body-editor";
import { bodyHasContent } from "@/lib/body";
```

Find the `<RequestTabs ... />` usage in the request view (currently around lines 677–681). Replace it with:

```tsx
<RequestTabs
  active={activeTab.bottomTab ?? "params"}
  onChange={(t) => updateActive({ bottomTab: t })}
  paramsCount={(activeTab.params ?? []).filter((p) => p.enabled && p.key).length}
  bodyActive={bodyHasContent(activeTab.body)}
/>
```

Immediately after the existing `params` branch (currently around lines 682–687, ending with `</ParamsEditor>` closer + `)}`), add a `"body"` branch in the same enclosing `<div>`:

```tsx
{activeTab.bottomTab === "body" && (
  <BodyEditor
    body={activeTab.body}
    onChange={(b) => updateActive({ body: b })}
  />
)}
```

- [ ] **Step 5: Type-check**

Run: `npm run build`
Expected: build succeeds.

- [ ] **Step 6: Manual smoke test — JSON body end-to-end**

Run: `npm run tauri dev`

In the app:
1. Open a new request, set method to `POST`, URL to `https://httpbin.org/post`.
2. Click the `Body` tab. You should see the pills row with `None | JSON | Text | form-urlencoded | multipart`.
3. Click `JSON`. Editor appears with `Format` button and status line reading `Empty`.
4. Type `{"hello": "world"}` (one line, valid). Status line flips to `Valid JSON` after ~300 ms.
5. Click `Format`. Editor reformats to two-line pretty-printed JSON.
6. Click `Send`. Response `json` field should contain `{"hello":"world"}`, response `headers.Content-Type` should be `application/json`.
7. Confirm the `Body` tab shows a small dot next to its label.
8. Click the `Params` tab — body is preserved; click back to `Body` — content still there.
9. Click the `None` pill — editor area shows the empty placeholder. Dot on `Body` tab disappears.
10. Stop the dev server.

- [ ] **Step 7: Commit**

```bash
git add src/components/body/json-body-editor.tsx \
        src/components/body/body-editor.tsx \
        src/components/request-tabs.tsx \
        src/App.tsx
git commit -m "feat(body): JSON body editor + dispatcher, enable Body tab, indicator dot"
```

---

### Task 6: `RawTextBodyEditor`

**Files:**
- Create: `src/components/body/raw-text-body-editor.tsx`
- Modify: `src/components/body/body-editor.tsx`

- [ ] **Step 1: Create `RawTextBodyEditor`**

Write `src/components/body/raw-text-body-editor.tsx`:

```tsx
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const CONTENT_TYPES = [
  "text/plain",
  "text/xml",
  "application/xml",
  "text/html",
  "application/javascript",
];

export type RawTextBodyEditorProps = {
  text: string;
  contentType: string;
  onChange: (next: { text: string; contentType: string }) => void;
};

export function RawTextBodyEditor({
  text,
  contentType,
  onChange,
}: RawTextBodyEditorProps) {
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-center gap-2 border-b px-2 py-1">
        <label className="text-xs text-muted-foreground">Content-Type:</label>
        <Select
          value={contentType}
          onValueChange={(v) => onChange({ text, contentType: v })}
        >
          <SelectTrigger className="h-7 w-[220px] text-xs">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {CONTENT_TYPES.map((ct) => (
              <SelectItem key={ct} value={ct} className="text-xs">
                {ct}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <textarea
        value={text}
        onChange={(e) =>
          onChange({ text: e.currentTarget.value, contentType })
        }
        spellCheck={false}
        autoComplete="off"
        placeholder="raw body"
        className="flex-1 resize-none border-0 bg-transparent p-3 font-mono text-xs leading-5 outline-none focus-visible:ring-0"
      />
    </div>
  );
}
```

- [ ] **Step 2: Wire it into the dispatcher**

Open `src/components/body/body-editor.tsx`. Add the import:

```tsx
import { RawTextBodyEditor } from "@/components/body/raw-text-body-editor";
```

Replace the placeholder `body.type === "text"` branch with:

```tsx
{body.type === "text" && (
  <RawTextBodyEditor
    text={body.text}
    contentType={body.contentType}
    onChange={(next) => onChange({ type: "text", ...next })}
  />
)}
```

- [ ] **Step 3: Type-check**

Run: `npm run build`
Expected: build succeeds.

- [ ] **Step 4: Manual smoke test — raw text**

Run: `npm run tauri dev`

1. Open a `POST` request to `https://httpbin.org/post`.
2. Body tab → click `Text` pill.
3. Pick `application/xml` from the Content-Type select.
4. Type `<root><a>1</a></root>` into the textarea.
5. Click `Send`. Response `data` field should equal the XML, response `headers.Content-Type` should be `application/xml`.

- [ ] **Step 5: Commit**

```bash
git add src/components/body/raw-text-body-editor.tsx \
        src/components/body/body-editor.tsx
git commit -m "feat(body): raw text body editor with Content-Type select"
```

---

### Task 7: `FormBodyEditor` (form-urlencoded)

**Files:**
- Create: `src/components/body/form-body-editor.tsx`
- Modify: `src/components/body/body-editor.tsx`

- [ ] **Step 1: Create `FormBodyEditor`**

Write `src/components/body/form-body-editor.tsx`:

```tsx
import { useRef } from "react";
import { X } from "lucide-react";

import { cn } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import {
  nextBodyRowId,
  newBodyFieldRow,
  type BodyFieldRow,
} from "@/lib/body";

export type FormBodyEditorProps = {
  fields: BodyFieldRow[];
  onChange: (next: BodyFieldRow[]) => void;
};

export function FormBodyEditor({ fields, onChange }: FormBodyEditorProps) {
  const draftIdRef = useRef<string>(nextBodyRowId());

  function updateRow(id: string, patch: Partial<BodyFieldRow>) {
    onChange(fields.map((f) => (f.id === id ? { ...f, ...patch } : f)));
  }

  function deleteRow(id: string) {
    onChange(fields.filter((f) => f.id !== id));
  }

  function handleEdit(id: string, patch: Partial<BodyFieldRow>) {
    if (id === draftIdRef.current) {
      const materializedId = draftIdRef.current;
      draftIdRef.current = nextBodyRowId();
      onChange([
        ...fields,
        { ...newBodyFieldRow({ id: materializedId }), ...patch },
      ]);
      return;
    }
    onChange(fields.map((f) => (f.id === id ? { ...f, ...patch } : f)));
  }

  const blank = newBodyFieldRow({ id: draftIdRef.current });
  const rows: BodyFieldRow[] = [...fields, blank];

  return (
    <div className="overflow-hidden rounded-md border bg-card">
      <div className="sticky top-0 z-10 grid grid-cols-[36px_1fr_1fr_36px] items-center border-b bg-muted/60 text-xs font-medium text-muted-foreground backdrop-blur">
        <div className="border-r py-1.5 text-center">On</div>
        <div className="border-r px-2 py-1.5">Key</div>
        <div className="border-r px-2 py-1.5">Value</div>
        <div />
      </div>
      {rows.map((row) => {
        const isBlank = row.id === draftIdRef.current;
        return (
          <div
            key={row.id}
            className="group/row grid grid-cols-[36px_1fr_1fr_36px] items-center border-b border-border/60 last:border-b-0 hover:bg-accent/20"
          >
            <label className="flex h-8 cursor-pointer items-center justify-center border-r border-border/60">
              <RowCheckbox
                checked={row.enabled}
                onChange={(v) => updateRow(row.id, { enabled: v })}
                disabled={isBlank}
              />
            </label>
            <div className="border-r border-border/60">
              <CellInput
                value={row.key}
                placeholder="key"
                onChange={(v) => handleEdit(row.id, { key: v })}
              />
            </div>
            <div className="border-r border-border/60">
              <CellInput
                value={row.value}
                placeholder="value"
                onChange={(v) => handleEdit(row.id, { value: v })}
              />
            </div>
            <button
              type="button"
              onClick={() => deleteRow(row.id)}
              disabled={isBlank}
              className="flex h-8 items-center justify-center text-muted-foreground opacity-0 transition-opacity hover:text-foreground group-hover/row:opacity-100 disabled:!opacity-0"
              aria-label="Delete row"
            >
              <X className="size-3.5" />
            </button>
          </div>
        );
      })}
    </div>
  );
}

function CellInput({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <Input
      value={value}
      onChange={(e) => onChange(e.currentTarget.value)}
      placeholder={placeholder}
      autoComplete="off"
      spellCheck={false}
      className="h-8 rounded-none border-0 bg-transparent font-mono text-xs shadow-none focus-visible:ring-0 focus-visible:bg-background"
    />
  );
}

function RowCheckbox({
  checked,
  onChange,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={checked}
      onClick={() => !disabled && onChange(!checked)}
      disabled={disabled}
      className={cn(
        "flex size-4 items-center justify-center rounded-[4px] border border-muted-foreground/40 transition-colors",
        checked && !disabled
          ? "border-foreground bg-foreground text-background"
          : "bg-transparent",
        disabled && "cursor-not-allowed opacity-30",
        !disabled && "cursor-pointer hover:border-foreground",
      )}
    >
      {checked && (
        <svg viewBox="0 0 12 12" className="size-3" fill="none">
          <path
            d="M2.5 6.5L5 9L9.5 3.5"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      )}
    </button>
  );
}
```

- [ ] **Step 2: Wire it into the dispatcher**

In `src/components/body/body-editor.tsx`, add:

```tsx
import { FormBodyEditor } from "@/components/body/form-body-editor";
```

Replace the placeholder `body.type === "form-urlencoded"` branch with:

```tsx
{body.type === "form-urlencoded" && (
  <FormBodyEditor
    fields={body.fields}
    onChange={(fields) => onChange({ type: "form-urlencoded", fields })}
  />
)}
```

Wrap with the same outer padding as `ParamsEditor` if your existing layout adds one.

- [ ] **Step 3: Type-check**

Run: `npm run build`
Expected: build succeeds.

- [ ] **Step 4: Manual smoke test — form-urlencoded**

Run: `npm run tauri dev`

1. `POST https://httpbin.org/post`, body type `form-urlencoded`.
2. Add two rows: `name=vento`, `version=0.1`.
3. Send. Response `form` field should be `{"name": "vento", "version": "0.1"}`. Response `headers.Content-Type` should start with `application/x-www-form-urlencoded`.
4. Toggle the `name` row off via its checkbox. Send. Response `form` only has `version`.

- [ ] **Step 5: Commit**

```bash
git add src/components/body/form-body-editor.tsx \
        src/components/body/body-editor.tsx
git commit -m "feat(body): form-urlencoded editor (spreadsheet grid)"
```

---

### Task 8: Install Tauri dialog plugin for the file picker

**Files:**
- Modify: `package.json`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/capabilities/default.json`

- [ ] **Step 1: Install the JS plugin**

Run: `npm install @tauri-apps/plugin-dialog`
Expected: `package.json` and `package-lock.json` updated, `node_modules/@tauri-apps/plugin-dialog` exists.

- [ ] **Step 2: Add the Rust plugin to Cargo**

Open `src-tauri/Cargo.toml`. Inside `[dependencies]`, add:

```toml
tauri-plugin-dialog = "2"
```

- [ ] **Step 3: Register the plugin in the Tauri builder**

Open `src-tauri/src/lib.rs`. Find the `tauri::Builder::default()` chain (near the bottom). Add one `.plugin(...)` line just below the existing `tauri_plugin_opener::init()` plugin registration:

```rust
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
```

- [ ] **Step 4: Grant the dialog capability**

Open `src-tauri/capabilities/default.json`. In the `"permissions"` array, add `"dialog:default"`:

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Capability for the main window",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "opener:default",
    "dialog:default",
    "sql:default",
    "sql:allow-load",
    "sql:allow-execute",
    "sql:allow-select",
    "sql:allow-close"
  ]
}
```

- [ ] **Step 5: Verify Rust compiles**

Run: `cd src-tauri && cargo check 2>&1 | tail -20`
Expected: `Finished ...` with no errors.

- [ ] **Step 6: Commit**

```bash
git add package.json package-lock.json \
        src-tauri/Cargo.toml \
        src-tauri/src/lib.rs \
        src-tauri/capabilities/default.json
git commit -m "feat(body): add tauri-plugin-dialog for multipart file picker"
```

---

### Task 9: `MultipartBodyEditor`

**Files:**
- Create: `src/components/body/multipart-body-editor.tsx`
- Modify: `src/components/body/body-editor.tsx`

- [ ] **Step 1: Create `MultipartBodyEditor`**

Write `src/components/body/multipart-body-editor.tsx`:

```tsx
import { useRef } from "react";
import { X } from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

import { cn } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  nextBodyRowId,
  newBodyFieldRow,
  type BodyFieldRow,
} from "@/lib/body";

export type MultipartBodyEditorProps = {
  fields: BodyFieldRow[];
  onChange: (next: BodyFieldRow[]) => void;
};

export function MultipartBodyEditor({
  fields,
  onChange,
}: MultipartBodyEditorProps) {
  const draftIdRef = useRef<string>(nextBodyRowId());

  function updateRow(id: string, patch: Partial<BodyFieldRow>) {
    onChange(fields.map((f) => (f.id === id ? { ...f, ...patch } : f)));
  }

  function deleteRow(id: string) {
    onChange(fields.filter((f) => f.id !== id));
  }

  function handleEdit(id: string, patch: Partial<BodyFieldRow>) {
    if (id === draftIdRef.current) {
      const materializedId = draftIdRef.current;
      draftIdRef.current = nextBodyRowId();
      onChange([
        ...fields,
        { ...newBodyFieldRow({ id: materializedId }), ...patch },
      ]);
      return;
    }
    onChange(fields.map((f) => (f.id === id ? { ...f, ...patch } : f)));
  }

  async function pickFile(id: string) {
    const selected = await openDialog({ multiple: false, directory: false });
    if (typeof selected !== "string") return; // null when user cancels
    handleEdit(id, { value: selected, kind: "file" });
  }

  const blank = newBodyFieldRow({ id: draftIdRef.current });
  const rows: BodyFieldRow[] = [...fields, blank];

  return (
    <div className="overflow-hidden rounded-md border bg-card">
      <div className="sticky top-0 z-10 grid grid-cols-[36px_1fr_110px_1fr_36px] items-center border-b bg-muted/60 text-xs font-medium text-muted-foreground backdrop-blur">
        <div className="border-r py-1.5 text-center">On</div>
        <div className="border-r px-2 py-1.5">Key</div>
        <div className="border-r px-2 py-1.5">Type</div>
        <div className="border-r px-2 py-1.5">Value</div>
        <div />
      </div>
      {rows.map((row) => {
        const isBlank = row.id === draftIdRef.current;
        const kind = row.kind ?? "text";
        return (
          <div
            key={row.id}
            className="group/row grid grid-cols-[36px_1fr_110px_1fr_36px] items-center border-b border-border/60 last:border-b-0 hover:bg-accent/20"
          >
            <label className="flex h-8 cursor-pointer items-center justify-center border-r border-border/60">
              <RowCheckbox
                checked={row.enabled}
                onChange={(v) => updateRow(row.id, { enabled: v })}
                disabled={isBlank}
              />
            </label>
            <div className="border-r border-border/60">
              <CellInput
                value={row.key}
                placeholder="key"
                onChange={(v) => handleEdit(row.id, { key: v })}
              />
            </div>
            <div className="border-r border-border/60 px-1">
              <Select
                value={kind}
                onValueChange={(v) =>
                  // switching kind clears the value cell so a path doesn't
                  // accidentally become a text value (and vice versa)
                  handleEdit(row.id, {
                    kind: v as "text" | "file",
                    value: "",
                  })
                }
                disabled={isBlank}
              >
                <SelectTrigger className="h-8 rounded-none border-0 bg-transparent text-xs shadow-none focus:ring-0">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="text" className="text-xs">
                    Text
                  </SelectItem>
                  <SelectItem value="file" className="text-xs">
                    File
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="border-r border-border/60">
              {kind === "text" ? (
                <CellInput
                  value={row.value}
                  placeholder="value"
                  onChange={(v) => handleEdit(row.id, { value: v })}
                />
              ) : (
                <button
                  type="button"
                  onClick={() => pickFile(row.id)}
                  disabled={isBlank}
                  className="flex h-8 w-full items-center gap-2 px-2 text-left font-mono text-xs hover:bg-accent/40 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  <span className="text-muted-foreground">Choose file...</span>
                  {row.value && (
                    <span className="truncate text-foreground" title={row.value}>
                      {row.value}
                    </span>
                  )}
                </button>
              )}
            </div>
            <button
              type="button"
              onClick={() => deleteRow(row.id)}
              disabled={isBlank}
              className="flex h-8 items-center justify-center text-muted-foreground opacity-0 transition-opacity hover:text-foreground group-hover/row:opacity-100 disabled:!opacity-0"
              aria-label="Delete row"
            >
              <X className="size-3.5" />
            </button>
          </div>
        );
      })}
    </div>
  );
}

function CellInput({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <Input
      value={value}
      onChange={(e) => onChange(e.currentTarget.value)}
      placeholder={placeholder}
      autoComplete="off"
      spellCheck={false}
      className="h-8 rounded-none border-0 bg-transparent font-mono text-xs shadow-none focus-visible:ring-0 focus-visible:bg-background"
    />
  );
}

function RowCheckbox({
  checked,
  onChange,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={checked}
      onClick={() => !disabled && onChange(!checked)}
      disabled={disabled}
      className={cn(
        "flex size-4 items-center justify-center rounded-[4px] border border-muted-foreground/40 transition-colors",
        checked && !disabled
          ? "border-foreground bg-foreground text-background"
          : "bg-transparent",
        disabled && "cursor-not-allowed opacity-30",
        !disabled && "cursor-pointer hover:border-foreground",
      )}
    >
      {checked && (
        <svg viewBox="0 0 12 12" className="size-3" fill="none">
          <path
            d="M2.5 6.5L5 9L9.5 3.5"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      )}
    </button>
  );
}
```

- [ ] **Step 2: Wire it into the dispatcher**

In `src/components/body/body-editor.tsx`, add:

```tsx
import { MultipartBodyEditor } from "@/components/body/multipart-body-editor";
```

Replace the placeholder `body.type === "multipart"` branch with:

```tsx
{body.type === "multipart" && (
  <MultipartBodyEditor
    fields={body.fields}
    onChange={(fields) => onChange({ type: "multipart", fields })}
  />
)}
```

- [ ] **Step 3: Type-check**

Run: `npm run build`
Expected: build succeeds.

- [ ] **Step 4: Manual smoke test — multipart**

Prepare a small file: `printf 'hello multipart' > /tmp/vento-test.txt`

Run: `npm run tauri dev`

1. `POST https://httpbin.org/post`, body type `multipart`.
2. Row 1: kind `Text`, key `note`, value `hi`.
3. Row 2: kind `File`, key `upload`, click `Choose file...` → select `/tmp/vento-test.txt`. The chosen path should appear in the cell.
4. Send. Response `form.note` is `hi`, response `files.upload` is `hello multipart`. Response `headers.Content-Type` starts with `multipart/form-data; boundary=`.
5. Delete `/tmp/vento-test.txt` and send again. Red error banner shows `body file not found: /tmp/vento-test.txt`.

- [ ] **Step 5: Commit**

```bash
git add src/components/body/multipart-body-editor.tsx \
        src/components/body/body-editor.tsx
git commit -m "feat(body): multipart editor with file picker"
```

---

### Task 10: Persist body to `saved_requests`

**Files:**
- Modify: `src/lib/db.ts`
- Modify: `src/App.tsx`

- [ ] **Step 1: Extend the `SavedRequest` type and CRUD helpers**

Open `src/lib/db.ts`.

Add `body_type` and `body` to the `SavedRequest` type:

```ts
export type SavedRequest = {
  id: number;
  collection_id: number;
  folder_id: number | null;
  name: string;
  method: string;
  url: string;
  created_at: number;
  params: string | null;
  body_type: string | null;
  body: string | null;
};
```

Extend `createRequest` to accept and write the new columns:

```ts
export async function createRequest(input: {
  collection_id: number;
  folder_id: number | null;
  name: string;
  method: string;
  url: string;
  params: string | null;
  body_type: string | null;
  body: string | null;
}): Promise<number> {
  const db = await loadDb();
  const res = await db.execute(
    "INSERT INTO saved_requests (collection_id, folder_id, name, method, url, created_at, params, body_type, body) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    [
      input.collection_id,
      input.folder_id,
      input.name,
      input.method,
      input.url,
      Date.now(),
      input.params,
      input.body_type,
      input.body,
    ],
  );
  return res.lastInsertId ?? 0;
}
```

Extend `updateRequest`:

```ts
export async function updateRequest(input: {
  id: number;
  method: string;
  url: string;
  params: string | null;
  body_type: string | null;
  body: string | null;
}): Promise<void> {
  const db = await loadDb();
  await db.execute(
    "UPDATE saved_requests SET method = $1, url = $2, params = $3, body_type = $4, body = $5 WHERE id = $6",
    [input.method, input.url, input.params, input.body_type, input.body, input.id],
  );
}
```

Update the `SELECT` in `getCollectionsTree` to include the new columns:

```ts
    db.select<SavedRequest[]>(
      "SELECT id, collection_id, folder_id, name, method, url, created_at, params, body_type, body FROM saved_requests ORDER BY name ASC",
    ),
```

- [ ] **Step 2: Save body on auto-save and pass through to `createRequest` callers**

Open `src/App.tsx`. Find the auto-save `updateRequest({ ... })` call (around line 252) and extend the payload:

```ts
        await updateRequest({
          id: dbId,
          method: snapMethod,
          url: snapUrl,
          params:
            snapParams.length > 0
              ? JSON.stringify(toPersisted(snapParams))
              : null,
          body_type: activeTab.body.type === "none" ? null : activeTab.body.type,
          body:
            activeTab.body.type === "none"
              ? null
              : JSON.stringify(toPersistedBody(activeTab.body)),
        });
```

Add to the auto-save effect's dependency array: `activeTab?.body`.

Also extend the signature snapshot tracker (the `sig` const built around line 240) to include body so it triggers re-saves:

```ts
const sig = JSON.stringify([
  activeTab.method,
  activeTab.url,
  toPersisted(activeTab.params),
  activeTab.body,
]);
```

Add the import near the other `@/lib/body` import:

```ts
import {
  EMPTY_BODY,
  fromPersistedBody,
  resolveBody,
  toPersistedBody,
  type RequestBody,
} from "@/lib/body";
```

- [ ] **Step 3: Widen the `onLoadRequest` callback signature so callers can pass a saved body**

There are three signatures of the same callback that need an optional `body` field:

- `src/components/collections-panel.tsx:68` — `Props.onLoadRequest`
- `src/components/collections-panel.tsx:635` and `src/components/collections-panel.tsx:667` — internal `onLoadRequest` parameters
- `src/components/app-sidebar.tsx:40` — `AppSidebarProps.onLoadRequest`

In each, change the inline type from:

```ts
(req: { method: string; url: string; name?: string; requestId?: string; params?: string | null }) => void
```

to:

```ts
(req: {
  method: string;
  url: string;
  name?: string;
  requestId?: string;
  params?: string | null;
  body?: string | null;
}) => void
```

Then update the two tree-row invocations inside `collections-panel.tsx` (the ones at the existing lines `657` and `692`, which read `onLoadRequest({ method: r.method, url: r.url, name: r.name, requestId: ..., params: r.params })`) to also pass `body: r.body`:

```ts
onLoadRequest({
  method: r.method,
  url: r.url,
  name: r.name,
  requestId: `r:${r.id}`,
  params: r.params,
  body: r.body,
});
```

- [ ] **Step 4: Hydrate body in `handleLoadRequest`**

Open `src/App.tsx`. Find `function handleLoadRequest(req: { ... })` (around line 342). Widen its parameter type to match (add `body?: string | null` to the inline shape). Then, near where `persistedParams = fromPersisted(req.params)` is computed, add:

```ts
let restoredBody: RequestBody = EMPTY_BODY;
if (req.body) {
  try {
    restoredBody = fromPersistedBody(JSON.parse(req.body));
  } catch (e) {
    console.error("failed to parse saved body, falling back to none", e);
  }
}
```

Pass `body: restoredBody` into the `addTab({ ... })` call inside `handleLoadRequest`.

The history callsite at line 380 (`handleLoadRequest({ method: item.method, url: item.url, params: item.params })`) does NOT need `body` — history has no body column. The optional field allows this callsite to stay as-is.

- [ ] **Step 5: Update the one `createRequest` callsite**

There is exactly one direct caller: `src/components/collections-panel.tsx:262`. Add `body_type: null` and `body: null` to its argument object:

```ts
await createRequest({
  collection_id: ...,
  folder_id: ...,
  name: ...,
  method: ...,
  url: ...,
  params: ...,
  body_type: null,
  body: null,
});
```

New requests start empty; the auto-save flow from Step 2 will write the body once the user edits it.

- [ ] **Step 6: Type-check**

Run: `npm run build`
Expected: build succeeds.

- [ ] **Step 7: Manual smoke test — body persists across app restarts**

Run: `npm run tauri dev`

1. Create a new request in a collection. POST `https://httpbin.org/post`, body `JSON`, contents `{"persist": true}`. Send to confirm.
2. Save it (whatever the existing save action is — usually it auto-saves to the collection it lives in).
3. Close the tab.
4. Reopen the request from the collections panel. Body tab still has `JSON` selected and the same text.
5. Repeat for: raw text + custom Content-Type; form-urlencoded with two rows; multipart with one text + one file row.
6. Quit the app, restart, reopen — same data restored.
7. Switch a saved request's body type from `multipart` to `none` and confirm the row data does not reappear after restart (wipe on type switch persists correctly).

- [ ] **Step 8: Commit**

```bash
git add src/lib/db.ts src/App.tsx \
        src/components/collections-panel.tsx \
        src/components/app-sidebar.tsx
git commit -m "feat(body): persist body to saved_requests, hydrate on open"
```

---

### Task 11: End-to-end manual test pass

**Files:** none (verification only)

This task runs the full manual test plan from the spec to catch regressions across the body work.

- [ ] **Step 1: Start the dev app**

Run: `npm run tauri dev`

- [ ] **Step 2: Run the nine scenarios from the spec**

For each scenario, confirm the expected behavior. Cross off any that already passed during earlier tasks if their code hasn't changed since.

1. **JSON body POST round-trip.** POST `https://httpbin.org/post`, body JSON `{"a":1}`. Response `json.a === 1`, `headers.Content-Type === "application/json"`.
2. **Raw text custom Content-Type.** Body type Text, Content-Type `application/xml`, text `<x/>`. Response `headers.Content-Type === "application/xml"`, `data === "<x/>"`.
3. **form-urlencoded with two fields.** Rows `name=vento`, `version=0.1`. Response `form === {name: "vento", version: "0.1"}`.
4. **multipart text + file.** Prepare `/tmp/vento-test.txt` (`printf 'hello multipart' > /tmp/vento-test.txt`). One text row `note=hi`, one file row `upload=/tmp/vento-test.txt`. Response shows both, file content matches.
5. **Var interpolation.** Set an env var `name=vento` on the active environment. JSON body `{"who":"{{name}}"}` → response `json.who === "vento"`. Repeat for text body, form value, multipart text value.
6. **Save → reopen → restore.** For each of the four body types, save a request, close, reopen via the collections panel; body restored intact.
7. **Multipart file moved.** Send a multipart with a file, delete that file, send again → red banner `body file not found: <path>`, no crash.
8. **Method switch preserves body.** Configure a JSON body, switch method GET ↔ POST several times; body unchanged.
9. **JSON parse hint.** Type valid JSON → status `Valid JSON`. Edit to make it invalid → status `Invalid: <message>` within 300 ms. Click Format → reformats with 2-space indent (only when valid).

- [ ] **Step 3: If any scenario fails, file a fix as a follow-up**

For each failure, add a short note to the spec's `Open Questions` section (`docs/superpowers/specs/2026-05-19-request-body-design.md`) describing what failed and what you observed. Commit the spec edits separately so the implementation commits stay clean.

- [ ] **Step 4: Stop the dev server.**

- [ ] **Step 5: Commit (only if there are unstaged changes from prior tasks).**

If everything is already committed, skip. Otherwise:

```bash
git add -A
git commit -m "chore(body): final test-pass adjustments"
```

---

## Self-Review

Run through the spec's Decisions table and confirm each row maps to one of the tasks above:

| Decision | Implemented in |
|---|---|
| Body types: JSON / Text / form-urlencoded / multipart | Tasks 5, 6, 7, 9 |
| Plain `<textarea>` editor for raw text/JSON | Tasks 5, 6 |
| Auto Content-Type | Task 2 (Rust matches variant) + Task 6 (user-picked for text) |
| All HTTP methods can carry a body | Task 2 (no method gate) |
| Persistence: tab memory + saved_requests | Tasks 3 (memory), 10 (DB) |
| Multipart file path stored, re-read at send | Tasks 2 (Rust read) + 9 (path stored) + 10 (path persisted) |
| History unchanged | (no task — confirmed by absence of history schema changes) |
| Var interpolation in body | Tasks 1 (`resolveBody`), 3 (send path) |
| Body-tab dot indicator | Task 5 (`bodyActive` prop) |
| JSON: inline parse hint + Format button, never blocks send | Task 5 (`JsonBodyEditor`) |
| Switch body type wipes data | Task 5 (`blankBody` in `BodyEditor`) |
| No test framework added | (whole plan — manual verification only) |

All decisions covered. Component-name consistency verified: `BodyEditor`, `BodyTypePills`, `JsonBodyEditor`, `RawTextBodyEditor`, `FormBodyEditor`, `MultipartBodyEditor` used identically across tasks. The `bodyActive` prop name, the `Tab.body` field name, and the `RequestBody` discriminator values all match between tasks.
