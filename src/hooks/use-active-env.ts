import { useEffect, useState } from "react";
import {
  buildVarMap,
  getGlobalsEnvironment,
  listVariables,
  type VarMap,
} from "@/lib/db";

const STORAGE_KEY = "vento:active-env-id";

export function useActiveEnv(refreshKey: number) {
  const [activeEnvId, setActiveEnvId] = useState<number | null>(() => {
    const s = localStorage.getItem(STORAGE_KEY);
    if (!s) return null;
    const n = parseInt(s, 10);
    return Number.isFinite(n) ? n : null;
  });
  const [envMap, setEnvMap] = useState<VarMap>({});
  const [globalsMap, setGlobalsMap] = useState<VarMap>({});
  const [globalsId, setGlobalsId] = useState<number | null>(null);

  useEffect(() => {
    if (activeEnvId === null) localStorage.removeItem(STORAGE_KEY);
    else localStorage.setItem(STORAGE_KEY, String(activeEnvId));
  }, [activeEnvId]);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const globals = await getGlobalsEnvironment();
        const gVars = await listVariables(globals.id);
        const gMap = await buildVarMap(globals.id, gVars);
        if (cancelled) return;
        setGlobalsId(globals.id);
        setGlobalsMap(gMap);

        if (activeEnvId !== null && activeEnvId !== globals.id) {
          const vars = await listVariables(activeEnvId);
          const map = await buildVarMap(activeEnvId, vars);
          if (cancelled) return;
          setEnvMap(map);
        } else {
          setEnvMap({});
        }
      } catch (e) {
        console.error("useActiveEnv load failed", e);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [activeEnvId, refreshKey]);

  return { activeEnvId, setActiveEnvId, envMap, globalsMap, globalsId };
}
