# Vento

Desktop HTTP client (Postman-style) built on Tauri 2 + React 19 + TypeScript +
Vite + Tailwind 4 + shadcn/ui (`radix-nova` style, Lucide icons, neutral base).

Rust backend handles network/secrets/SQLite; React frontend handles UI.

## Architecture

- `src-tauri/src/lib.rs` — Tauri commands and DB migrations.
  - `send_request` — performs HTTP via `reqwest` (rustls, gzip/brotli/deflate,
    multipart). Returns `{ status, duration_ms, body, headers, size_bytes }`.
  - `secret_{set,get,delete}` — OS keychain via `keyring` crate. Service name
    `com.vento.env`, entry key `"{env_id}::{key}"`.
  - All schema lives in the `migrations` `Vec<Migration>`. Add a new
    `Migration { version: N+1, ... MigrationKind::Up }` — never edit existing
    ones. Tables: `history`, `collections`, `folders`, `saved_requests`,
    `environments`, `env_variables`.
- `src/App.tsx` — root component. Multi-tab state (`Tab[]` of kind
  `request | settings | environment`), URL bar, method dropdown, response
  viewer, debounced auto-save (500ms) for tabs whose `requestId` starts with
  `r:` (collection-backed requests).
- `src/lib/db.ts` — typed wrappers over `@tauri-apps/plugin-sql`
  (`sqlite:vento.db`). Single shared `Database` promise via `loadDb()`.
- `src/lib/body.ts` — `RequestBody` union (`none | json | text |
  form-urlencoded | multipart`), `resolveBody()` for var interpolation,
  `toPersistedBody`/`fromPersistedBody` strip/regen React row ids around DB.
  Multipart **file paths are NOT interpolated**.
- `src/lib/url-params.ts` — `parseUrl` / `buildUrl` / `toPersisted` /
  `fromPersisted`. Params are persisted as JSON in `saved_requests.params`.
- `src/lib/interpolation.ts` — `{{var}}` resolver. Lookup order: active env
  map → globals map. Unresolved tokens are returned alongside the output and
  logged but not blocking.
- `src/lib/secrets.ts` — thin `invoke()` wrappers for the secret commands.
- `src/hooks/use-active-env.ts` — loads variables for active env + Globals into
  `VarMap` records. Active env id persists to `localStorage`
  (`vento:active-env-id`). Secret values fetched from keyring per `buildVarMap`.
- `src/components/body/*` — one editor per body type. `body-editor.tsx`
  switches editors via `BodyTypePills`.
- `src/components/{params,environment,...}-editor.tsx` — spreadsheet-grid
  editors (see UI section below).
- `src/components/app-sidebar.tsx`, `collections-panel.tsx`,
  `environments-panel.tsx`, `tree-view.tsx` — left nav with Collections /
  History / Environments / Settings.

## Persistence model

- **SQLite** (`sqlite:vento.db`, via `tauri-plugin-sql`) — collections,
  folders, saved requests (incl. `params`, `body_type`, `body` JSON), history,
  environments, env variables.
- **OS keychain** (via `keyring`) — plaintext of any env variable with
  `secret = 1`. `env_variables.value` stays empty for secrets; lookup happens
  in `buildVarMap`.
- **`localStorage`** — `vento:layout-mode`, `vento:active-env-id`.
- Tab state is **in-memory only** (React state in `App.tsx`); not persisted
  across reloads.

## Conventions

- Path alias: `@/*` → `src/*` (`tsconfig.json` + `vite.config.ts`).
- shadcn primitives live in `src/components/ui/`. Add new ones with `shadcn`
  CLI; do not hand-roll Radix wrappers.
- Variable references in any user-entered string use Mustache-style
  `{{name}}` and resolve via `resolveVars` / `resolveVarsInList` /
  `resolveBody`. URLs, param values, body content all support it.
- HTTP methods are the constant `HTTP_METHODS` tuple in `App.tsx` with
  per-method colors in `METHOD_COLOR` — match these when adding method UI.

## Dev / build

- `npm run tauri dev` — full app (boots Vite on 1420, then Tauri shell).
- `npm run dev` — Vite only (no Rust). Useful for pure UI work.
- `npm run build` — `tsc && vite build` (frontend only).
- `npm run tauri build` — release bundle.

## Git commits

- Always commit and push as user `fathialamre` only.
- Never add `Co-Authored-By: Claude` (or any Claude attribution) to commit messages.
- Never set Claude as author or co-author.
- Do not include "Generated with Claude Code" footers in commit messages or PR bodies.
- Commit messages must read as if written by the user.

## UI: tabular editors (params, env vars, headers, etc.)

All key/value editor grids in Vento share the same spreadsheet-style look. When
building or modifying one, match this pattern:

- Outer wrapper: `overflow-hidden rounded-md border bg-card`.
- CSS grid (`grid-cols-[...]`) with the same column template on header and rows.
- Header row:
  - `sticky top-0 z-10`, `bg-muted/60`, `backdrop-blur`, `border-b`,
    `text-xs font-medium text-muted-foreground`.
  - Each header cell except the last gets `border-r`.
- Body row:
  - `border-b border-border/60 last:border-b-0`, hover with `hover:bg-accent/20`,
    `group/row` so the trailing delete icon can opacity-fade in on hover.
  - Each body cell except the last gets `border-r border-border/60`.
  - Inputs inside cells use `h-8 rounded-none border-0 bg-transparent font-mono
    text-xs shadow-none focus-visible:bg-background focus-visible:ring-0` so the
    grid lines stay visible.
- Trailing blank "draft" row: keep React identity stable by storing the draft id
  in `useRef` and rotating it only when the row is materialized — see
  `src/components/params-editor.tsx` (`draftIdRef`) and
  `src/components/environment-editor.tsx`. Do NOT key the blank row by a static
  string like `"blank"`; it loses focus on first keystroke.
