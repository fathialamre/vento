import { useRef } from "react";
import { X } from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

import { cn } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  nextBodyRowId,
  newBodyFieldRow,
  type BodyFieldRow,
} from "@/lib/body";

export type MultipartBodyEditorProps = {
  fields: BodyFieldRow[];
  onChange: (next: BodyFieldRow[]) => void;
};

export function MultipartBodyEditor({
  fields,
  onChange,
}: MultipartBodyEditorProps) {
  const draftIdRef = useRef<string>(nextBodyRowId());

  function updateRow(id: string, patch: Partial<BodyFieldRow>) {
    onChange(fields.map((f) => (f.id === id ? { ...f, ...patch } : f)));
  }

  function deleteRow(id: string) {
    onChange(fields.filter((f) => f.id !== id));
  }

  function handleEdit(id: string, patch: Partial<BodyFieldRow>) {
    if (id === draftIdRef.current) {
      const materializedId = draftIdRef.current;
      draftIdRef.current = nextBodyRowId();
      onChange([
        ...fields,
        { ...newBodyFieldRow({ id: materializedId }), ...patch },
      ]);
      return;
    }
    onChange(fields.map((f) => (f.id === id ? { ...f, ...patch } : f)));
  }

  async function pickFile(id: string) {
    const selected = await openDialog({ multiple: false, directory: false });
    if (typeof selected !== "string") return;
    handleEdit(id, { value: selected, kind: "file" });
  }

  const blank = newBodyFieldRow({ id: draftIdRef.current });
  const rows: BodyFieldRow[] = [...fields, blank];

  return (
    <div className="overflow-hidden rounded-md border bg-card">
      <div className="sticky top-0 z-10 grid grid-cols-[36px_1fr_110px_1fr_36px] items-center border-b bg-muted/60 text-xs font-medium text-muted-foreground backdrop-blur">
        <div className="border-r py-1.5 text-center">On</div>
        <div className="border-r px-2 py-1.5">Key</div>
        <div className="border-r px-2 py-1.5">Type</div>
        <div className="border-r px-2 py-1.5">Value</div>
        <div />
      </div>
      {rows.map((row) => {
        const isBlank = row.id === draftIdRef.current;
        const kind = row.kind ?? "text";
        return (
          <div
            key={row.id}
            className="group/row grid grid-cols-[36px_1fr_110px_1fr_36px] items-center border-b border-border/60 last:border-b-0 hover:bg-accent/20"
          >
            <label className="flex h-8 cursor-pointer items-center justify-center border-r border-border/60">
              <RowCheckbox
                checked={row.enabled}
                onChange={(v) => updateRow(row.id, { enabled: v })}
                disabled={isBlank}
              />
            </label>
            <div className="border-r border-border/60">
              <CellInput
                value={row.key}
                placeholder="key"
                onChange={(v) => handleEdit(row.id, { key: v })}
              />
            </div>
            <div className="border-r border-border/60 px-1">
              <Select
                value={kind}
                onValueChange={(v) =>
                  handleEdit(row.id, {
                    kind: v as "text" | "file",
                    value: "",
                  })
                }
                disabled={isBlank}
              >
                <SelectTrigger className="h-8 rounded-none border-0 bg-transparent text-xs shadow-none focus:ring-0">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="text" className="text-xs">
                    Text
                  </SelectItem>
                  <SelectItem value="file" className="text-xs">
                    File
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="border-r border-border/60">
              {kind === "text" ? (
                <CellInput
                  value={row.value}
                  placeholder="value"
                  onChange={(v) => handleEdit(row.id, { value: v })}
                />
              ) : (
                <button
                  type="button"
                  onClick={() => pickFile(row.id)}
                  disabled={isBlank}
                  className="flex h-8 w-full items-center gap-2 px-2 text-left font-mono text-xs hover:bg-accent/40 disabled:cursor-not-allowed disabled:opacity-50"
                >
                  <span className="text-muted-foreground">Choose file...</span>
                  {row.value && (
                    <span className="truncate text-foreground" title={row.value}>
                      {row.value}
                    </span>
                  )}
                </button>
              )}
            </div>
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
  );
}

function CellInput({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <Input
      value={value}
      onChange={(e) => onChange(e.currentTarget.value)}
      placeholder={placeholder}
      autoComplete="off"
      spellCheck={false}
      className="h-8 rounded-none border-0 bg-transparent font-mono text-xs shadow-none focus-visible:ring-0 focus-visible:bg-background"
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
