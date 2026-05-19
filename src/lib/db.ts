import Database from "@tauri-apps/plugin-sql";
import { secretSet, secretGet, secretDelete } from "@/lib/secrets";

const DB_URL = "sqlite:vento.db";

export type HistoryItem = {
  id: number;
  method: string;
  url: string;
  status: number | null;
  duration_ms: number | null;
  response_preview: string | null;
  sent_at: number;
  params: string | null;
};

let dbPromise: Promise<Database> | null = null;

export function loadDb(): Promise<Database> {
  if (!dbPromise) {
    dbPromise = Database.load(DB_URL);
  }
  return dbPromise;
}

export async function insertHistory(input: {
  method: string;
  url: string;
  status: number | null;
  duration_ms: number | null;
  response_preview: string | null;
  params: string | null;
}): Promise<void> {
  const db = await loadDb();
  await db.execute(
    "INSERT INTO history (method, url, status, duration_ms, response_preview, sent_at, params) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    [
      input.method,
      input.url,
      input.status,
      input.duration_ms,
      input.response_preview,
      Date.now(),
      input.params,
    ],
  );
}

export async function listHistory(limit = 50): Promise<HistoryItem[]> {
  const db = await loadDb();
  return db.select<HistoryItem[]>(
    "SELECT id, method, url, status, duration_ms, response_preview, sent_at, params FROM history ORDER BY sent_at DESC LIMIT $1",
    [limit],
  );
}

export async function clearHistory(): Promise<void> {
  const db = await loadDb();
  await db.execute("DELETE FROM history");
}

export type Collection = {
  id: number;
  name: string;
  created_at: number;
};

export type Folder = {
  id: number;
  collection_id: number;
  parent_folder_id: number | null;
  name: string;
  created_at: number;
};

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

export type TreeFolder = Folder & {
  folders: TreeFolder[];
  requests: SavedRequest[];
};

export type TreeCollection = Collection & {
  folders: TreeFolder[];
  requests: SavedRequest[];
};

export async function listCollections(): Promise<Collection[]> {
  const db = await loadDb();
  return db.select<Collection[]>(
    "SELECT id, name, created_at FROM collections ORDER BY created_at ASC",
  );
}

export async function createCollection(name: string): Promise<number> {
  const db = await loadDb();
  const res = await db.execute(
    "INSERT INTO collections (name, created_at) VALUES ($1, $2)",
    [name, Date.now()],
  );
  return res.lastInsertId ?? 0;
}

export async function renameCollection(id: number, name: string): Promise<void> {
  const db = await loadDb();
  await db.execute("UPDATE collections SET name = $1 WHERE id = $2", [name, id]);
}

export async function deleteCollection(id: number): Promise<void> {
  const db = await loadDb();
  await db.execute("DELETE FROM collections WHERE id = $1", [id]);
}

export async function createFolder(input: {
  collection_id: number;
  parent_folder_id: number | null;
  name: string;
}): Promise<number> {
  const db = await loadDb();
  const res = await db.execute(
    "INSERT INTO folders (collection_id, parent_folder_id, name, created_at) VALUES ($1, $2, $3, $4)",
    [input.collection_id, input.parent_folder_id, input.name, Date.now()],
  );
  return res.lastInsertId ?? 0;
}

export async function renameFolder(id: number, name: string): Promise<void> {
  const db = await loadDb();
  await db.execute("UPDATE folders SET name = $1 WHERE id = $2", [name, id]);
}

export async function deleteFolder(id: number): Promise<void> {
  const db = await loadDb();
  await db.execute("DELETE FROM folders WHERE id = $1", [id]);
}

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

export async function renameRequest(id: number, name: string): Promise<void> {
  const db = await loadDb();
  await db.execute("UPDATE saved_requests SET name = $1 WHERE id = $2", [name, id]);
}

export async function deleteRequest(id: number): Promise<void> {
  const db = await loadDb();
  await db.execute("DELETE FROM saved_requests WHERE id = $1", [id]);
}

export async function getCollectionsTree(): Promise<TreeCollection[]> {
  const db = await loadDb();
  const [collections, folders, requests] = await Promise.all([
    db.select<Collection[]>(
      "SELECT id, name, created_at FROM collections ORDER BY created_at ASC",
    ),
    db.select<Folder[]>(
      "SELECT id, collection_id, parent_folder_id, name, created_at FROM folders ORDER BY name ASC",
    ),
    db.select<SavedRequest[]>(
      "SELECT id, collection_id, folder_id, name, method, url, created_at, params, body_type, body FROM saved_requests ORDER BY name ASC",
    ),
  ]);

  const folderById = new Map<number, TreeFolder>();
  folders.forEach((f) =>
    folderById.set(f.id, { ...f, folders: [], requests: [] }),
  );

  const tree: TreeCollection[] = collections.map((c) => ({
    ...c,
    folders: [],
    requests: [],
  }));
  const collectionById = new Map(tree.map((c) => [c.id, c]));

  folderById.forEach((f) => {
    if (f.parent_folder_id == null) {
      collectionById.get(f.collection_id)?.folders.push(f);
    } else {
      folderById.get(f.parent_folder_id)?.folders.push(f);
    }
  });

  requests.forEach((r) => {
    if (r.folder_id == null) {
      collectionById.get(r.collection_id)?.requests.push(r);
    } else {
      folderById.get(r.folder_id)?.requests.push(r);
    }
  });

  return tree;
}

export type Environment = {
  id: number;
  name: string;
  is_globals: number;
  created_at: number;
};

export type EnvVariable = {
  id: number;
  environment_id: number;
  key: string;
  value: string;
  secret: number;
  enabled: number;
  position: number;
};

export type VarMap = Record<string, string>;

export type VariableInput = {
  key: string;
  value: string;
  secret: number;
  enabled: number;
  position: number;
};

export async function listEnvironments(): Promise<Environment[]> {
  const db = await loadDb();
  return db.select<Environment[]>(
    "SELECT id, name, is_globals, created_at FROM environments ORDER BY is_globals DESC, created_at ASC",
  );
}

export async function getGlobalsEnvironment(): Promise<Environment> {
  const db = await loadDb();
  const rows = await db.select<Environment[]>(
    "SELECT id, name, is_globals, created_at FROM environments WHERE is_globals = 1 LIMIT 1",
  );
  if (rows.length === 0) {
    throw new Error("Globals environment missing — DB migration not applied?");
  }
  return rows[0];
}

export async function createEnvironment(name: string): Promise<number> {
  const trimmed = name.trim();
  if (!trimmed) throw new Error("Environment name required");
  const db = await loadDb();
  const res = await db.execute(
    "INSERT INTO environments (name, is_globals, created_at) VALUES ($1, 0, $2)",
    [trimmed, Date.now()],
  );
  return res.lastInsertId ?? 0;
}

export async function renameEnvironment(id: number, name: string): Promise<void> {
  const trimmed = name.trim();
  if (!trimmed) throw new Error("Environment name required");
  const db = await loadDb();
  const rows = await db.select<{ is_globals: number }[]>(
    "SELECT is_globals FROM environments WHERE id = $1",
    [id],
  );
  if (rows[0]?.is_globals === 1) throw new Error("Cannot rename Globals");
  await db.execute("UPDATE environments SET name = $1 WHERE id = $2", [trimmed, id]);
}

export async function deleteEnvironment(id: number): Promise<void> {
  const db = await loadDb();
  const rows = await db.select<{ is_globals: number }[]>(
    "SELECT is_globals FROM environments WHERE id = $1",
    [id],
  );
  if (rows[0]?.is_globals === 1) throw new Error("Cannot delete Globals");
  const secrets = await db.select<{ key: string }[]>(
    "SELECT key FROM env_variables WHERE environment_id = $1 AND secret = 1",
    [id],
  );
  await Promise.all(secrets.map((r) => secretDelete(id, r.key).catch(() => {})));
  await db.execute("DELETE FROM environments WHERE id = $1", [id]);
}

export async function listVariables(environmentId: number): Promise<EnvVariable[]> {
  const db = await loadDb();
  return db.select<EnvVariable[]>(
    "SELECT id, environment_id, key, value, secret, enabled, position FROM env_variables WHERE environment_id = $1 ORDER BY position ASC, id ASC",
    [environmentId],
  );
}

export async function duplicateEnvironment(id: number, newName: string): Promise<number> {
  const db = await loadDb();
  const trimmed = newName.trim();
  if (!trimmed) throw new Error("New environment name required");
  const src = await db.select<Environment[]>(
    "SELECT id, name, is_globals, created_at FROM environments WHERE id = $1",
    [id],
  );
  if (src.length === 0) throw new Error("Source environment not found");
  if (src[0].is_globals === 1) throw new Error("Cannot duplicate Globals");

  const srcVars = await listVariables(id);
  const res = await db.execute(
    "INSERT INTO environments (name, is_globals, created_at) VALUES ($1, 0, $2)",
    [trimmed, Date.now()],
  );
  const newId = res.lastInsertId ?? 0;
  for (const v of srcVars) {
    await db.execute(
      "INSERT INTO env_variables (environment_id, key, value, secret, enabled, position) VALUES ($1, $2, $3, $4, $5, $6)",
      [newId, v.key, v.value, v.secret, v.enabled, v.position],
    );
    if (v.secret === 1) {
      const plaintext = await secretGet(id, v.key).catch(() => null);
      if (plaintext !== null) {
        await secretSet(newId, v.key, plaintext).catch(() => {});
      }
    }
  }
  return newId;
}

export async function replaceVariables(
  environmentId: number,
  vars: VariableInput[],
  plaintextSecrets: Record<string, string>,
): Promise<void> {
  const db = await loadDb();
  const prior = await db.select<{ key: string }[]>(
    "SELECT key FROM env_variables WHERE environment_id = $1 AND secret = 1",
    [environmentId],
  );
  const nextSecretKeys = new Set(
    vars.filter((v) => v.secret === 1 && v.key.trim() !== "").map((v) => v.key),
  );
  const toDeleteFromKeyring = prior
    .map((r) => r.key)
    .filter((k) => !nextSecretKeys.has(k));

  await db.execute("BEGIN");
  try {
    await db.execute(
      "DELETE FROM env_variables WHERE environment_id = $1",
      [environmentId],
    );
    for (const v of vars) {
      if (v.key.trim() === "") continue;
      const storedValue = v.secret === 1 ? "" : v.value;
      await db.execute(
        "INSERT INTO env_variables (environment_id, key, value, secret, enabled, position) VALUES ($1, $2, $3, $4, $5, $6)",
        [environmentId, v.key, storedValue, v.secret, v.enabled, v.position],
      );
    }
    await db.execute("COMMIT");
  } catch (e) {
    await db.execute("ROLLBACK").catch(() => {});
    throw e;
  }

  await Promise.all(
    toDeleteFromKeyring.map((k) => secretDelete(environmentId, k).catch(() => {})),
  );
  await Promise.all(
    vars
      .filter((v) => v.secret === 1 && v.key.trim() !== "")
      .map((v) => {
        const plaintext = plaintextSecrets[v.key];
        if (plaintext === undefined) return Promise.resolve();
        return secretSet(environmentId, v.key, plaintext).catch(() => {});
      }),
  );
}

export async function buildVarMap(
  environmentId: number,
  vars: EnvVariable[],
): Promise<VarMap> {
  const map: VarMap = {};
  for (const v of vars) {
    if (v.enabled !== 1) continue;
    if (v.key === "") continue;
    if (v.secret === 1) {
      const val = await secretGet(environmentId, v.key).catch(() => null);
      if (val !== null) map[v.key] = val;
    } else {
      map[v.key] = v.value;
    }
  }
  return map;
}

export async function countVariablesByEnv(): Promise<Record<number, number>> {
  const db = await loadDb();
  const rows = await db.select<{ environment_id: number; n: number }[]>(
    "SELECT environment_id, COUNT(*) as n FROM env_variables GROUP BY environment_id",
  );
  const out: Record<number, number> = {};
  for (const r of rows) out[r.environment_id] = r.n;
  return out;
}
