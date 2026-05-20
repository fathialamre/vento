# Team Sync — Design Spec

Status: draft
Date: 2026-05-20
Owner: fathialamre

## Summary

Add multi-user, multi-device collaboration to Vento on top of a **self-hosted
Appwrite** backend. Workspaces map 1:1 to Appwrite Teams. Local SQLite becomes
a cache; Appwrite is source of truth when signed in. Realtime updates flow via
Appwrite's WebSocket channels. Conflict resolution is **Postman-style**: optimistic
local apply, version-CAS push, modal-driven manual resolution. Secrets remain
per-device in the OS keychain.

Anonymous (no-account) usage stays fully functional locally. No SaaS tier. Free
and open source.

## Goals

- Two or more users can edit the same workspace from different devices and see
  each other's changes within ~2 seconds.
- Edits made offline reconcile cleanly on reconnect, with conflicts surfaced
  via a Postman-like resolution UI.
- Secrets never leave a device; each member sets their own copy per environment
  variable marked `secret`.
- The project remains usable with no Appwrite endpoint configured — "local
  mode" is first-class.
- All sync logic is implementable on web/mobile clients with no Tauri-specific
  code paths.

## Non-Goals

- No hosted "Vento Cloud". Users run their own Appwrite instance.
- No billing, entitlements, free/paid tiers.
- No CRDTs. Conflict policy is operational-transform-free, version-CAS + manual.
- No history sync. The `history` table stays local-only forever.
- No GraphQL request bodies, no mocking server, no test runner suites in this
  spec (orthogonal features).
- No end-to-end encryption of synced documents in v1. Documents are protected
  by Appwrite permissions only.

## Decisions

| Topic | Decision | Reason |
|---|---|---|
| Backend | Self-host Appwrite | Open source, all-in-one (auth + DB + realtime + storage), self-host fits free positioning |
| Workspace ↔ Team | 1:1 | Roles + members native; switcher reads Team list |
| Secrets | Per-device OS keychain only | Privacy; trivial to implement; opt-in shared secrets deferred |
| Conflict resolution | Postman-style: version CAS + manual modal | Familiar UX; no CRDT complexity in v1 |
| Conflict granularity | Doc-level v1 (M5), field-level v1.1 (M5.5) | Ships value early; field-level is pure renderer change later |
| Activity log | Stored in Appwrite collection, 90d default retention, configurable | Auditability without bloat |
| Presence | Realtime broadcast only, no persistence | Lightweight; vanishes on disconnect |
| Auth | Appwrite Auth (OAuth GitHub/Google + email/password + anonymous) | Built-in, no extra dep |
| OAuth callback | `vento://callback` deep link | Native desktop UX |
| Anonymous trial | First-class "local mode" (no session at all) | Lowest friction onboarding |
| Bulk import on first sign-in | Prompt user: Import / Keep separate / Discard | Avoids surprise overwrite |
| Pricing model | Free, self-host only, open source (MIT) | No SaaS, no billing infra |
| Writes path | All via Appwrite Functions for atomic CAS + activity | Single invariant; no SDK-direct writes to synced collections |
| Cross-platform reuse | Sync engine in pure TS; Tauri at edges via ports/adapters | Future web + mobile clients |
| ID scheme | UUIDv7 client-generated | Sortable, no central allocator |
| Soft delete | `deleted_at` tombstone, 30d purge | Restorable, cheap |
| File uploads (multipart) | Appwrite Storage bucket, doc holds `file_id` | Avoid 1MB doc limit |
| Schema versioning | `_meta.schema_version` doc; client refuses start if mismatch | Forward-safe |
| Position / ordering | Integer `position` with renumber on conflict | Defer LexoRank |
| History table | Stays local SQLite, never synced | Privacy + size |

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  UI layer (React, framework-agnostic logic)                 │
├─────────────────────────────────────────────────────────────┤
│  sync engine (src/sync, pure TS)                            │
│  - SyncStore       state machine, queues, conflict tracker  │
│  - AppwriteAdapter HTTP + Realtime SDK calls only           │
│  - models          UUID, version, sync_state                │
├─────────────────────────────────────────────────────────────┤
│  ports (interfaces, src/ports)                              │
│  LocalStore │ SecretStore │ FileStore │ DeepLink │ Clock    │
├─────────────────────────────────────────────────────────────┤
│  adapters (src/adapters/{tauri,web,mobile})                 │
│  Tauri impls today; Web/Mobile later                        │
└─────────────────────────────────────────────────────────────┘
                            │
                            ▼
              ┌─────────────────────────────┐
              │  Self-hosted Appwrite       │
              │  - Auth (OAuth, anon)       │
              │  - DB (collections below)   │
              │  - Realtime (channels)      │
              │  - Storage (request-files)  │
              │  - Functions (CAS, purge)   │
              └─────────────────────────────┘
```

Import-boundary rule: `src/sync/**` and `src/lib/**` must not import from
`@tauri-apps/*`. Tauri SDK access is confined to `src/adapters/tauri/`.

## Identity model

- **User**: Appwrite Account. Has email, name, prefs. Identified by `$id`.
- **Workspace**: Appwrite Team. `team_id` is the workspace id used in app.
- **Membership**: Appwrite Team Membership. Roles: `owner`, `editor`, `viewer`.
  Owners can rename/delete workspace and change member roles. Editors can
  read/write all docs. Viewers can read only.
- **Local mode**: no Appwrite session at all. `workspace_id` on local rows is
  `NULL`. App functions exactly as today.

A user may belong to N workspaces. Sidebar top shows a workspace switcher
listing memberships plus a permanent "Local" pseudo-entry.

## Data model

### Base attributes (every synced entity)

| attribute | type | notes |
|---|---|---|
| `$id` | string(36) | UUIDv7, client-generated, also stored in `id` column locally |
| `workspace_id` | string(36) | Team id |
| `created_at` | int (ms epoch) | Set by Function on create |
| `updated_at` | int (ms epoch) | Bumped every write |
| `created_by` | string | User `$id` |
| `updated_by` | string | User `$id` |
| `version` | int | Starts at 1, +1 per write (CAS basis) |
| `deleted_at` | int? | Tombstone |

### Per-entity schemas

#### `workspaces`

App-level metadata for a Team. One doc per Team.

- `team_id` (string, unique), `name`, `icon_url`, `description`,
  `default_environment_id`, `activity_retention_days` (int, default 90),
  `settings_json` (string, opaque)
- Permissions: read `team:WS`, write `team:WS/owner`.

#### `collections`

- base + `name`, `description`, `position`
- Indexes: `workspace_id`, `(workspace_id, deleted_at)`
- Permissions: read `team:WS`, write `team:WS/editor`.

#### `folders`

- base + `collection_id`, `parent_folder_id` (nullable), `name`, `position`
- Indexes: `workspace_id`, `collection_id`, `parent_folder_id`

#### `saved_requests`

- base + `collection_id`, `folder_id` (nullable), `name`, `method`, `url`,
  `params_json`, `body_type`, `body_json`, `headers_json`, `position`
- Body JSON > 900KB → spill to Storage bucket and store `body_blob_id` instead.
- Indexes: `workspace_id`, `collection_id`, `folder_id`

#### `environments`

- base + `name`, `is_globals` (bool), `position`
- One Globals env per workspace, enforced by Function check + partial unique
  index `(workspace_id, is_globals)` where `is_globals = true`.

#### `env_variables`

- base + `environment_id`, `key`, `value` (empty string if secret), `secret`
  (bool), `enabled` (bool), `position`
- Plaintext for secrets stays in OS keychain only.
- Indexes: `environment_id`, `workspace_id`

#### `activity`

- `workspace_id`, `entity_type` (enum), `entity_id`, `action` (`create | update
  | delete | restore`), `summary` (≤200 chars), `actor_user_id`, `timestamp`,
  optional `diff_json` (field-level deltas for field-level conflict UX)
- Write-only via Function. Read by team members.
- Indexes: `(workspace_id, timestamp DESC)`, `(entity_type, entity_id,
  timestamp DESC)`

#### `_meta`

Singleton-collection style.

- `schema_version` doc: `{ value: int }`. Client refuses to start if local
  client schema < this value.
- Future: app feature flags, rollout switches.

### Storage bucket — `request-files`

- Multipart file uploads + spilled body blobs.
- Permissions: read `team:WS`, write `team:WS/editor`.
- Local cache by file id at `~/Library/Application Support/com.alamre.vento/files/`
  (per-platform path via Tauri path API).

### Local SQLite — added columns

For each synced table (`collections`, `folders`, `saved_requests`,
`environments`, `env_variables`):

```sql
ALTER TABLE T ADD COLUMN uuid TEXT;
ALTER TABLE T ADD COLUMN workspace_id TEXT;
ALTER TABLE T ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE T ADD COLUMN updated_by TEXT;
ALTER TABLE T ADD COLUMN created_by TEXT;
ALTER TABLE T ADD COLUMN version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE T ADD COLUMN deleted_at INTEGER;
ALTER TABLE T ADD COLUMN sync_state TEXT NOT NULL DEFAULT 'local';
ALTER TABLE T ADD COLUMN base_version INTEGER NOT NULL DEFAULT 0;
```

`sync_state` values:
`local | pending_create | clean | dirty | pending_update | conflicted |
pending_delete`.

Indexes:

```sql
CREATE UNIQUE INDEX idx_T_uuid     ON T(uuid);
CREATE INDEX idx_T_workspace      ON T(workspace_id, deleted_at);
CREATE INDEX idx_T_sync           ON T(sync_state);
```

UUID backfill happens at app boot: any row with `uuid IS NULL` gets a fresh
UUIDv7.

#### History table

No changes. Stays local. Never synced.

#### Keychain key format migration

- Old: `{env_id}::{key}` where `env_id` is local int.
- New (local mode): `local::{env_uuid}::{key}`.
- New (synced): `{workspace_id}::{env_uuid}::{key}`.
- One-shot rotation at first migration boot. Falls back gracefully if old key
  unreadable.

## Sync engine

### State machine (per row)

```
[ created locally ] ──► pending_create
                            │
                push success ▼   conflict on uniqueness
                          clean ─────────────► conflicted
                            │
              user edit ────▼
                          dirty ──debounce──► pending_update
                                                  │
                                push ok ──────────▼
                                                clean
                                push stale ─────► conflicted
                                                  │
                                user resolves ────▼
                                                dirty (retry)

[ realtime event for newer version ]
   if local clean   → upsert, stays clean
   if local dirty   → conflict candidate, mark conflicted
   if local conflicted → keep, ignore (already pending resolution)

[ delete ] ──► pending_delete ──push ok──► row purged locally
                              ──push stale (someone restored)──► conflicted
```

`base_version` is the last server `version` the client observed. Push includes
`base_version`; server CAS checks it.

### Push path

1. UI mutation → write to local SQLite, set `sync_state = dirty`.
2. Debounce 500ms.
3. Mark `pending_update`, call Function `entity_update(collection, id, patch,
   base_version)`.
4. Function read current doc:
   - If `current.version == base_version` → write patch, `version =
     base_version + 1`, append `activity`, return new doc.
   - Else → return `{ conflict: true, current }`.
5. Client on success → store new `version` and `base_version`, set `clean`.
   On conflict → set `conflicted`, surface modal.

### Pull path

- On workspace switch / cold start: paginated fetch all docs since
  `last_pulled_at`. Upsert into local cache.
- Realtime subscribe to channels: `databases.vento.collections.<coll>.documents`
  filtered by `workspace_id` via client-side filter.
- Each event: upsert if newer `version`, ignore if older, conflict-mark if
  local dirty.

### Offline queue

- `dirty`/`pending_*` rows accumulate.
- On reconnect: flush in `updated_at` order.
- Conflicts pile into the "Conflict queue" UI.

### Cross-platform reuse

Sync engine has no Tauri imports. Local persistence accessed via:

```ts
interface LocalStore {
  get(table: string, id: string): Promise<Row | null>;
  put(table: string, row: Row): Promise<void>;
  upsert(table: string, rows: Row[]): Promise<void>;
  list(table: string, where: WhereClause): Promise<Row[]>;
  transaction(fn: (tx: LocalStore) => Promise<void>): Promise<void>;
}

interface SecretStore {
  set(scope: string, key: string, value: string): Promise<void>;
  get(scope: string, key: string): Promise<string | null>;
  delete(scope: string, key: string): Promise<void>;
}

interface FileStore {
  pickOpen(): Promise<string | null>;
  read(path: string): Promise<Uint8Array>;
  cacheRemote(fileId: string, bytes: Uint8Array): Promise<string>; // returns local path
}

interface DeepLink {
  onUrl(handler: (url: URL) => void): () => void;
  schemeOk(): boolean;
}
```

Tauri implementations live in `src/adapters/tauri/`. Web stubs land in
`src/adapters/web/` later.

## Auth + deep link

### Setup wizard (first launch in sync mode)

1. Settings → "Connect to workspace".
2. Inputs: Appwrite endpoint URL, project id. "Test connection" pings the
   endpoint.
3. Choose sign-in: GitHub, Google, Email/Password.
4. Browser opens via `shell.open(url)` to Appwrite's OAuth URL. Success URL =
   `vento://callback?status=success`, failure URL =
   `vento://callback?status=failure&reason=...`.
5. Appwrite OAuth completes → 302 to `vento://callback?...`. OS hands the URL
   to Vento via deep-link handler.
6. App reads session cookie via Appwrite SDK; updates UI; loads workspace list.

### Anonymous → account upgrade

App always starts in local mode unless `vento:has-account = true` flag is
set. Signing in upgrades from local to synced. The bulk-import prompt fires
once on first sign-in to decide what happens to existing local data.

### Platform setup

- macOS: Tauri config registers `vento` scheme in Info.plist via
  `bundle.macOS.urlSchemes` (Tauri v2 syntax).
- Windows: MSI installer writes HKCU\Software\Classes\vento registry keys.
- Linux: Bundler emits `.desktop` file with `x-scheme-handler/vento` MIME type.

## Conflict UX

### Editor footer states

| State | Footer text |
|---|---|
| Idle, in sync | `saved · v12` |
| Local edit, debouncing | `unsaved changes…` |
| Push in flight | `saving…` |
| Push succeeded | `saved · v13` |
| Stale push → conflicted | `⚠ conflict — resolve` (clickable) |
| Offline | `offline — 3 changes queued` |

### Presence avatars

- Top-right of editor, max 4 visible, "+N" overflow on hover.
- Color hash from user `$id`. Pulsing dot if actively editing (heartbeat
  payload includes `editing: true`).
- Tooltip: name + role + activity ("viewing", "editing").

### Doc-level conflict modal (M5)

Two-column layout: **Yours** | **Theirs**. Single radio chooses whole-doc
winner. `Cancel` keeps row `conflicted` and leaves banner.

### Field-level conflict modal (M5.5)

Three columns: **Yours** | **Theirs** | **Merged Result**. Radio per field.
Identical fields collapse. `Save Merged` writes merged payload with
`base_version = current.version`. Bulk shortcuts: "Take all theirs", "Take
all mine".

### Conflict queue UI

When offline reconnect produces N conflicts, banner at app top: `⚠ N
conflicts to resolve`. Click opens a list dialog showing each pending
conflict with one-click open into the modal.

### Delete vs edit variant

Special modal: "Bob deleted X. You have unsaved changes. Restore or
discard?" Restore = clear tombstone + write new version.

### Activity drawer

Right-side panel per entity, toggleable. Lists recent `activity` rows for
that `entity_id` newest first. Each row shows actor avatar, summary,
relative time, version number. Click for diff peek (field-level only, from
`activity.diff_json`).

## Bulk import on first sign-in

Dialog fires once on first sign-in (or each new workspace if user opts in
to keep asking).

```
┌─── Welcome to [Workspace Name] ─────────────────────┐
│ This device has local data:                         │
│   • 12 collections (78 requests)                    │
│   • 3 environments (24 variables)                   │
│                                                     │
│ ● Import into workspace        (default)            │
│ ○ Keep separate (Local + Workspace)                 │
│ ○ Discard local data                                │
│                                                     │
│ [ ] Don't ask for next workspaces                   │
│                                                     │
│            [Cancel]            [Continue]           │
└─────────────────────────────────────────────────────┘
```

Behavior:
- **Import**: client calls Function `workspace_bulk_import(payload)` with
  all local rows. Function writes them with new `workspace_id`. Local rows
  get `workspace_id` filled + `sync_state = clean`.
- **Keep separate**: local rows stay `local`; sidebar switcher lists
  "Local" + each workspace.
- **Discard**: confirm dialog ("type DELETE") then purge local synced-table
  rows.

## Appwrite Functions

All writes go through Functions for CAS + activity logging atomicity. SDK
direct writes are reserved for reads.

| Function | Purpose |
|---|---|
| `entity_create(collection, payload)` | Generate or accept UUID, write doc with `version = 1`, append activity row. |
| `entity_update(collection, id, patch, base_version)` | CAS update. Returns conflict on mismatch. Appends activity row with diff. |
| `entity_delete(collection, id, base_version)` | Soft delete (set `deleted_at`). CAS guarded. Appends activity row. |
| `entity_restore(collection, id, base_version)` | Clear `deleted_at`. Appends activity. |
| `workspace_bulk_import(payload)` | Batched create for first-sign-in import. |
| `purge_activity` (cron daily) | Delete `activity` rows older than each workspace's `activity_retention_days`. |
| `purge_tombstones` (cron daily) | Hard-delete rows with `deleted_at < now - 30d`. Includes orphaned keychain key cleanup hooks. |
| `warm_keepalive` (cron 60s) | Pings each write Function to keep instance warm; reduces cold-start latency on user writes. |

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Data loss during UUID migration | Backup local SQLite before migration; ship a "revert" command |
| Realtime channel limits on heavy workspaces | Self-host removes hard limit; client falls back to polling at 5s if WS drops |
| Function cold start latency for every write | Use Appwrite Functions on "always warm" if available; otherwise direct CAS-emulated SDK write in v1 trial path |
| Concurrent multi-tab edits in same client | Single shared `SyncStore` singleton; tabs share row state |
| Self-host complexity scares users away | Invest M8 docs heavily; ship docker-compose one-liner |
| Conflict modal too aggressive | Banner-not-modal default; modal only on explicit save click; bulk resolve buttons |
| Keychain key collisions on workspace rename | Workspaces never renamed by id; key uses `workspace_id` (immutable Team id), not name |
| Forward-incompatible schema bumps | `_meta.schema_version` gate; old clients refuse start with clear upgrade message |
| Body blobs not garbage collected | Reference-counting in Storage via Function; weekly orphan sweep |

## Resolved

1. **CAS via Functions** — every write goes through an Appwrite Function that
   reads current, compares `base_version`, writes patch, bumps version, appends
   activity. Cold-start mitigated with a 60s warm-up cron Function. No
   SDK-direct writes to synced collections from clients.
2. **Schema deploy** — single TS SDK script (`scripts/deploy-appwrite-schema.ts`),
   idempotent, run by self-hoster as part of setup.
3. **Bulk import idempotency** — dedup by local UUID. Re-running import skips
   docs whose UUID already exists in the workspace.
4. **Per-collection import granularity** — ships in v1.0 as part of M2's
   bulk-import dialog (advanced expander with collection checkboxes).
5. **License** — MIT.

## Open questions (post-launch)

- **Presence cost**: how many concurrent viewers does a typical workspace
  reach? If realistically < 5, broadcast-only works; if 50+, persistence
  needed. Profile during beta.
