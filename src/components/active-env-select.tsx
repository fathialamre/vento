import * as React from "react";
import { Check, ChevronDown, Layers, Settings2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { listEnvironments, type Environment } from "@/lib/db";
import { cn } from "@/lib/utils";

type Props = {
  activeEnvId: number | null;
  onChange: (id: number | null) => void;
  onManage: () => void;
  refreshKey?: number;
};

export function ActiveEnvSelect({
  activeEnvId,
  onChange,
  onManage,
  refreshKey,
}: Props) {
  const [envs, setEnvs] = React.useState<Environment[]>([]);

  React.useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const list = await listEnvironments();
        if (!cancelled) setEnvs(list);
      } catch (e) {
        console.error("active env list failed", e);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [refreshKey]);

  const userEnvs = envs.filter((e) => e.is_globals !== 1);
  const active = userEnvs.find((e) => e.id === activeEnvId);
  const label = active ? active.name : "No environment";

  React.useEffect(() => {
    if (activeEnvId === null) return;
    if (envs.length === 0) return;
    if (!userEnvs.some((e) => e.id === activeEnvId)) {
      onChange(null);
    }
  }, [envs, activeEnvId, onChange, userEnvs]);

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="h-8 max-w-[200px] justify-between gap-2 font-normal"
          title="Active environment"
        >
          <span className="flex items-center gap-1.5 overflow-hidden">
            <Layers
              className={cn(
                "size-3.5 shrink-0",
                active ? "text-foreground" : "text-muted-foreground",
              )}
            />
            <span className="truncate">{label}</span>
          </span>
          <ChevronDown className="size-3.5 shrink-0 opacity-50" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-56">
        <DropdownMenuLabel>Environment</DropdownMenuLabel>
        <DropdownMenuItem onSelect={() => onChange(null)}>
          <span className="flex flex-1 items-center gap-2">
            <span className="size-4 shrink-0">
              {activeEnvId === null && <Check className="size-4" />}
            </span>
            No environment
          </span>
        </DropdownMenuItem>
        {userEnvs.length > 0 && <DropdownMenuSeparator />}
        {userEnvs.map((env) => (
          <DropdownMenuItem key={env.id} onSelect={() => onChange(env.id)}>
            <span className="flex flex-1 items-center gap-2">
              <span className="size-4 shrink-0">
                {activeEnvId === env.id && <Check className="size-4" />}
              </span>
              <span className="truncate">{env.name}</span>
            </span>
          </DropdownMenuItem>
        ))}
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={onManage}>
          <Settings2 className="mr-2 size-4" />
          Manage environments…
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
