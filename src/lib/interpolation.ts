export type VarMap = Record<string, string>;

export type ResolveResult = {
  output: string;
  unresolved: string[];
};

const VAR_RE = /\{\{\s*([A-Za-z0-9_.-]+)\s*\}\}/g;

export function resolveVars(
  input: string,
  env: VarMap,
  globals: VarMap,
): ResolveResult {
  if (!input || input.indexOf("{{") === -1) {
    return { output: input ?? "", unresolved: [] };
  }
  const seen = new Set<string>();
  const unresolved: string[] = [];
  const output = input.replace(VAR_RE, (match, name: string) => {
    if (Object.prototype.hasOwnProperty.call(env, name)) return env[name];
    if (Object.prototype.hasOwnProperty.call(globals, name)) return globals[name];
    if (!seen.has(name)) {
      seen.add(name);
      unresolved.push(name);
    }
    return match;
  });
  return { output, unresolved };
}

export function resolveVarsInList(
  inputs: string[],
  env: VarMap,
  globals: VarMap,
): { outputs: string[]; unresolved: string[] } {
  const seen = new Set<string>();
  const unresolved: string[] = [];
  const outputs = inputs.map((s) => {
    const r = resolveVars(s, env, globals);
    for (const u of r.unresolved) {
      if (!seen.has(u)) {
        seen.add(u);
        unresolved.push(u);
      }
    }
    return r.output;
  });
  return { outputs, unresolved };
}
