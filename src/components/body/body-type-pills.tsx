import { cn } from "@/lib/utils";
import type { RequestBody } from "@/lib/body";

export type BodyTypeId = RequestBody["type"];

const PILLS: { id: BodyTypeId; label: string }[] = [
  { id: "none", label: "None" },
  { id: "json", label: "JSON" },
  { id: "text", label: "Text" },
  { id: "form-urlencoded", label: "form-urlencoded" },
  { id: "multipart", label: "multipart" },
];

export type BodyTypePillsProps = {
  active: BodyTypeId;
  onChange: (id: BodyTypeId) => void;
};

export function BodyTypePills({ active, onChange }: BodyTypePillsProps) {
  return (
    <div className="flex items-center gap-0.5 border-b">
      {PILLS.map((p) => {
        const isActive = active === p.id;
        return (
          <button
            key={p.id}
            type="button"
            onClick={() => onChange(p.id)}
            className={cn(
              "relative flex h-9 items-center border-b-2 border-transparent px-3 text-xs font-medium transition-colors",
              isActive
                ? "border-primary text-primary"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {p.label}
          </button>
        );
      })}
    </div>
  );
}
