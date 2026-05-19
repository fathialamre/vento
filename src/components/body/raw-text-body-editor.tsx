import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const CONTENT_TYPES = [
  "text/plain",
  "text/xml",
  "application/xml",
  "text/html",
  "application/javascript",
];

export type RawTextBodyEditorProps = {
  text: string;
  contentType: string;
  onChange: (next: { text: string; contentType: string }) => void;
};

export function RawTextBodyEditor({
  text,
  contentType,
  onChange,
}: RawTextBodyEditorProps) {
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-center gap-2 border-b px-2 py-1">
        <label className="text-xs text-muted-foreground">Content-Type:</label>
        <Select
          value={contentType}
          onValueChange={(v) => onChange({ text, contentType: v })}
        >
          <SelectTrigger className="h-7 w-[220px] text-xs">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {CONTENT_TYPES.map((ct) => (
              <SelectItem key={ct} value={ct} className="text-xs">
                {ct}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <textarea
        value={text}
        onChange={(e) =>
          onChange({ text: e.currentTarget.value, contentType })
        }
        spellCheck={false}
        autoComplete="off"
        placeholder="raw body"
        className="flex-1 resize-none border-0 bg-transparent p-3 font-mono text-xs leading-5 outline-none focus-visible:ring-0"
      />
    </div>
  );
}
