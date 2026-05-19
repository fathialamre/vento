import * as React from "react";
import { Eye, EyeOff, Globe, Layers, Loader2, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import {
  listVariables,
  replaceVariables,
  type EnvVariable,
  type VariableInput,
} from "@/lib/db";
import { secretGet } from "@/lib/secrets";

type Row = {
  id: string;
  key: string;
  value: string;
  secret: boolean;
  enabled: boolean;
};

type Props = {
  environmentId: number;
  isGlobals: boolean;
  onSaved: () => void;
};

let rowSeq = 0;
function newRowId(): string {
  rowSeq += 1;
  return `er-${rowSeq}-${Date.now().toString(36)}`;
}

export function EnvironmentEditor({ environmentId, isGlobals, onSaved }: Props) {
  const [rows, setRows] = React.useState<Row[]>([]);
  const [loading, setLoading] = React.useState(true);
  const [saving, setSaving] = React.useState(false);
  const saveTimer = React.useRef<number | null>(null);
  const skipNextSaveRef = React.useRef(true);
  const draftIdRef = React.useRef<string>(newRowId());

  React.useEffect(() => {
    let cancelled = false;
    skipNextSaveRef.current = true;
    setLoading(true);
    (async () => {
      try {
        const vars = await listVariables(environmentId);
        const loaded: Row[] = await Promise.all(
          vars.map(async (v: EnvVariable) => {
            const isSecret = v.secret === 1;
            const value = isSecret
              ? (await secretGet(environmentId, v.key).catch(() => null)) ?? ""
              : v.value;
            return {
              id: newRowId(),
              key: v.key,
              value,
              secret: isSecret,
              enabled: v.enabled === 1,
            };
          }),
        );
        if (cancelled) return;
        setRows(loaded);
      } catch (e) {
        console.error("env editor load failed", e);
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [environmentId]);

  React.useEffect(() => {
    if (loading) return;
    if (skipNextSaveRef.current) {
      skipNextSaveRef.current = false;
      return;
    }
    if (saveTimer.current) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(async () => {
      saveTimer.current = null;
      setSaving(true);
      try {
        const vars: VariableInput[] = rows
          .filter((r) => r.key.trim() !== "")
          .map((r, i) => ({
            key: r.key,
            value: r.secret ? "" : r.value,
            secret: r.secret ? 1 : 0,
            enabled: r.enabled ? 1 : 0,
            position: i,
          }));
        const plaintextSecrets: Record<string, string> = {};
        for (const r of rows) {
          if (r.secret && r.key.trim() !== "") plaintextSecrets[r.key] = r.value;
        }
        await replaceVariables(environmentId, vars, plaintextSecrets);
        onSaved();
      } catch (e) {
        console.error("env save failed", e);
      } finally {
        setSaving(false);
      }
    }, 500);
    return () => {
      if (saveTimer.current) {
        window.clearTimeout(saveTimer.current);
        saveTimer.current = null;
      }
    };
  }, [rows, loading, environmentId, onSaved]);

  function updateRow(id: string, patch: Partial<Row>) {
    setRows((prev) => prev.map((r) => (r.id === id ? { ...r, ...patch } : r)));
  }

  function deleteRow(id: string) {
    setRows((prev) => prev.filter((r) => r.id !== id));
  }

  function handleEdit(id: string, patch: Partial<Row>) {
    if (id === draftIdRef.current) {
      const materializedId = draftIdRef.current;
      draftIdRef.current = newRowId();
      setRows((prev) => [
        ...prev,
        { id: materializedId, key: "", value: "", secret: false, enabled: true, ...patch },
      ]);
      return;
    }
    updateRow(id, patch);
  }

  return (
    <div className="flex h-full flex-1 flex-col overflow-hidden">
      <div className="flex items-center gap-2 border-b px-4 py-3">
        {isGlobals ? (
          <Globe className="size-4 text-muted-foreground" />
        ) : (
          <Layers className="size-4 text-muted-foreground" />
        )}
        <span className="text-sm font-medium">
          {isGlobals ? "Globals" : "Environment"}
        </span>
        <span className="text-xs text-muted-foreground">
          Variables apply via {"{{name}}"} in URLs and query params.
        </span>
        <div className="ml-auto flex items-center gap-2 text-xs text-muted-foreground">
          {saving && <Loader2 className="size-3 animate-spin" />}
          {saving ? "Saving…" : ""}
        </div>
      </div>

      <div className="flex-1 overflow-auto p-4">
        {loading ? (
          <div className="text-sm text-muted-foreground">Loading…</div>
        ) : (
          <div className="overflow-hidden rounded-md border bg-card">
            <div className="sticky top-0 z-10 grid grid-cols-[36px_1fr_2fr_56px_36px] items-center border-b bg-muted/60 text-xs font-medium text-muted-foreground backdrop-blur">
              <div className="border-r py-1.5 text-center">On</div>
              <div className="border-r px-2 py-1.5">Key</div>
              <div className="border-r px-2 py-1.5">Value</div>
              <div className="border-r px-2 py-1.5 text-center">Secret</div>
              <div />
            </div>
            {[
              ...rows,
              {
                id: draftIdRef.current,
                key: "",
                value: "",
                secret: false,
                enabled: true,
              } as Row,
            ].map((row) => {
              const isBlank = row.id === draftIdRef.current;
              return (
                <div
                  key={row.id}
                  className="group/row grid grid-cols-[36px_1fr_2fr_56px_36px] items-center border-b border-border/60 last:border-b-0 hover:bg-accent/20"
                >
                  <label className="flex h-8 cursor-pointer items-center justify-center border-r border-border/60">
                    <RowCheckbox
                      checked={row.enabled}
                      disabled={isBlank}
                      onChange={(v) => updateRow(row.id, { enabled: v })}
                    />
                  </label>
                  <div className="border-r border-border/60">
                    <CellInput
                      value={row.key}
                      placeholder="key"
                      onChange={(v) => handleEdit(row.id, { key: v })}
                    />
                  </div>
                  <div className="border-r border-border/60">
                    <SecretValueInput
                      value={row.value}
                      secret={row.secret}
                      onChange={(v) => handleEdit(row.id, { value: v })}
                    />
                  </div>
                  <button
                    type="button"
                    onClick={() => updateRow(row.id, { secret: !row.secret })}
                    disabled={isBlank}
                    className="flex h-8 items-center justify-center border-r border-border/60 text-muted-foreground hover:text-foreground disabled:opacity-30"
                    title={row.secret ? "Stored as secret in keyring" : "Mark as secret"}
                  >
                    {row.secret ? (
                      <EyeOff className="size-3.5" />
                    ) : (
                      <Eye className="size-3.5 opacity-50" />
                    )}
                  </button>
                  <button
                    type="button"
                    onClick={() => deleteRow(row.id)}
                    disabled={isBlank}
                    className="flex h-8 items-center justify-center text-muted-foreground opacity-0 transition-opacity hover:text-foreground group-hover/row:opacity-100 disabled:!opacity-0"
                    aria-label="Delete row"
                  >
                    <X className="size-3.5" />
                  </button>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

function CellInput({
  value,
  onChange,
  placeholder,
  className,
  type = "text",
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  className?: string;
  type?: string;
}) {
  return (
    <Input
      value={value}
      type={type}
      onChange={(e) => onChange(e.currentTarget.value)}
      placeholder={placeholder}
      autoComplete="off"
      spellCheck={false}
      className={cn(
        "h-8 rounded-none border-0 bg-transparent font-mono text-xs shadow-none focus-visible:bg-background focus-visible:ring-0",
        className,
      )}
    />
  );
}

function SecretValueInput({
  value,
  secret,
  onChange,
}: {
  value: string;
  secret: boolean;
  onChange: (v: string) => void;
}) {
  const [focused, setFocused] = React.useState(false);
  const masked = secret && !focused && value !== "";
  return (
    <Input
      value={masked ? "••••••••" : value}
      onFocus={() => setFocused(true)}
      onBlur={() => setFocused(false)}
      onChange={(e) => onChange(e.currentTarget.value)}
      placeholder="value"
      autoComplete="off"
      spellCheck={false}
      className={cn(
        "h-8 rounded-none border-0 bg-transparent font-mono text-xs shadow-none focus-visible:bg-background focus-visible:ring-0",
        secret && "text-amber-700 dark:text-amber-400",
      )}
    />
  );
}

function RowCheckbox({
  checked,
  onChange,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={checked}
      onClick={() => !disabled && onChange(!checked)}
      disabled={disabled}
      className={cn(
        "flex size-4 items-center justify-center rounded-[4px] border border-muted-foreground/40 transition-colors",
        checked && !disabled
          ? "border-foreground bg-foreground text-background"
          : "bg-transparent",
        disabled && "cursor-not-allowed opacity-30",
        !disabled && "cursor-pointer hover:border-foreground",
      )}
    >
      {checked && (
        <svg viewBox="0 0 12 12" className="size-3" fill="none">
          <path
            d="M2.5 6.5L5 9L9.5 3.5"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      )}
    </button>
  );
}
