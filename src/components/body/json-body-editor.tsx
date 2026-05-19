import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";

export type JsonBodyEditorProps = {
  text: string;
  onChange: (text: string) => void;
};

type ParseStatus =
  | { kind: "empty" }
  | { kind: "valid" }
  | { kind: "invalid"; message: string };

function parse(text: string): ParseStatus {
  if (text.trim() === "") return { kind: "empty" };
  try {
    JSON.parse(text);
    return { kind: "valid" };
  } catch (e) {
    return { kind: "invalid", message: (e as Error).message };
  }
}

export function JsonBodyEditor({ text, onChange }: JsonBodyEditorProps) {
  const [status, setStatus] = useState<ParseStatus>(() => parse(text));

  useEffect(() => {
    const id = window.setTimeout(() => setStatus(parse(text)), 300);
    return () => window.clearTimeout(id);
  }, [text]);

  function handleFormat() {
    try {
      const obj = JSON.parse(text);
      onChange(JSON.stringify(obj, null, 2));
    } catch {
      // ignore — status line already shows the parse error
    }
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-center justify-end border-b px-2 py-1">
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="h-7 px-2 text-xs"
          onClick={handleFormat}
          disabled={status.kind !== "valid"}
        >
          Format
        </Button>
      </div>
      <textarea
        value={text}
        onChange={(e) => onChange(e.currentTarget.value)}
        spellCheck={false}
        autoComplete="off"
        placeholder='{ "key": "value" }'
        className="flex-1 resize-none border-0 bg-transparent p-3 font-mono text-xs leading-5 outline-none focus-visible:ring-0"
      />
      <div className="border-t px-3 py-1 text-[11px]">
        {status.kind === "empty" && (
          <span className="text-muted-foreground">Empty</span>
        )}
        {status.kind === "valid" && (
          <span className="text-emerald-600">Valid JSON</span>
        )}
        {status.kind === "invalid" && (
          <span className="text-rose-500">Invalid: {status.message}</span>
        )}
      </div>
    </div>
  );
}
