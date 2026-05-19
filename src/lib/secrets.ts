import { invoke } from "@tauri-apps/api/core";

export function secretSet(envId: number, key: string, value: string): Promise<void> {
  return invoke<void>("secret_set", { envId, key, value });
}

export function secretGet(envId: number, key: string): Promise<string | null> {
  return invoke<string | null>("secret_get", { envId, key });
}

export function secretDelete(envId: number, key: string): Promise<void> {
  return invoke<void>("secret_delete", { envId, key });
}
