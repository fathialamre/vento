import { useState } from "react";
import { Columns2, Info, Monitor, Moon, Palette, Rows2, Sun } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useTheme } from "@/components/theme-provider";
import { cn } from "@/lib/utils";

const APP_VERSION = "0.1.0";
const APP_NAME = "Vento";
const APP_DESCRIPTION = "A fast, native API client built with Tauri.";

type SectionId = "appearance" | "about";

type Section = {
  id: SectionId;
  label: string;
  icon: LucideIcon;
};

const SECTIONS: Section[] = [
  { id: "appearance", label: "Appearance", icon: Palette },
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
        {activeId === "about" && <AboutSection />}
      </div>
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
