import { cn } from "@/lib/utils";

export type BottomTab = "params" | "headers" | "body" | "auth";

export type RequestTabsProps = {
  active: BottomTab;
  onChange: (tab: BottomTab) => void;
  paramsCount: number;
};

const TABS: { id: BottomTab; label: string; enabled: boolean }[] = [
  { id: "params", label: "Params", enabled: true },
  { id: "headers", label: "Headers", enabled: false },
  { id: "body", label: "Body", enabled: false },
  { id: "auth", label: "Auth", enabled: false },
];

export function RequestTabs({ active, onChange, paramsCount }: RequestTabsProps) {
  return (
    <div className="flex items-center gap-0.5 border-b">
      {TABS.map((t) => {
        const isActive = active === t.id;
        const count = t.id === "params" ? paramsCount : 0;
        return (
          <button
            key={t.id}
            type="button"
            onClick={() => t.enabled && onChange(t.id)}
            disabled={!t.enabled}
            className={cn(
              "relative flex h-9 items-center gap-1.5 border-b-2 border-transparent px-3 text-xs font-medium transition-colors",
              isActive
                ? "border-foreground text-foreground"
                : "text-muted-foreground hover:text-foreground",
              !t.enabled && "cursor-not-allowed opacity-40 hover:text-muted-foreground",
            )}
          >
            {t.label}
            {count > 0 && (
              <span className="rounded bg-accent px-1 py-0.5 text-[10px] font-bold text-accent-foreground">
                {count}
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}
