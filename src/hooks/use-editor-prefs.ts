import { useEffect, useState } from "react";

export type EditorPrefs = {
  fontSize: number;   // px
  lineHeight: number; // unitless multiplier
};

const STORAGE_KEY = "vento:editor-prefs";
const EVENT_NAME = "vento:editor-prefs-change";

export const DEFAULT_EDITOR_PREFS: EditorPrefs = {
  fontSize: 12,
  lineHeight: 1.5,
};

export const FONT_SIZE_RANGE = { min: 10, max: 22, step: 1 };
export const LINE_HEIGHT_RANGE = { min: 1.0, max: 2.2, step: 0.1 };

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

function load(): EditorPrefs {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_EDITOR_PREFS;
    const parsed = JSON.parse(raw) as Partial<EditorPrefs>;
    return {
      fontSize: clamp(
        typeof parsed.fontSize === "number"
          ? parsed.fontSize
          : DEFAULT_EDITOR_PREFS.fontSize,
        FONT_SIZE_RANGE.min,
        FONT_SIZE_RANGE.max,
      ),
      lineHeight: clamp(
        typeof parsed.lineHeight === "number"
          ? parsed.lineHeight
          : DEFAULT_EDITOR_PREFS.lineHeight,
        LINE_HEIGHT_RANGE.min,
        LINE_HEIGHT_RANGE.max,
      ),
    };
  } catch {
    return DEFAULT_EDITOR_PREFS;
  }
}

function save(prefs: EditorPrefs) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(prefs));
  window.dispatchEvent(new CustomEvent(EVENT_NAME));
}

export function useEditorPrefs(): {
  prefs: EditorPrefs;
  setFontSize: (px: number) => void;
  setLineHeight: (lh: number) => void;
  reset: () => void;
} {
  const [prefs, setPrefs] = useState<EditorPrefs>(load);

  useEffect(() => {
    const handler = () => setPrefs(load());
    window.addEventListener(EVENT_NAME, handler);
    return () => window.removeEventListener(EVENT_NAME, handler);
  }, []);

  function setFontSize(px: number) {
    const next: EditorPrefs = {
      ...prefs,
      fontSize: clamp(px, FONT_SIZE_RANGE.min, FONT_SIZE_RANGE.max),
    };
    save(next);
    setPrefs(next);
  }

  function setLineHeight(lh: number) {
    const next: EditorPrefs = {
      ...prefs,
      lineHeight: clamp(
        Math.round(lh * 10) / 10,
        LINE_HEIGHT_RANGE.min,
        LINE_HEIGHT_RANGE.max,
      ),
    };
    save(next);
    setPrefs(next);
  }

  function reset() {
    save(DEFAULT_EDITOR_PREFS);
    setPrefs(DEFAULT_EDITOR_PREFS);
  }

  return { prefs, setFontSize, setLineHeight, reset };
}
