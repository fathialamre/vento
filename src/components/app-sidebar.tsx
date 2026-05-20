import * as React from "react";
import { FolderOpen, History, Layers, Settings } from "lucide-react";

import { CollectionsPanel } from "@/components/collections-panel";
import { EnvironmentsPanel } from "@/components/environments-panel";
import { NavUser } from "@/components/nav-user";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarHeader,
  SidebarInput,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  useSidebar,
} from "@/components/ui/sidebar";
import type { HistoryItem } from "@/lib/db";

const user = {
  name: "shadcn",
  email: "m@example.com",
  avatar: "/avatars/shadcn.jpg",
};

type NavItem = { title: string; icon: React.ComponentType<{ className?: string }> };

const navMain: NavItem[] = [
  { title: "Collections", icon: FolderOpen },
  { title: "Environments", icon: Layers },
  { title: "History", icon: History },
];

type AppSidebarProps = React.ComponentProps<typeof Sidebar> & {
  history: HistoryItem[];
  methodColor: Record<string, string>;
  onSelectHistory: (item: HistoryItem) => void;
  onLoadRequest: (req: { method: string; url: string; name?: string; requestId?: string; params?: string | null; body?: string | null }) => void;
  openRequestIds: Set<string>;
  activeRequestId?: string;
  collectionsRevision?: number;
  environmentsRevision?: number;
  onOpenSettings: () => void;
  onOpenEnvironment: (envId: number, name: string, isGlobals: boolean) => void;
  activeEnvId: number | null;
  activeNavTitle: string;
  onActiveNavChange: (title: string) => void;
  onResizeStart: (e: React.MouseEvent<HTMLDivElement>) => void;
};

export function AppSidebar({
  history,
  methodColor,
  onSelectHistory,
  onLoadRequest,
  openRequestIds,
  activeRequestId,
  collectionsRevision,
  environmentsRevision,
  onOpenSettings,
  onOpenEnvironment,
  activeEnvId,
  activeNavTitle,
  onActiveNavChange,
  onResizeStart,
  ...props
}: AppSidebarProps) {
  const [query, setQuery] = React.useState("");
  const { setOpen } = useSidebar();

  const filtered = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return history;
    return history.filter(
      (h) =>
        h.url.toLowerCase().includes(q) ||
        h.method.toLowerCase().includes(q),
    );
  }, [history, query]);

  const placeholder =
    activeNavTitle === "Collections"
      ? "Search collections..."
      : activeNavTitle === "Environments"
        ? "Search environments..."
        : "Search history...";

  return (
    <Sidebar
      collapsible="icon"
      className="overflow-hidden *:data-[sidebar=sidebar]:flex-row"
      {...props}
    >
      <Sidebar
        collapsible="none"
        className="w-[calc(var(--sidebar-width-icon)+1px)]! border-r"
      >
        <SidebarHeader>
          <SidebarMenu>
            <SidebarMenuItem>
              <SidebarMenuButton size="lg" asChild className="md:h-8 md:p-0">
                <a href="#" className="justify-center">
                  <img
                    src="/vento-logo.svg"
                    alt="Vento"
                    className="size-8"
                    draggable={false}
                  />
                </a>
              </SidebarMenuButton>
            </SidebarMenuItem>
          </SidebarMenu>
        </SidebarHeader>
        <SidebarContent>
          <SidebarGroup>
            <SidebarGroupContent className="px-1.5 md:px-0">
              <SidebarMenu>
                {navMain.map((item) => (
                  <SidebarMenuItem key={item.title}>
                    <SidebarMenuButton
                      tooltip={{ children: item.title, hidden: false }}
                      onClick={() => {
                        onActiveNavChange(item.title);
                        setOpen(true);
                      }}
                      isActive={activeNavTitle === item.title}
                      className="px-2.5 data-active:bg-primary/15 data-active:text-primary hover:data-active:bg-primary/20 md:px-2"
                    >
                      <item.icon />
                      <span>{item.title}</span>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                ))}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        </SidebarContent>
        <SidebarFooter>
          <SidebarMenu>
            <SidebarMenuItem>
              <SidebarMenuButton
                tooltip={{ children: "Settings", hidden: false }}
                onClick={onOpenSettings}
                className="px-2.5 md:px-2"
              >
                <Settings />
                <span>Settings</span>
              </SidebarMenuButton>
            </SidebarMenuItem>
          </SidebarMenu>
          <NavUser user={user} />
        </SidebarFooter>
      </Sidebar>

      <Sidebar collapsible="none" className="hidden flex-1 md:flex">
        <SidebarHeader className="gap-3.5 border-b p-4">
          <div className="flex w-full items-center justify-between">
            <div className="text-base font-medium text-foreground">
              {activeNavTitle}
            </div>
            {activeNavTitle === "History" && (
              <span className="text-xs text-muted-foreground">
                {filtered.length}
              </span>
            )}
          </div>
          <SidebarInput
            placeholder={placeholder}
            value={query}
            onChange={(e) => setQuery(e.currentTarget.value)}
          />
        </SidebarHeader>
        <SidebarContent>
          {activeNavTitle === "Collections" ? (
            <CollectionsPanel
              onLoadRequest={onLoadRequest}
              openRequestIds={openRequestIds}
              activeRequestId={activeRequestId}
              query={query}
              refreshKey={collectionsRevision}
            />
          ) : activeNavTitle === "Environments" ? (
            <EnvironmentsPanel
              onOpenEnvironment={onOpenEnvironment}
              activeEnvId={activeEnvId}
              query={query}
              refreshKey={environmentsRevision}
            />
          ) : (
            <SidebarGroup className="px-0">
              <SidebarGroupContent>
                {filtered.length === 0 && (
                  <div className="p-4 text-sm text-muted-foreground">
                    No requests yet. Send one to see it here.
                  </div>
                )}
                {filtered.map((item) => (
                  <button
                    type="button"
                    key={item.id}
                    onClick={() => onSelectHistory(item)}
                    className="flex w-full flex-col items-start gap-1 border-b p-4 text-left text-sm leading-tight last:border-b-0 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
                  >
                    <div className="flex w-full items-center gap-2">
                      <span
                        className={`w-14 shrink-0 text-xs font-bold ${methodColor[item.method] ?? "text-foreground"}`}
                      >
                        {item.method}
                      </span>
                      <span className="ml-auto text-xs text-muted-foreground">
                        {formatRelative(item.sent_at)}
                      </span>
                    </div>
                    <span className="w-full text-xs">
                      {item.url}
                    </span>
                    {(item.status !== null || item.duration_ms !== null) && (
                      <span className="text-[11px] text-muted-foreground">
                        {item.status !== null && <>status {item.status}</>}
                        {item.status !== null && item.duration_ms !== null && (
                          <> • </>
                        )}
                        {item.duration_ms !== null && <>{item.duration_ms}ms</>}
                      </span>
                    )}
                  </button>
                ))}
              </SidebarGroupContent>
            </SidebarGroup>
          )}
        </SidebarContent>
      </Sidebar>

      <div
        role="separator"
        aria-orientation="vertical"
        onMouseDown={onResizeStart}
        className="absolute inset-y-0 -right-0.5 z-20 hidden w-1 cursor-col-resize bg-transparent transition-colors hover:bg-border active:bg-primary/40 md:block group-data-[collapsible=icon]:hidden"
      />
    </Sidebar>
  );
}

function formatRelative(ts: number): string {
  const diffMs = Date.now() - ts;
  const sec = Math.floor(diffMs / 1000);
  if (sec < 60) return `${sec}s`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}h`;
  const day = Math.floor(hr / 24);
  if (day < 7) return `${day}d`;
  return new Date(ts).toLocaleDateString();
}
