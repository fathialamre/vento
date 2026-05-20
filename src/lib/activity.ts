// Local audit log. Every create/update/delete on a synced entity writes a
// row here. Lives entirely on device — see Migration 9 in src-tauri/src/lib.rs.
// Cloud sync (M4+) will mirror these events to an Appwrite collection so
// teammates see the activity drawer.
//
// All log writes are best-effort: a failure here must never abort the parent
// operation, so each callsite wraps logActivity in a fire-and-forget pattern.

import type Database from "@tauri-apps/plugin-sql";

export type EntityType =
  | "collection"
  | "folder"
  | "saved_request"
  | "environment"
  | "env_variable"
  | "env_variables_bulk";

export type ActivityAction = "create" | "update" | "delete" | "restore";

const ENTITY_TO_TABLE: Record<EntityType, string | null> = {
  collection: "collections",
  folder: "folders",
  saved_request: "saved_requests",
  environment: "environments",
  env_variable: "env_variables",
  env_variables_bulk: null, // synthetic event: no single row to resolve
};

export async function logActivity(
  db: Database,
  opts: {
    entityType: EntityType;
    entityId?: number;
    action: ActivityAction;
    summary?: string;
    actorUserId?: string | null;
  },
): Promise<void> {
  try {
    let entityUuid: string | null = null;
    const table = ENTITY_TO_TABLE[opts.entityType];
    if (table && opts.entityId != null) {
      const rows = await db.select<{ uuid: string | null }[]>(
        `SELECT uuid FROM ${table} WHERE id = $1`,
        [opts.entityId],
      );
      entityUuid = rows[0]?.uuid ?? null;
    }
    await db.execute(
      `INSERT INTO local_activity
         (entity_type, entity_uuid, entity_id, action, summary, actor_user_id, timestamp)
       VALUES ($1, $2, $3, $4, $5, $6, $7)`,
      [
        opts.entityType,
        entityUuid,
        opts.entityId ?? null,
        opts.action,
        opts.summary ?? null,
        opts.actorUserId ?? null,
        Date.now(),
      ],
    );
  } catch (e) {
    // Audit log failures must not break the user-visible operation.
    console.warn("logActivity failed", e);
  }
}
