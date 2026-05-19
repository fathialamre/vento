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
