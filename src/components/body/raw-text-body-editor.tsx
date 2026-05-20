import { useMemo } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { xml } from "@codemirror/lang-xml";
import { html } from "@codemirror/lang-html";
import { javascript } from "@codemirror/lang-javascript";
import { EditorView } from "@codemirror/view";
import type { Extension } from "@codemirror/state";

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useIsDark } from "@/hooks/use-is-dark";
import { useEditorPrefs } from "@/hooks/use-editor-prefs";

const CONTENT_TYPES = [
  "text/plain",
  "text/xml",
  "application/xml",
  "text/html",
  "application/javascript",
];

function languageExtension(contentType: string): Extension | null {
  switch (contentType) {
    case "text/xml":
    case "application/xml":
      return xml();
    case "text/html":
      return html();
    case "application/javascript":
      return javascript();
    default:
      return null;
  }
}

function buildEditorTheme(fontSize: number, lineHeight: number) {
  return EditorView.theme({
    "&": { height: "100%", fontSize: `${fontSize}px` },
    ".cm-scroller": {
      fontFamily: "var(--font-mono, ui-monospace, monospace)",
      lineHeight: String(lineHeight),
    },
    ".cm-content": { padding: "8px 0" },
    ".cm-gutters": { backgroundColor: "transparent", border: "none" },
  });
}

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
  const isDark = useIsDark();
  const { prefs } = useEditorPrefs();

  const extensions = useMemo<Extension[]>(() => {
    const exts: Extension[] = [
      buildEditorTheme(prefs.fontSize, prefs.lineHeight),
      EditorView.lineWrapping,
    ];
    const lang = languageExtension(contentType);
    if (lang) exts.push(lang);
    return exts;
  }, [contentType, prefs.fontSize, prefs.lineHeight]);

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
      <div className="min-h-0 flex-1 overflow-hidden">
        <CodeMirror
          value={text}
          onChange={(v) => onChange({ text: v, contentType })}
          theme={isDark ? "dark" : "light"}
          extensions={extensions}
          basicSetup={{
            lineNumbers: true,
            foldGutter: true,
            highlightActiveLine: true,
            highlightActiveLineGutter: true,
            autocompletion: false,
          }}
          height="100%"
          className="h-full"
          placeholder="raw body"
        />
      </div>
    </div>
  );
}
