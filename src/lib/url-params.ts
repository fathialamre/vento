export type QueryParam = {
  id: string;
  key: string;
  value: string;
  enabled: boolean;
  encode: boolean;
  description?: string;
};

export type PersistedQueryParam = Omit<QueryParam, "id">;

let rowCounter = 0;
export function nextRowId(): string {
  rowCounter += 1;
  return `p-${rowCounter}-${Date.now().toString(36)}`;
}

export function newParamRow(overrides?: Partial<QueryParam>): QueryParam {
  return {
    id: nextRowId(),
    key: "",
    value: "",
    enabled: true,
    encode: true,
    description: "",
    ...overrides,
  };
}

function safeDecode(s: string): string {
  try {
    return decodeURIComponent(s.replace(/\+/g, " "));
  } catch {
    return s;
  }
}

export function parseUrl(raw: string): { base: string; params: QueryParam[] } {
  const qIdx = raw.indexOf("?");
  if (qIdx === -1) return { base: raw, params: [] };

  const base = raw.slice(0, qIdx);
  const query = raw.slice(qIdx + 1);
  if (query === "") return { base, params: [] };

  const params: QueryParam[] = [];
  for (const segment of query.split("&")) {
    if (segment === "") continue;
    const eqIdx = segment.indexOf("=");
    const rawKey = eqIdx === -1 ? segment : segment.slice(0, eqIdx);
    const rawVal = eqIdx === -1 ? "" : segment.slice(eqIdx + 1);
    params.push({
      id: nextRowId(),
      key: safeDecode(rawKey),
      value: safeDecode(rawVal),
      enabled: true,
      encode: true,
      description: "",
    });
  }
  return { base, params };
}

export function buildUrl(base: string, params: QueryParam[]): string {
  const active = params.filter((p) => p.enabled && p.key !== "");
  if (active.length === 0) return base;

  const parts: string[] = [];
  for (const p of active) {
    const k = encodeURIComponent(p.key);
    const v = p.encode ? encodeURIComponent(p.value) : p.value;
    parts.push(`${k}=${v}`);
  }
  return `${base}?${parts.join("&")}`;
}

export function getBase(url: string): string {
  const qIdx = url.indexOf("?");
  return qIdx === -1 ? url : url.slice(0, qIdx);
}

export function toPersisted(params: QueryParam[]): PersistedQueryParam[] {
  return params.map(({ id: _id, ...rest }) => rest);
}

export function fromPersisted(json: string | null | undefined): QueryParam[] {
  if (!json) return [];
  try {
    const arr = JSON.parse(json) as PersistedQueryParam[];
    if (!Array.isArray(arr)) return [];
    return arr.map((p) => ({
      id: nextRowId(),
      key: p.key ?? "",
      value: p.value ?? "",
      enabled: p.enabled ?? true,
      encode: p.encode ?? true,
      description: p.description ?? "",
    }));
  } catch {
    return [];
  }
}
