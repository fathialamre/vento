import { useState } from "react";
import {
  Code2,
  Columns2,
  Info,
  Minus,
  Monitor,
  Moon,
  Palette,
  Plus,
  RotateCcw,
  Rows2,
  Sun,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useTheme } from "@/components/theme-provider";
import { Button } from "@/components/ui/button";
import {
  FONT_SIZE_RANGE,
  LINE_HEIGHT_RANGE,
  useEditorPrefs,
} from "@/hooks/use-editor-prefs";
import { cn } from "@/lib/utils";

const APP_VERSION = "0.1.0";
const APP_NAME = "Vento";
const APP_DESCRIPTION = "A fast, native API client built with Tauri.";

type SectionId = "appearance" | "editor" | "about";

type Section = {
  id: SectionId;
  label: string;
  icon: LucideIcon;
};

const SECTIONS: Section[] = [
  { id: "appearance", label: "Appearance", icon: Palette },
  { id: "editor", label: "Editor", icon: Code2 },
  { id: "about", label: "About", icon: Info },
];

const THEMES = [
  { value: "light" as const, label: "Light", icon: Sun },
  { value: "dark" as const, label: "Dark", icon: Moon },
  { value: "system" as const, label: "System", icon: Monitor },
];

const LAYOUTS = [
  { value: "vertical" as const, label: "Stacked", icon: Rows2 },
  { value: "horizontal" as const, label: "Side by side", icon: Columns2 },
];

type Props = {
  layoutMode: "vertical" | "horizontal";
  onLayoutChange: (m: "vertical" | "horizontal") => void;
};

export function SettingsView({ layoutMode, onLayoutChange }: Props) {
  const [activeId, setActiveId] = useState<SectionId>("appearance");
  const active = SECTIONS.find((s) => s.id === activeId) ?? SECTIONS[0];

  return (
    <div className="flex h-full overflow-hidden">
      <aside className="w-56 shrink-0 border-r bg-muted/20 p-3">
        <h2 className="px-2 pb-2 font-heading text-base font-medium">
          Settings
        </h2>
        <nav className="flex flex-col gap-1">
          {SECTIONS.map(({ id, label, icon: Icon }) => {
            const isActive = id === activeId;
            return (
              <button
                key={id}
                type="button"
                onClick={() => setActiveId(id)}
                className={cn(
                  "flex items-center gap-2 rounded-md px-2 py-1.5 text-sm transition-colors",
                  isActive
                    ? "bg-accent text-accent-foreground"
                    : "text-muted-foreground hover:bg-muted hover:text-foreground",
                )}
              >
                <Icon className="size-4" />
                <span>{label}</span>
              </button>
            );
          })}
        </nav>
      </aside>

      <div className="flex-1 overflow-auto p-6">
        <h3 className="pb-4 font-heading text-lg font-medium">
          {active.label}
        </h3>
        {activeId === "appearance" && (
          <AppearanceSection layoutMode={layoutMode} onLayoutChange={onLayoutChange} />
        )}
        {activeId === "editor" && <EditorSection />}
        {activeId === "about" && <AboutSection />}
      </div>
    </div>
  );
}

function EditorSection() {
  const { prefs, setFontSize, setLineHeight, reset } = useEditorPrefs();

  const previewStyle = {
    fontSize: `${prefs.fontSize}px`,
    lineHeight: String(prefs.lineHeight),
    fontFamily: "var(--font-mono, ui-monospace, monospace)",
  };

  return (
    <div className="max-w-md space-y-6">
      <section className="space-y-3">
        <div className="flex items-center justify-between">
          <h4 className="text-sm font-medium">Font size</h4>
          <span className="text-xs text-muted-foreground">
            {prefs.fontSize} px
          </span>
        </div>
        <NumericStepper
          value={prefs.fontSize}
          min={FONT_SIZE_RANGE.min}
          max={FONT_SIZE_RANGE.max}
          step={FONT_SIZE_RANGE.step}
          onChange={setFontSize}
          format={(v) => `${v} px`}
        />
      </section>

      <section className="space-y-3">
        <div className="flex items-center justify-between">
          <h4 className="text-sm font-medium">Line height</h4>
          <span className="text-xs text-muted-foreground">
            {prefs.lineHeight.toFixed(1)}
          </span>
        </div>
        <NumericStepper
          value={prefs.lineHeight}
          min={LINE_HEIGHT_RANGE.min}
          max={LINE_HEIGHT_RANGE.max}
          step={LINE_HEIGHT_RANGE.step}
          onChange={setLineHeight}
          format={(v) => v.toFixed(1)}
        />
      </section>

      <section className="space-y-2">
        <h4 className="text-sm font-medium">Preview</h4>
        <div
          className="overflow-hidden rounded-md border bg-card p-3"
          style={previewStyle}
        >
          {"{\n  \"name\": \"vento\",\n  \"version\": \"0.1.0\"\n}"
            .split("\n")
            .map((line, i) => (
              <div key={i}>{line || " "}</div>
            ))}
        </div>
      </section>

      <div>
        <Button
          type="button"
          size="sm"
          variant="ghost"
          className="h-8 px-2 text-xs"
          onClick={reset}
        >
          <RotateCcw className="mr-1 size-3.5" />
          Reset to defaults
        </Button>
      </div>
    </div>
  );
}

function NumericStepper({
  value,
  min,
  max,
  step,
  onChange,
  format,
}: {
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (v: number) => void;
  format: (v: number) => string;
}) {
  return (
    <div className="flex items-center gap-2">
      <Button
        type="button"
        size="icon"
        variant="outline"
        className="size-8"
        onClick={() => onChange(value - step)}
        disabled={value <= min}
        aria-label="Decrease"
      >
        <Minus className="size-3.5" />
      </Button>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(parseFloat(e.currentTarget.value))}
        className="h-2 flex-1 cursor-pointer accent-primary"
      />
      <Button
        type="button"
        size="icon"
        variant="outline"
        className="size-8"
        onClick={() => onChange(value + step)}
        disabled={value >= max}
        aria-label="Increase"
      >
        <Plus className="size-3.5" />
      </Button>
      <span className="w-12 text-right font-mono text-xs tabular-nums text-muted-foreground">
        {format(value)}
      </span>
    </div>
  );
}

function AboutSection() {
  return (
    <div className="flex flex-col items-center gap-4 text-center">
      <img
        src="/vento-logo.svg"
        alt={`${APP_NAME} logo`}
        className="size-24"
        draggable={false}
      />
      <div className="space-y-1">
        <h4 className="font-heading text-xl font-semibold">{APP_NAME}</h4>
        <p className="text-sm text-muted-foreground">Version {APP_VERSION}</p>
      </div>
      <p className="text-sm text-muted-foreground">{APP_DESCRIPTION}</p>
    </div>
  );
}

function AppearanceSection({ layoutMode, onLayoutChange }: Props) {
  const { theme, setTheme } = useTheme();

  return (
    <div className="max-w-md space-y-6">
      <section className="space-y-3">
        <h4 className="text-sm font-medium">Theme</h4>
        <div className="grid grid-cols-3 gap-3">
          {THEMES.map(({ value, label, icon: Icon }) => (
            <button
              key={value}
              type="button"
              onClick={() => setTheme(value)}
              className={cn(
                "flex items-center gap-2 rounded-lg border-2 px-4 py-3 transition-colors hover:bg-muted",
                theme === value
                  ? "border-primary bg-muted"
                  : "border-transparent",
              )}
            >
              <Icon className="size-4 text-muted-foreground" />
              <span className="text-xs font-medium">{label}</span>
            </button>
          ))}
        </div>
      </section>

      <section className="space-y-3">
        <h4 className="text-sm font-medium">Request / Response layout</h4>
        <div className="grid grid-cols-2 gap-3">
          {LAYOUTS.map(({ value, label, icon: Icon }) => (
            <button
              key={value}
              type="button"
              onClick={() => onLayoutChange(value)}
              className={cn(
                "flex items-center gap-2 rounded-lg border-2 px-4 py-3 transition-colors hover:bg-muted",
                layoutMode === value
                  ? "border-primary bg-muted"
                  : "border-transparent",
              )}
            >
              <Icon className="size-4 text-muted-foreground" />
              <span className="text-xs font-medium">{label}</span>
            </button>
          ))}
        </div>
      </section>
    </div>
  );
}
