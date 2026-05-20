import { useEffect, useMemo, useState } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { json } from "@codemirror/lang-json";
import { linter, lintGutter, type Diagnostic } from "@codemirror/lint";
import { EditorView } from "@codemirror/view";

import { Button } from "@/components/ui/button";
import { useIsDark } from "@/hooks/use-is-dark";

export type JsonBodyEditorProps = {
  text: string;
  onChange: (text: string) => void;
};

type ParseStatus =
  | { kind: "empty" }
  | { kind: "valid" }
  | { kind: "invalid"; message: string };

function parseStatus(text: string): ParseStatus {
  if (text.trim() === "") return { kind: "empty" };
  try {
    JSON.parse(text);
    return { kind: "valid" };
  } catch (e) {
    return { kind: "invalid", message: (e as Error).message };
  }
}

const jsonLinter = linter((view) => {
  const text = view.state.doc.toString();
  if (text.trim() === "") return [];
  try {
    JSON.parse(text);
    return [];
  } catch (e) {
    const msg = (e as Error).message;
    let pos = 0;
    const mPos = msg.match(/position (\d+)/i);
    if (mPos) {
      pos = parseInt(mPos[1], 10);
    } else {
      const mLineCol = msg.match(/line (\d+) column (\d+)/i);
      if (mLineCol) {
        const line = parseInt(mLineCol[1], 10);
        const col = parseInt(mLineCol[2], 10);
        try {
          pos = view.state.doc.line(line).from + (col - 1);
        } catch {
          pos = 0;
        }
      }
    }
    const docLen = view.state.doc.length;
    const diag: Diagnostic = {
      from: Math.min(Math.max(0, pos), docLen),
      to: Math.min(Math.max(0, pos) + 1, docLen),
      severity: "error",
      message: msg,
    };
    return [diag];
  }
});

const editorTheme = EditorView.theme({
  "&": { height: "100%", fontSize: "12px" },
  ".cm-scroller": { fontFamily: "var(--font-mono, ui-monospace, monospace)" },
  ".cm-content": { padding: "8px 0" },
  ".cm-gutters": { backgroundColor: "transparent", border: "none" },
});

export function JsonBodyEditor({ text, onChange }: JsonBodyEditorProps) {
  const isDark = useIsDark();
  const [status, setStatus] = useState<ParseStatus>(() => parseStatus(text));

  useEffect(() => {
    const id = window.setTimeout(() => setStatus(parseStatus(text)), 300);
    return () => window.clearTimeout(id);
  }, [text]);

  const extensions = useMemo(
    () => [json(), jsonLinter, lintGutter(), editorTheme, EditorView.lineWrapping],
    [],
  );

  function handleFormat() {
    try {
      const obj = JSON.parse(text);
      onChange(JSON.stringify(obj, null, 2));
    } catch {
      // status line already shows error
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
      <div className="min-h-0 flex-1 overflow-hidden">
        <CodeMirror
          value={text}
          onChange={onChange}
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
        />
      </div>
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
