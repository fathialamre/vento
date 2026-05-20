import { createTheme } from "@uiw/codemirror-themes";
import { tags as t } from "@lezer/highlight";
import type { Extension } from "@codemirror/state";

export type CmThemeParams = {
  isDark: boolean;
  fontSize: number;
  lineHeight: number;
};

export function ventoCmTheme({
  isDark,
  fontSize,
  lineHeight: _lineHeight,
}: CmThemeParams): Extension {
  const palette = isDark
    ? {
        keyword: "#c084fc",
        string: "#34d399",
        number: "#f59e0b",
        propName: "#f87171",
        bool: "#60a5fa",
        comment: "#71717a",
        bracket: "#a1a1aa",
        invalid: "#f43f5e",
      }
    : {
        keyword: "#7c3aed",
        string: "#059669",
        number: "#b45309",
        propName: "#b91c1c",
        bool: "#2563eb",
        comment: "#71717a",
        bracket: "#52525b",
        invalid: "#e11d48",
      };

  return createTheme({
    theme: isDark ? "dark" : "light",
    settings: {
      background: "var(--card)",
      backgroundImage: "",
      foreground: "var(--card-foreground)",
      caret: "var(--foreground)",
      selection: "var(--accent)",
      selectionMatch: "var(--accent)",
      lineHighlight: "transparent",
      gutterBackground: "var(--card)",
      gutterForeground: "var(--muted-foreground)",
      gutterBorder: "transparent",
      gutterActiveForeground: "var(--foreground)",
      fontFamily: "var(--font-mono, ui-monospace, monospace)",
      fontSize: `${fontSize}px`,
    },
    styles: [
      { tag: [t.comment, t.lineComment, t.blockComment], color: palette.comment, fontStyle: "italic" },
      { tag: [t.string, t.special(t.string)], color: palette.string },
      { tag: [t.number, t.integer, t.float], color: palette.number },
      { tag: [t.bool, t.null, t.keyword], color: palette.bool },
      { tag: [t.propertyName, t.definition(t.propertyName)], color: palette.propName },
      { tag: [t.bracket, t.brace, t.squareBracket, t.paren, t.punctuation], color: palette.bracket },
      { tag: t.invalid, color: palette.invalid },
      { tag: [t.tagName, t.angleBracket], color: palette.propName },
      { tag: t.attributeName, color: palette.keyword },
      { tag: t.attributeValue, color: palette.string },
      { tag: [t.variableName, t.function(t.variableName)], color: palette.keyword },
      { tag: t.operator, color: palette.bracket },
    ],
  });
}

// extra theme tweaks layered on top: line-height, padding, focus outline.
import { EditorView } from "@codemirror/view";

export function ventoCmExtras(lineHeight: number): Extension {
  return EditorView.theme({
    "&": { height: "100%" },
    "&.cm-focused": { outline: "none" },
    ".cm-scroller": { lineHeight: String(lineHeight) },
    ".cm-content": { padding: "8px 0" },
    ".cm-activeLine": { backgroundColor: "transparent" },
    ".cm-activeLineGutter": { backgroundColor: "transparent" },
  });
}
