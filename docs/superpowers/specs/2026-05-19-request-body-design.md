# Request Body — Design Spec

Status: draft
Date: 2026-05-19
Owner: fathialamre

## Summary

Add HTTP request body support to Vento. Today the `Body` request-tab is
disabled, the Rust `send_request` command ignores body data, and the database
has no body columns. This spec adds four body types — Raw JSON, Raw text (with
selectable Content-Type), `application/x-www-form-urlencoded`, and
`multipart/form-data` — across the React UI, Rust backend, and SQLite schema.

Out of scope: GraphQL body type, binary body type, request headers editor
(separately gated), authentication tab, history snapshots of request bodies.

## Goals

- User can configure a body of any of the four supported types on any request,
  regardless of HTTP method.
- Body is preserved across tab switches and persists with saved requests.
- Multipart bodies can include file uploads (chosen via OS file picker, stored
  by absolute path).
- `Content-Type` is set automatically from the chosen body type.
- Environment variable interpolation (`{{var}}`) works in every body type.
- The `Body` tab visibly indicates when a body is configured.

## Non-Goals

- No headers-tab integration. The headers tab remains disabled. When it is
  enabled later, the user-supplied `Content-Type` will need to win over the
  auto-set value, but that wiring belongs to the headers feature, not this
  spec.
- No body-snapshot column in `history`. Replaying a historical request will
  not replay its body. We can revisit if user demand appears.
- No JSON-schema validation, no schema-driven autocomplete, no diff view.
- No drag-and-drop file attach (file picker only) in v1.

## Decisions

| Topic | Decision | Reason |
|---|---|---|
| Body types in v1 | JSON, raw text, form-urlencoded, multipart | User requested all four |
| Editor for raw text/JSON | Plain `<textarea>` (mono) | No new heavy editor dep; matches Vento's lean-deps style |
| Content-Type | Auto-set from body type | Headers tab still disabled; revisit when wired |
| Methods that can carry a body | All methods | Postman-style flexibility, no method gating |
| Persistence | Tab memory + `saved_requests` DB | Restored on tab reopen |
| Multipart file persistence | Absolute path stored, file re-read at send time | Keeps DB small; missing-file errors surface at send |
| History | No body column | Avoids DB bloat from large/multipart bodies |
| Var interpolation in body | Yes, all body types and all string fields | Consistent with URL/params |
| Body-tab indicator | Dot when `body.type !== "none"` | Cheap; doesn't conflict with params count badge |
| JSON validation | Inline parse hint + `Format` button, never blocks send | Lets user send malformed bodies on purpose |
| Body-type switch | Wipe previous body state | Simpler; explicit user action |
| Tests | Manual test plan, no test framework added | Matches current project posture |

## Architecture

```
RequestTabs ──► BottomTab "body" enabled (was disabled)
        │
        ▼
BodyEditor (dispatcher, src/components/body/body-editor.tsx)
   │  props: { body: RequestBody, onChange }
   │
   ├─ <BodyTypePills>          (None | JSON | Text | form-urlencoded | multipart)
   └─ switch (body.type):
        ├─ "none"            → <Empty> placeholder
        ├─ "json"            → <JsonBodyEditor>     { text }
        ├─ "text"            → <RawTextBodyEditor>  { text, contentType }
        ├─ "form-urlencoded" → <FormBodyEditor>     { fields[] }
        └─ "multipart"       → <MultipartBodyEditor>{ fields[] }

App.tsx tab state ── `body: RequestBody` field added
   │
   ▼
sendRequest(...) resolves {{vars}} in body, then:
   invoke("send_request", { method, url, params, body })
   │
   ▼
Rust send_request matches body variant, attaches via reqwest.
```

### Component responsibilities

Per CLAUDE.md, each component has a clear single purpose:

- **`BodyEditor`** — dispatch only. Renders `<BodyTypePills>` and the child
  for the current variant. No body-format logic.
- **`BodyTypePills`** — renders five pills, mirrors `request-tabs.tsx` look
  (`border-b` + `border-primary` underline for active). Emits selection.
- **`JsonBodyEditor`** — full-height mono `<textarea>`. `Format` button at
  top-right. Status line below editor with debounced (300 ms) parse result.
- **`RawTextBodyEditor`** — `Content-Type` `<Select>` at top
  (`text/plain` default; options: `text/plain`, `text/xml`,
  `application/xml`, `text/html`, `application/javascript`). Mono textarea
  fills below.
- **`FormBodyEditor`** — spreadsheet grid matching `params-editor.tsx`.
  Columns: `[On] [Key] [Value] [×]`. Trailing draft row via the `draftIdRef`
  pattern from CLAUDE.md.
- **`MultipartBodyEditor`** — same grid but five columns:
  `[On] [Key] [Kind ▼] [Value | File picker] [×]`. When `kind === "file"`,
  the value cell shows a `Choose file...` button and the picked absolute
  path. File picker uses `@tauri-apps/plugin-dialog` (new dep).

## Data Model

### Frontend (new file `src/lib/body.ts`)

```ts
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

export function bodyHasContent(b: RequestBody): boolean {
  // used for the Body-tab dot indicator
  return b.type !== "none";
}

export function resolveBody(b: RequestBody, lookup: VarLookup): RequestBody {
  // walks the variant and replaces {{var}} in every string field except
  // multipart file paths. Disabled rows are kept in tree (resolve, don't drop).
}
```

### Tab state (`App.tsx`)

`Tab` gains:

```ts
body: RequestBody;   // default EMPTY_BODY in newTab()
```

### Backend (`src-tauri/src/lib.rs`)

```rust
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

#[derive(Deserialize)]
struct BodyField {
    key: String,
    value: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum FieldKind { Text, File }
impl Default for FieldKind { fn default() -> Self { FieldKind::Text } }

#[derive(Deserialize)]
struct MultipartField {
    key: String,
    value: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    kind: FieldKind,
}
```

`send_request` signature grows one optional argument:

```rust
async fn send_request(
    method: String,
    url: String,
    params: Option<Vec<QueryParam>>,
    body: Option<RequestBody>,
) -> Result<HttpResponse, String>
```

### Database migration v5

```sql
ALTER TABLE saved_requests ADD COLUMN body_type TEXT;
ALTER TABLE saved_requests ADD COLUMN body TEXT;
```

- `body_type`: lowercase variant tag (`"none" | "json" | "text" |
  "form-urlencoded" | "multipart"`). Indexable for future filtering.
- `body`: JSON-serialized `RequestBody` payload.
- Pre-migration rows: both columns `NULL`. Loader falls back to `EMPTY_BODY`.

History table unchanged.

## Data Flow — Send Path

```
User clicks Send in App.tsx
   │
   ├─ resolveVars on url, params (existing)
   ├─ resolveBody(tab.body, envLookup):
   │     - json.text, text.text, text.contentType
   │     - form/multipart key + value (text values only; file paths untouched)
   │
   ▼
invoke("send_request", { method, url, params, body: resolvedBody })
   │
   ▼
Rust matches body:
   None              → no body, no Content-Type
   Json{text}        → .header(CT, "application/json").body(text)
   Text{t, ct}       → .header(CT, ct).body(t)
   FormUrlencoded{f} → .form(&kv_map)               // reqwest sets CT
   Multipart{f}      → reqwest::multipart::Form
                         for each enabled field with non-empty key:
                           Text → form.text(k, v)
                           File → tokio::fs::read(path) →
                                  form.part(k, Part::bytes(bytes)
                                              .file_name(basename(path)))
                       .multipart(form)              // reqwest sets CT+boundary
```

Disabled rows and empty-key rows are skipped on the Rust side.

Body is attached unconditionally based on its variant — including on
`GET`/`HEAD`/`DELETE`/`OPTIONS` requests when one is configured. We do not
gate the attach on method; `reqwest` handles whatever the server permits.

## Persistence

### Save (`src/lib/db.ts` `updateRequest`)

- `body_type ← body.type`
- `body ← JSON.stringify(body)`
- Multipart `value` cells already hold path strings; no special handling.

### Load (open saved request → new tab)

- Read `body_type` and `body` columns.
- `JSON.parse(body)` if present and parses, else `EMPTY_BODY`. A malformed
  body string is treated as no body (logged to console, not surfaced to the
  user) so a corrupt row never blocks opening a saved request.
- If a multipart file is missing at send time, Rust read fails and the error
  surfaces in the existing red banner. We do not pre-check file existence
  (would be racy anyway).

### Tab memory

Body always lives in tab state. Switching tabs preserves body. Switching the
body type wipes prior body data (no stash, no undo prompt — user choice).

## Error Handling

| Failure | Surface |
|---|---|
| JSON parse error | Inline status under JSON editor; does NOT block send |
| Empty key in form / multipart row | Row skipped silently (matches params behavior) |
| Multipart row with `kind=file` but no path | Row skipped silently |
| Switching body type with content | Wiped (no prompt) |
| File missing at send time | `Result::Err("body file not found: <path>")` → red banner |
| File read error | `Result::Err("failed to read body file <path>: <io err>")` → red banner |
| Network/HTTP errors | Unchanged, existing red banner |

`tokio::fs::read` is used (we are already on the tokio runtime via reqwest).
No pre-flight validation gate; everything fails at send time so that
malformed JSON or missing files do not silently block the user.

## Testing

No JavaScript/TypeScript test framework is in the repo today. We add no
framework as part of this work. Instead, the implementation plan will
include a manual test plan covering:

1. JSON body POST to `https://httpbin.org/post` → response `json` field
   matches the request payload.
2. Raw text with `application/xml` → request reaches server with that
   Content-Type.
3. form-urlencoded with two fields → server sees both in `form`.
4. multipart with one text field and one file → server sees both; file
   bytes match local file.
5. `{{var}}` interpolation in JSON text, raw text, form value, and multipart
   text value resolves correctly when an active environment is selected.
6. Save request, close tab, reopen via collections panel → body restored
   intact for each of the four types.
7. Multipart file deleted between save and send → error appears in red
   banner; no crash.
8. Switch a request between `GET` and `POST` → body field is unchanged.
9. JSON parse hint flips between `valid` and `invalid line N` as the user
   types; `Format` button reformats with 2-space indent.

Rust side: smoke-test by running the app and exercising the same cases. No
unit tests added.

## Open Questions

None at time of writing. Headers-tab/body Content-Type coordination is
deferred to the headers feature.

## Risks

- **Multipart file path drift.** Saving a request that references a file
  whose path later changes is a footgun. We accept it; the error message is
  explicit. Future enhancement could be relative-path resolution against a
  workspace root, but no workspace concept exists today.
- **Large bodies in `saved_requests.body` column.** A pasted 5 MB JSON body
  saved into the DB will grow the file. Probably acceptable, but if it
  surfaces as a problem we can move to a sidecar table.
- **Var interpolation in JSON body.** Values containing unescaped quotes
  will break JSON validity. We honor the user's choice (option "all body
  types"), and the inline parse hint will tell them when this happens.
