import { useRef } from "react";
import { X } from "lucide-react";

import { cn } from "@/lib/utils";
import { nextRowId, type QueryParam } from "@/lib/url-params";
import { Input } from "@/components/ui/input";
import {
  ContextMenu,
  ContextMenuCheckboxItem,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";

export type ParamsEditorProps = {
  params: QueryParam[];
  onChange: (next: QueryParam[]) => void;
};

export function ParamsEditor({ params, onChange }: ParamsEditorProps) {
  // Unique-per-instance id for the trailing blank-placeholder row.
  // When user types into it, the row gets materialized into `params` keeping this id,
  // and a fresh draft id is generated for the next blank — so React reuses the input
  // node the user is typing in and focus is preserved.
  const draftIdRef = useRef<string>(nextRowId());

  function updateRow(id: string, patch: Partial<QueryParam>) {
    onChange(params.map((p) => (p.id === id ? { ...p, ...patch } : p)));
  }

  function deleteRow(id: string) {
    onChange(params.filter((p) => p.id !== id));
  }

  function handleEdit(id: string, patch: Partial<QueryParam>) {
    if (id === draftIdRef.current) {
      const materializedId = draftIdRef.current;
      draftIdRef.current = nextRowId();
      onChange([
        ...params,
        { ...BLANK_ROW, id: materializedId, ...patch },
      ]);
      return;
    }
    onChange(params.map((p) => (p.id === id ? { ...p, ...patch } : p)));
  }

  const blankRow: QueryParam = { ...BLANK_ROW, id: draftIdRef.current };
  const rows: QueryParam[] = [...params, blankRow];

  return (
    <div className="overflow-hidden rounded-md border bg-card">
      <div className="grid grid-cols-[36px_1fr_1fr_1fr_36px] items-center border-b bg-muted/40 text-xs font-medium text-muted-foreground">
        <div className="py-1.5 text-center">On</div>
        <div className="px-2 py-1.5">Key</div>
        <div className="px-2 py-1.5">Value</div>
        <div className="px-2 py-1.5">Description</div>
        <div />
      </div>
      {rows.map((row) => {
        const isBlankPlaceholder = row.id === draftIdRef.current;
        return (
          <ContextMenu key={row.id}>
            <ContextMenuTrigger asChild>
              <div className="group/row grid grid-cols-[36px_1fr_1fr_1fr_36px] items-center border-b border-border/60 last:border-b-0 hover:bg-accent/20">
                <label className="flex h-8 cursor-pointer items-center justify-center">
                  <RowCheckbox
                    checked={row.enabled}
                    onChange={(v) => updateRow(row.id, { enabled: v })}
                    disabled={isBlankPlaceholder}
                  />
                </label>
                <CellInput
                  value={row.key}
                  placeholder="key"
                  onChange={(v) => handleEdit(row.id, { key: v })}
                />
                <CellInput
                  value={row.value}
                  placeholder="value"
                  onChange={(v) => handleEdit(row.id, { value: v })}
                  className={cn(!row.encode && "underline decoration-amber-500 decoration-dotted underline-offset-4")}
                  title={row.encode ? undefined : "URL encoding disabled — value sent raw"}
                />
                <CellInput
                  value={row.description ?? ""}
                  placeholder="description"
                  onChange={(v) => handleEdit(row.id, { description: v })}
                />
                <button
                  type="button"
                  onClick={() => deleteRow(row.id)}
                  disabled={isBlankPlaceholder}
                  className="flex h-8 items-center justify-center text-muted-foreground opacity-0 transition-opacity hover:text-foreground group-hover/row:opacity-100 disabled:!opacity-0"
                  aria-label="Delete row"
                >
                  <X className="size-3.5" />
                </button>
              </div>
            </ContextMenuTrigger>
            <ContextMenuContent>
              <ContextMenuCheckboxItem
                checked={!row.encode}
                onCheckedChange={(checked) =>
                  updateRow(row.id, { encode: !checked })
                }
                disabled={isBlankPlaceholder}
              >
                Disable URL encoding
              </ContextMenuCheckboxItem>
              <ContextMenuCheckboxItem
                checked={row.enabled}
                onCheckedChange={(checked) =>
                  updateRow(row.id, { enabled: !!checked })
                }
                disabled={isBlankPlaceholder}
              >
                Enabled
              </ContextMenuCheckboxItem>
              <ContextMenuSeparator />
              <ContextMenuItem
                onSelect={() => deleteRow(row.id)}
                disabled={isBlankPlaceholder}
              >
                Delete row
              </ContextMenuItem>
            </ContextMenuContent>
          </ContextMenu>
        );
      })}
    </div>
  );
}

function CellInput({
  value,
  onChange,
  placeholder,
  className,
  title,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  className?: string;
  title?: string;
}) {
  return (
    <Input
      value={value}
      onChange={(e) => onChange(e.currentTarget.value)}
      placeholder={placeholder}
      title={title}
      autoComplete="off"
      spellCheck={false}
      className={cn(
        "h-8 rounded-none border-0 bg-transparent font-mono text-xs shadow-none focus-visible:ring-0 focus-visible:bg-background",
        className,
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

const BLANK_ROW: Omit<QueryParam, "id"> = {
  key: "",
  value: "",
  enabled: true,
  encode: true,
  description: "",
};
