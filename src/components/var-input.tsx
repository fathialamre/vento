import * as React from "react";
import { AlertCircle } from "lucide-react";

import { cn } from "@/lib/utils";
import { Input } from "@/components/ui/input";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import type { VarMap } from "@/lib/interpolation";

const HIGHLIGHT_RE = /\{\{\s*([A-Za-z0-9_.-]+)\s*\}\}/g;
const PARTIAL_LEFT_RE = /\{\{\s*([A-Za-z0-9_.-]*)$/;
const PARTIAL_RIGHT_RE = /^([A-Za-z0-9_.-]*)(\}\})?/;

type Props = Omit<
  React.InputHTMLAttributes<HTMLInputElement>,
  "onChange" | "value"
> & {
  value: string;
  onChange: (v: string) => void;
  envMap: VarMap;
  globalsMap: VarMap;
  containerClassName?: string;
  onVarClick?: (name: string, source: "env" | "globals") => void;
};

export function VarInput({
  value,
  onChange,
  envMap,
  globalsMap,
  className,
  containerClassName,
  onKeyDown,
  onBlur,
  onVarClick,
  ...rest
}: Props) {
  const inputRef = React.useRef<HTMLInputElement>(null);
  const overlayRef = React.useRef<HTMLDivElement>(null);

  const [acOpen, setAcOpen] = React.useState(false);
  const [acIndex, setAcIndex] = React.useState(0);
  const [partial, setPartial] = React.useState("");
  const [cmdHeld, setCmdHeld] = React.useState(false);

  React.useEffect(() => {
    function onDown(e: KeyboardEvent) { if (e.metaKey || e.ctrlKey) setCmdHeld(true); }
    function onUp(e: KeyboardEvent) { if (!e.metaKey && !e.ctrlKey) setCmdHeld(false); }
    document.addEventListener("keydown", onDown);
    document.addEventListener("keyup", onUp);
    return () => {
      document.removeEventListener("keydown", onDown);
      document.removeEventListener("keyup", onUp);
    };
  }, []);

  const allKeys = React.useMemo(() => {
    const s = new Set<string>([
      ...Object.keys(envMap),
      ...Object.keys(globalsMap),
    ]);
    return Array.from(s).sort();
  }, [envMap, globalsMap]);

  const suggestions = React.useMemo(() => {
    if (!acOpen) return [];
    const q = partial.toLowerCase();
    return allKeys.filter((k) => k.toLowerCase().includes(q)).slice(0, 8);
  }, [acOpen, partial, allKeys]);

  function syncScroll() {
    if (inputRef.current && overlayRef.current) {
      overlayRef.current.scrollLeft = inputRef.current.scrollLeft;
    }
  }

  function detectAutocomplete(el: HTMLInputElement) {
    const pos = el.selectionStart ?? 0;
    const left = el.value.slice(0, pos);
    const m = left.match(PARTIAL_LEFT_RE);
    if (m) {
      setPartial(m[1]);
      setAcIndex(0);
      setAcOpen(true);
    } else {
      setAcOpen(false);
    }
  }

  function handleChange(e: React.ChangeEvent<HTMLInputElement>) {
    onChange(e.currentTarget.value);
    detectAutocomplete(e.currentTarget);
    requestAnimationFrame(syncScroll);
  }

  function insertSuggestion(name: string) {
    const el = inputRef.current;
    if (!el) return;
    const pos = el.selectionStart ?? el.value.length;
    const left = el.value.slice(0, pos);
    const right = el.value.slice(pos);
    const startM = left.match(PARTIAL_LEFT_RE);
    if (!startM) return;
    const startIdx = left.length - startM[0].length;
    const beforeBrace = left.slice(0, startIdx);
    const rightM = right.match(PARTIAL_RIGHT_RE);
    const eatLen = rightM ? rightM[0].length : 0;
    const tail = right.slice(eatLen);
    const insert = `{{${name}}}`;
    const newValue = beforeBrace + insert + tail;
    onChange(newValue);
    setAcOpen(false);
    const newPos = beforeBrace.length + insert.length;
    requestAnimationFrame(() => {
      el.focus();
      el.setSelectionRange(newPos, newPos);
      syncScroll();
    });
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
    if (acOpen && suggestions.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setAcIndex((i) => (i + 1) % suggestions.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setAcIndex((i) => (i - 1 + suggestions.length) % suggestions.length);
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        insertSuggestion(suggestions[acIndex]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setAcOpen(false);
        return;
      }
    }
    onKeyDown?.(e);
  }

  const overlayContent = React.useMemo(() => {
    if (!value) return null;
    const out: React.ReactNode[] = [];
    let lastIdx = 0;
    let m: RegExpExecArray | null;
    let i = 0;
    const re = new RegExp(HIGHLIGHT_RE.source, "g");
    while ((m = re.exec(value)) !== null) {
      if (m.index > lastIdx) {
        out.push(<span key={i++}>{value.slice(lastIdx, m.index)}</span>);
      }
      const name = m[1];
      const fromEnv = Object.prototype.hasOwnProperty.call(envMap, name);
      const fromGlobals = Object.prototype.hasOwnProperty.call(globalsMap, name);
      const exists = fromEnv || fromGlobals;
      if (exists) {
        const resolvedValue = envMap[name] ?? globalsMap[name] ?? "";
        const source = fromEnv ? "env" : "globals";
        out.push(
          <Tooltip key={i++}>
            <TooltipTrigger asChild>
              <span
                className={cn(
                  "pointer-events-auto font-bold text-primary",
                  cmdHeld &&
                    "cursor-pointer underline decoration-primary/60 decoration-dotted underline-offset-4",
                )}
                onMouseDown={(e) => {
                  e.preventDefault();
                  inputRef.current?.focus();
                }}
                onClick={(e) => {
                  if ((e.metaKey || e.ctrlKey) && onVarClick) {
                    e.preventDefault();
                    onVarClick(name, source);
                  }
                }}
              >
                {m[0]}
              </span>
            </TooltipTrigger>
            <TooltipContent side="bottom" className="max-w-xs">
              <div className="flex flex-col gap-1.5">
                <div className="flex items-center gap-1.5">
                  <span className="break-all font-mono text-xs">
                    {resolvedValue || (
                      <span className="opacity-50">empty</span>
                    )}
                  </span>
                  <span
                    className={cn(
                      "shrink-0 rounded px-1 text-[9px] font-semibold uppercase",
                      fromEnv
                        ? "bg-primary/15 text-primary"
                        : "bg-muted text-muted-foreground",
                    )}
                  >
                    {fromEnv ? "env" : "glob"}
                  </span>
                </div>
                {onVarClick && (
                  <span className="text-[10px] opacity-50">⌘/Ctrl click to open</span>
                )}
              </div>
            </TooltipContent>
          </Tooltip>,
        );
      } else {
        out.push(
          <Tooltip key={i++}>
            <TooltipTrigger asChild>
              <span
                className="pointer-events-auto font-bold text-rose-500 underline decoration-rose-500/60 decoration-dotted underline-offset-4"
                onMouseDown={(e) => {
                  e.preventDefault();
                  inputRef.current?.focus();
                }}
              >
                {m[0]}
              </span>
            </TooltipTrigger>
            <TooltipContent side="bottom" className="max-w-xs">
              <div className="flex items-start gap-2">
                <AlertCircle className="mt-0.5 size-3.5 shrink-0 text-rose-500" />
                <div className="flex flex-col gap-0.5">
                  <span className="font-semibold">
                    <span className="font-mono">{`{{${name}}}`}</span> not defined
                  </span>
                  <span className="opacity-80">
                    Add it to the active environment or Globals.
                  </span>
                </div>
              </div>
            </TooltipContent>
          </Tooltip>,
        );
      }
      lastIdx = m.index + m[0].length;
    }
    if (lastIdx < value.length) {
      out.push(<span key={i++}>{value.slice(lastIdx)}</span>);
    }
    return out;
  }, [value, envMap, globalsMap, cmdHeld, onVarClick]);

  return (
    <TooltipProvider delayDuration={150}>
    <div className={cn("relative flex-1", containerClassName)}>
      <div
        ref={overlayRef}
        aria-hidden
        className={cn(
          "pointer-events-none absolute inset-0 flex items-center overflow-hidden whitespace-pre px-2.5 py-1 text-base md:text-sm text-foreground",
        )}
      >
        {overlayContent}
      </div>
      <Input
        ref={inputRef}
        value={value}
        onChange={handleChange}
        onScroll={syncScroll}
        onKeyDown={handleKeyDown}
        onSelect={(e) => detectAutocomplete(e.currentTarget)}
        onClick={(e) => detectAutocomplete(e.currentTarget)}
        onBlur={(e) => {
          setTimeout(() => setAcOpen(false), 150);
          onBlur?.(e);
        }}
        className={cn(
          "bg-transparent text-transparent caret-foreground selection:bg-foreground/20 selection:text-transparent",
          className,
        )}
        autoComplete="off"
        spellCheck={false}
        {...rest}
      />
      {acOpen && suggestions.length > 0 && (
        <div className="absolute left-0 top-full z-50 mt-1 w-[260px] overflow-hidden rounded-md border bg-popover p-1 shadow-md">
          {suggestions.map((s, idx) => {
            const val = envMap[s] ?? globalsMap[s] ?? "";
            const fromEnv = Object.prototype.hasOwnProperty.call(envMap, s);
            return (
              <button
                key={s}
                type="button"
                onMouseDown={(e) => {
                  e.preventDefault();
                  insertSuggestion(s);
                }}
                onMouseEnter={() => setAcIndex(idx)}
                className={cn(
                  "flex w-full items-center gap-2 rounded px-2 py-1 text-left text-xs",
                  idx === acIndex
                    ? "bg-accent text-accent-foreground"
                    : "hover:bg-accent/50",
                )}
              >
                <span className="font-mono font-medium">{s}</span>
                <span className="ml-auto max-w-[140px] truncate text-muted-foreground">
                  {val}
                </span>
                <span
                  className={cn(
                    "shrink-0 rounded px-1 text-[9px] font-semibold uppercase",
                    fromEnv
                      ? "bg-primary/15 text-primary"
                      : "bg-muted text-muted-foreground",
                  )}
                >
                  {fromEnv ? "env" : "glob"}
                </span>
              </button>
            );
          })}
        </div>
      )}
    </div>
    </TooltipProvider>
  );
}
