import * as React from "react";
import {
  Copy as CopyIcon,
  Globe,
  Layers,
  Pencil,
  Plus,
  Trash2,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  countVariablesByEnv,
  createEnvironment,
  deleteEnvironment,
  duplicateEnvironment,
  listEnvironments,
  renameEnvironment,
  type Environment,
} from "@/lib/db";

type DialogState =
  | { type: "new" }
  | { type: "rename"; env: Environment }
  | { type: "duplicate"; env: Environment }
  | { type: "delete"; env: Environment }
  | null;

type Props = {
  onOpenEnvironment: (envId: number, name: string, isGlobals: boolean) => void;
  activeEnvId: number | null;
  query?: string;
  refreshKey?: number;
};

export function EnvironmentsPanel({
  onOpenEnvironment,
  activeEnvId,
  query = "",
  refreshKey,
}: Props) {
  const [envs, setEnvs] = React.useState<Environment[]>([]);
  const [counts, setCounts] = React.useState<Record<number, number>>({});
  const [selectedId, setSelectedId] = React.useState<number | null>(null);
  const [dialog, setDialog] = React.useState<DialogState>(null);

  const refresh = React.useCallback(async () => {
    try {
      const [list, count] = await Promise.all([
        listEnvironments(),
        countVariablesByEnv(),
      ]);
      setEnvs(list);
      setCounts(count);
    } catch (e) {
      console.error("environments load failed", e);
    }
  }, []);

  React.useEffect(() => {
    refresh();
  }, [refresh, refreshKey]);

  const filtered = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return envs;
    return envs.filter((e) => e.name.toLowerCase().includes(q));
  }, [envs, query]);

  const selected = envs.find((e) => e.id === selectedId) ?? null;
  const selectedIsGlobals = selected?.is_globals === 1;
  const canModify = selected !== null && !selectedIsGlobals;

  return (
    <div className="flex h-full flex-col">
      <div className="flex flex-wrap items-center gap-1 border-b p-2">
        <Button
          size="sm"
          variant="ghost"
          onClick={() => setDialog({ type: "new" })}
          title="New environment"
        >
          <Plus className="size-4" />
          Environment
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={() => selected && setDialog({ type: "duplicate", env: selected })}
          disabled={!canModify}
          title="Duplicate"
        >
          <CopyIcon className="size-4" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={() => selected && setDialog({ type: "rename", env: selected })}
          disabled={!canModify}
          title="Rename"
        >
          <Pencil className="size-4" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={() => selected && setDialog({ type: "delete", env: selected })}
          disabled={!canModify}
          title="Delete"
          className="ml-auto text-destructive hover:text-destructive"
        >
          <Trash2 className="size-4" />
        </Button>
      </div>

      <div className="flex-1 overflow-auto">
        {filtered.length === 0 ? (
          <div className="p-4 text-sm text-muted-foreground">
            No environments. Click + Environment to create one.
          </div>
        ) : (
          filtered.map((env) => {
            const isActive = activeEnvId === env.id;
            const isSelected = selectedId === env.id;
            const isGlobals = env.is_globals === 1;
            const count = counts[env.id] ?? 0;
            return (
              <div
                key={env.id}
                onClick={() => setSelectedId(env.id)}
                onDoubleClick={() => onOpenEnvironment(env.id, env.name, isGlobals)}
                className={cn(
                  "flex w-full cursor-pointer items-center gap-2 border-b border-border/60 px-3 py-2 text-sm last:border-b-0",
                  isSelected
                    ? "bg-sidebar-accent text-sidebar-accent-foreground"
                    : "hover:bg-sidebar-accent/40",
                )}
              >
                {isGlobals ? (
                  <Globe className="size-4 shrink-0 text-muted-foreground" />
                ) : (
                  <Layers className="size-4 shrink-0 text-muted-foreground" />
                )}
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    setSelectedId(env.id);
                    onOpenEnvironment(env.id, env.name, isGlobals);
                  }}
                  className="flex-1 text-left"
                >
                  {env.name}
                </button>
                <span className="text-[11px] text-muted-foreground">{count}</span>
                {isActive && !isGlobals && (
                  <span className="rounded bg-foreground/10 px-1.5 py-0.5 text-[10px] font-medium text-foreground">
                    Active
                  </span>
                )}
              </div>
            );
          })
        )}
      </div>

      <NameDialog
        open={dialog?.type === "new"}
        title="New Environment"
        defaultValue=""
        confirmLabel="Create"
        onOpenChange={(open) => !open && setDialog(null)}
        onConfirm={async (name) => {
          await createEnvironment(name);
          setDialog(null);
          await refresh();
        }}
      />
      <NameDialog
        open={dialog?.type === "rename"}
        title="Rename Environment"
        defaultValue={dialog?.type === "rename" ? dialog.env.name : ""}
        confirmLabel="Rename"
        onOpenChange={(open) => !open && setDialog(null)}
        onConfirm={async (name) => {
          if (dialog?.type !== "rename") return;
          await renameEnvironment(dialog.env.id, name);
          setDialog(null);
          await refresh();
        }}
      />
      <NameDialog
        open={dialog?.type === "duplicate"}
        title="Duplicate Environment"
        defaultValue={
          dialog?.type === "duplicate" ? `${dialog.env.name} Copy` : ""
        }
        confirmLabel="Duplicate"
        onOpenChange={(open) => !open && setDialog(null)}
        onConfirm={async (name) => {
          if (dialog?.type !== "duplicate") return;
          await duplicateEnvironment(dialog.env.id, name);
          setDialog(null);
          await refresh();
        }}
      />
      <DeleteEnvDialog
        open={dialog?.type === "delete"}
        envName={dialog?.type === "delete" ? dialog.env.name : ""}
        onOpenChange={(open) => !open && setDialog(null)}
        onConfirm={async () => {
          if (dialog?.type !== "delete") return;
          await deleteEnvironment(dialog.env.id);
          if (selectedId === dialog.env.id) setSelectedId(null);
          setDialog(null);
          await refresh();
        }}
      />
    </div>
  );
}

function NameDialog({
  open,
  title,
  defaultValue,
  confirmLabel,
  onOpenChange,
  onConfirm,
}: {
  open: boolean;
  title: string;
  defaultValue: string;
  confirmLabel: string;
  onOpenChange: (open: boolean) => void;
  onConfirm: (name: string) => Promise<void>;
}) {
  const [name, setName] = React.useState("");
  const [loading, setLoading] = React.useState(false);

  React.useEffect(() => {
    if (open) setName(defaultValue);
  }, [open, defaultValue]);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const trimmed = name.trim();
    if (!trimmed) return;
    setLoading(true);
    try {
      await onConfirm(trimmed);
    } finally {
      setLoading(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
        </DialogHeader>
        <form onSubmit={submit} className="flex flex-col gap-4">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="env-name">Name</Label>
            <Input
              id="env-name"
              placeholder="Staging"
              value={name}
              onChange={(e) => setName(e.currentTarget.value)}
              autoFocus
            />
          </div>
          <DialogFooter>
            <Button type="submit" disabled={!name.trim() || loading}>
              {confirmLabel}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function DeleteEnvDialog({
  open,
  envName,
  onOpenChange,
  onConfirm,
}: {
  open: boolean;
  envName: string;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => Promise<void>;
}) {
  const [loading, setLoading] = React.useState(false);

  async function handleConfirm() {
    setLoading(true);
    try {
      await onConfirm();
    } finally {
      setLoading(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Delete "{envName}"?</DialogTitle>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">
          This permanently deletes the environment, all its variables, and any
          stored secrets in the OS keyring.
        </p>
        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={loading}
          >
            Cancel
          </Button>
          <Button
            variant="destructive"
            onClick={handleConfirm}
            disabled={loading}
          >
            Delete
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
