import React, { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  insertHistory,
  listHistory,
  renameRequest,
  updateRequest,
  type HistoryItem,
} from "@/lib/db";
import { AppSidebar } from "@/components/app-sidebar";
import { SettingsView } from "@/components/settings-view";
import { EnvironmentEditor } from "@/components/environment-editor";
import { ActiveEnvSelect } from "@/components/active-env-select";
import { VarInput } from "@/components/var-input";
import { useActiveEnv } from "@/hooks/use-active-env";
import { resolveVars, resolveVarsInList } from "@/lib/interpolation";
import {
  AlertCircle,
  ChevronDown,
  Columns2,
  Layers,
  Loader2,
  Plus,
  RefreshCcw,
  Rows2,
  Settings as SettingsIcon,
  X,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { JsonViewer } from "@/components/json-tree-viewer";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { ParamsEditor } from "@/components/params-editor";
import { RequestTabs, type BottomTab } from "@/components/request-tabs";
import {
  ResizableHandle,
  ResizablePanel,
  ResizablePanelGroup,
} from "@/components/ui/resizable";
import {
  buildUrl,
  fromPersisted,
  getBase,
  parseUrl,
  toPersisted,
  type QueryParam,
} from "@/lib/url-params";
import { listEnvironments } from "@/lib/db";
import { cn } from "@/lib/utils";

const HTTP_METHODS = [
  "GET",
  "POST",
  "PUT",
  "PATCH",
  "DELETE",
  "HEAD",
  "OPTIONS",
] as const;
type HttpMethod = (typeof HTTP_METHODS)[number];

const METHOD_COLOR: Record<HttpMethod, string> = {
  GET: "text-sky-500",
  POST: "text-teal-500",
  PUT: "text-indigo-500",
  PATCH: "text-fuchsia-500",
  DELETE: "text-rose-500",
  HEAD: "text-cyan-500",
  OPTIONS: "text-slate-400",
};

type RustResponse = {
  status: number;
  duration_ms: number;
  body: string;
  headers: [string, string][];
  size_bytes: number;
};

type ResponseTab = "body" | "headers";

type BodyViewMode = "json" | "pretty" | "raw";

type TabKind = "request" | "settings" | "environment";

type Tab = {
  id: string;
  kind: TabKind;
  name: string;
  requestId?: string; // stable identity for collection requests (dedup)
  environmentId?: number; // populated when kind === "environment"
  isGlobals?: boolean;
  method: HttpMethod;
  url: string;
  params: QueryParam[];
  bottomTab: BottomTab;
  response: string;
  responseHeaders: [string, string][];
  responseSize: number | null;
  responseTab: ResponseTab;
  bodyViewMode: BodyViewMode;
  error: string;
  loading: boolean;
  status: number | null;
  duration_ms: number | null;
};

const MIN_SIDEBAR_WIDTH = 280;
const MAX_SIDEBAR_WIDTH = 800;
const DEFAULT_SIDEBAR_WIDTH = 350;

let tabCounter = 0;
function newTab(overrides?: Partial<Tab>): Tab {
  tabCounter += 1;
  return {
    id: `tab-${Date.now().toString(36)}-${tabCounter}`,
    kind: "request",
    name: "New Request",
    method: "GET",
    url: "",
    params: [],
    bottomTab: "params",
    response: "",
    responseHeaders: [],
    responseSize: null,
    responseTab: "body",
    bodyViewMode: "json",
    error: "",
    loading: false,
    status: null,
    duration_ms: null,
    ...overrides,
  };
}

export default function App() {
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [collectionsRevision, setCollectionsRevision] = useState(0);
  const [environmentsRevision, setEnvironmentsRevision] = useState(0);
  const [sidebarNav, setSidebarNav] = useState<string>("Collections");
  const { activeEnvId, setActiveEnvId, envMap, globalsMap, globalsId } =
    useActiveEnv(environmentsRevision);
  const [layoutMode, setLayoutMode] = useState<"vertical" | "horizontal">(() => {
    const stored = localStorage.getItem("vento:layout-mode");
    return stored === "horizontal" ? "horizontal" : "vertical";
  });
  const [sidebarWidth, setSidebarWidth] = useState<number>(DEFAULT_SIDEBAR_WIDTH);
  const handleSidebarResizeStart = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      e.preventDefault();
      const startX = e.clientX;
      const startWidth = sidebarWidth;

      const prevCursor = document.body.style.cursor;
      const prevUserSelect = document.body.style.userSelect;
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";

      function onMove(ev: MouseEvent) {
        const next = Math.min(
          MAX_SIDEBAR_WIDTH,
          Math.max(MIN_SIDEBAR_WIDTH, startWidth + (ev.clientX - startX)),
        );
        setSidebarWidth(next);
      }

      function onUp() {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
        document.body.style.cursor = prevCursor;
        document.body.style.userSelect = prevUserSelect;
      }

      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [sidebarWidth],
  );
  const tabBarRef = useRef<HTMLDivElement>(null);
  const requestSnapshotsRef = useRef<Map<string, string>>(new Map());
  const saveTimersRef = useRef<Map<string, number>>(new Map());

  useEffect(() => {
    localStorage.setItem("vento:layout-mode", layoutMode);
  }, [layoutMode]);

  const activeTab = tabs.find((t) => t.id === activeId) ?? null;

  const refreshHistory = useCallback(async () => {
    try {
      setHistory(await listHistory(50));
    } catch (e) {
      console.error("history load failed", e);
    }
  }, []);

  useEffect(() => {
    refreshHistory();
  }, [refreshHistory]);

  useEffect(() => {
    if (!activeTab) return;
    const reqId = activeTab.requestId;
    if (!reqId?.startsWith("r:")) return;
    const dbId = parseInt(reqId.slice(2), 10);
    if (isNaN(dbId)) return;

    const sig = JSON.stringify([
      activeTab.method,
      activeTab.url,
      toPersisted(activeTab.params),
    ]);
    if (requestSnapshotsRef.current.get(reqId) === sig) return;

    const prev = saveTimersRef.current.get(reqId);
    if (prev) window.clearTimeout(prev);
    const snapMethod = activeTab.method;
    const snapUrl = activeTab.url;
    const snapParams = activeTab.params;
    const timer = window.setTimeout(async () => {
      saveTimersRef.current.delete(reqId);
      try {
        await updateRequest({
          id: dbId,
          method: snapMethod,
          url: snapUrl,
          params:
            snapParams.length > 0
              ? JSON.stringify(toPersisted(snapParams))
              : null,
        });
        requestSnapshotsRef.current.set(reqId, sig);
        setCollectionsRevision((r) => r + 1);
      } catch (e) {
        console.error("auto-save failed", e);
      }
    }, 500);
    saveTimersRef.current.set(reqId, timer);
  }, [activeTab?.requestId, activeTab?.method, activeTab?.url, activeTab?.params]);

  function updateActive(partial: Partial<Tab>) {
    setTabs((prev) =>
      prev.map((t) => (t.id === activeId ? { ...t, ...partial } : t)),
    );
  }

  function addTab(overrides?: Partial<Tab>) {
    const tab = newTab(overrides);
    setTabs((prev) => [...prev, tab]);
    setActiveId(tab.id);
    setTimeout(() => {
      tabBarRef.current?.scrollTo({ left: 99999, behavior: "smooth" });
    }, 0);
    return tab.id;
  }

  function openSettings() {
    const existing = tabs.find((t) => t.kind === "settings");
    if (existing) {
      setActiveId(existing.id);
      return;
    }
    addTab({ kind: "settings", name: "Settings" });
  }

  const handleEnvSaved = useCallback(() => {
    setEnvironmentsRevision((r) => r + 1);
  }, []);

  function handleOpenEnvironment(envId: number, name: string, isGlobals: boolean) {
    const existing = tabs.find(
      (t) => t.kind === "environment" && t.environmentId === envId,
    );
    if (existing) {
      setActiveId(existing.id);
      return;
    }
    addTab({
      kind: "environment",
      name: isGlobals ? "Globals" : name,
      environmentId: envId,
      isGlobals,
    });
  }

  async function handleVarClick(_name: string, source: "env" | "globals") {
    if (source === "globals" && globalsId !== null) {
      handleOpenEnvironment(globalsId, "Globals", true);
    } else if (source === "env" && activeEnvId !== null) {
      try {
        const envs = await listEnvironments();
        const env = envs.find((e) => e.id === activeEnvId);
        if (env) handleOpenEnvironment(env.id, env.name, false);
      } catch {}
    }
  }

  function closeTab(id: string) {
    setTabs((prev) => {
      const idx = prev.findIndex((t) => t.id === id);
      const next = prev.filter((t) => t.id !== id);
      if (next.length === 0) {
        setActiveId(null);
        return [];
      }
      if (id === activeId) {
        setActiveId(next[Math.min(idx, next.length - 1)].id);
      }
      return next;
    });
  }

  function handleLoadRequest(req: {
    method: string;
    url: string;
    name?: string;
    requestId?: string;
    params?: string | null;
  }) {
    const method = HTTP_METHODS.includes(req.method as HttpMethod)
      ? (req.method as HttpMethod)
      : "GET";
    // reactivate existing tab if same requestId
    if (req.requestId) {
      const existing = tabs.find((t) => t.requestId === req.requestId);
      if (existing) {
        setActiveId(existing.id);
        return;
      }
    }
    const persistedParams = fromPersisted(req.params);
    // If persisted params exist, prefer them; otherwise parse from URL.
    const params =
      persistedParams.length > 0 ? persistedParams : parseUrl(req.url).params;
    if (req.requestId) {
      requestSnapshotsRef.current.set(
        req.requestId,
        JSON.stringify([method, req.url, toPersisted(params)]),
      );
    }
    addTab({
      name: req.name ?? `${method} request`,
      requestId: req.requestId,
      method,
      url: req.url,
      params,
    });
  }

  function handleSelectHistory(item: HistoryItem) {
    handleLoadRequest({ method: item.method, url: item.url, params: item.params });
  }

  function handleUrlChange(newUrl: string) {
    const parsed = parseUrl(newUrl);
    updateActive({ url: newUrl, params: parsed.params });
  }

  function handleParamsChange(next: QueryParam[]) {
    const tab = tabs.find((t) => t.id === activeId);
    if (!tab) return;
    const base = getBase(tab.url);
    const built = buildUrl(base, next);
    updateActive({ params: next, url: built });
  }

  async function sendRequest() {
    const tab = tabs.find((t) => t.id === activeId);
    if (!tab) return;
    if (!tab.url.trim()) {
      updateActive({ error: "URL is required" });
      return;
    }
    setTabs((prev) =>
      prev.map((t) =>
        t.id === activeId
          ? {
              ...t,
              loading: true,
              error: "",
              response: "",
              responseHeaders: [],
              responseSize: null,
              status: null,
              duration_ms: null,
            }
          : t,
      ),
    );
    let status: number | null = null;
    let duration: number | null = null;
    let preview: string | null = null;
    try {
      const activeParams = (tab.params ?? []).filter((p) => p.enabled && p.key);
      const baseUrl = getBase(tab.url);
      const { output: resolvedUrl, unresolved: urlUnresolved } = resolveVars(
        baseUrl,
        envMap,
        globalsMap,
      );
      const { outputs: resolvedValues, unresolved: paramUnresolved } =
        resolveVarsInList(
          activeParams.map((p) => p.value),
          envMap,
          globalsMap,
        );
      const unresolved = Array.from(
        new Set([...urlUnresolved, ...paramUnresolved]),
      );
      if (unresolved.length) {
        console.warn("Unresolved environment vars:", unresolved);
      }
      const res = await invoke<RustResponse>("send_request", {
        method: tab.method,
        url: resolvedUrl,
        params: activeParams.map((p, i) => ({
          key: p.key,
          value: resolvedValues[i],
          encode: p.encode,
        })),
      });
      status = res.status;
      duration = res.duration_ms;
      preview = res.body.slice(0, 500);
      let bodyViewMode: BodyViewMode = "raw";
      try {
        JSON.parse(res.body);
        bodyViewMode = "json";
      } catch {
        // not JSON
      }
      setTabs((prev) =>
        prev.map((t) =>
          t.id === activeId
            ? {
                ...t,
                loading: false,
                response: res.body,
                responseHeaders: res.headers,
                responseSize: res.size_bytes,
                status,
                duration_ms: duration,
                bodyViewMode,
              }
            : t,
        ),
      );
    } catch (e) {
      const err = String(e);
      preview = err.slice(0, 500);
      setTabs((prev) =>
        prev.map((t) =>
          t.id === activeId ? { ...t, loading: false, error: err } : t,
        ),
      );
    } finally {
      try {
        await insertHistory({
          method: tab.method,
          url: tab.url,
          status,
          duration_ms: duration,
          response_preview: preview,
          params: (tab.params ?? []).length > 0 ? JSON.stringify(toPersisted(tab.params ?? [])) : null,
        });
        await refreshHistory();
      } catch (e) {
        console.error("history insert failed", e);
      }
    }
  }

  return (
    <SidebarProvider
      style={{ "--sidebar-width": `${sidebarWidth}px` } as React.CSSProperties}
    >
      <AppSidebar
        history={history}
        methodColor={METHOD_COLOR}
        onSelectHistory={handleSelectHistory}
        onLoadRequest={handleLoadRequest}
        openRequestIds={new Set(tabs.map((t) => t.requestId).filter(Boolean) as string[])}
        activeRequestId={activeTab?.requestId}
        collectionsRevision={collectionsRevision}
        environmentsRevision={environmentsRevision}
        onOpenSettings={openSettings}
        onOpenEnvironment={handleOpenEnvironment}
        activeEnvId={activeEnvId}
        activeNavTitle={sidebarNav}
        onActiveNavChange={setSidebarNav}
        onResizeStart={handleSidebarResizeStart}
      />
      <SidebarInset className="flex flex-col overflow-hidden">
        {/* Top bar */}
        <header className="flex h-14 shrink-0 items-center gap-2 border-b bg-background px-3">
          <SidebarTrigger className="-ml-1" />

          {/* Tab bar */}
          <div
            ref={tabBarRef}
            className="flex min-w-0 flex-1 items-center gap-0.5 overflow-x-auto"
            style={{ scrollbarWidth: "none" }}
          >
            {tabs.map((tab) => (
              <TabItem
                key={tab.id}
                tab={tab}
                isActive={tab.id === activeId}
                methodColor={METHOD_COLOR}
                onActivate={() => setActiveId(tab.id)}
                onClose={() => closeTab(tab.id)}
                onRename={async (name) => {
                  setTabs((prev) =>
                    prev.map((t) => (t.id === tab.id ? { ...t, name } : t)),
                  );
                  if (tab.requestId?.startsWith("r:")) {
                    const dbId = parseInt(tab.requestId.slice(2), 10);
                    if (!isNaN(dbId)) {
                      try {
                        await renameRequest(dbId, name);
                        setCollectionsRevision((r) => r + 1);
                      } catch (e) {
                        console.error("rename failed", e);
                      }
                    }
                  }
                }}
              />
            ))}
            <button
              type="button"
              onClick={() => addTab()}
              className="flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
              title="New tab"
            >
              <Plus className="size-3.5" />
            </button>
          </div>

          <ActiveEnvSelect
            activeEnvId={activeEnvId}
            onChange={setActiveEnvId}
            onManage={() => setSidebarNav("Environments")}
            refreshKey={environmentsRevision}
          />
          <ViewMenu layoutMode={layoutMode} onLayoutChange={setLayoutMode} />
        </header>

        {/* Active tab content */}
        {!activeTab ? (
          <div className="flex flex-1 items-center justify-center">
            <Empty className="bg-muted/20">
              <EmptyHeader>
                <EmptyMedia variant="icon">
                  <Plus />
                </EmptyMedia>
                <EmptyTitle>No open requests</EmptyTitle>
                <EmptyDescription>Create a new request or pick one from your collections.</EmptyDescription>
              </EmptyHeader>
              <EmptyContent>
                <Button onClick={() => addTab()}>
                  <Plus className="size-4" />
                  New Request
                </Button>
              </EmptyContent>
            </Empty>
          </div>
        ) : null}
        {activeTab && activeTab.kind === "settings" && (
          <div className="flex flex-1 overflow-hidden">
            <SettingsView layoutMode={layoutMode} onLayoutChange={setLayoutMode} />
          </div>
        )}
        {activeTab &&
          activeTab.kind === "environment" &&
          activeTab.environmentId !== undefined && (
            <div className="flex flex-1 overflow-hidden">
              <EnvironmentEditor
                environmentId={activeTab.environmentId}
                isGlobals={activeTab.isGlobals ?? false}
                onSaved={handleEnvSaved}
              />
            </div>
          )}
        {activeTab && activeTab.kind === "request" && <div className="flex flex-1 flex-col overflow-hidden">
          <form
            className="flex w-full items-stretch gap-2 p-4 pb-2"
            onSubmit={(e) => {
              e.preventDefault();
              sendRequest();
            }}
          >
            <div className="flex flex-1 items-stretch">
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button
                    type="button"
                    variant="outline"
                    size="lg"
                    className={cn(
                      "w-[120px] justify-between rounded-r-none border-r-0 font-bold",
                      METHOD_COLOR[activeTab.method],
                    )}
                  >
                    {activeTab.method}
                    <ChevronDown className="size-4 opacity-70" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start" className="w-[120px]">
                  {HTTP_METHODS.map((m) => (
                    <DropdownMenuItem
                      key={m}
                      onSelect={() => updateActive({ method: m })}
                      className={cn("font-bold", METHOD_COLOR[m])}
                    >
                      {m}
                    </DropdownMenuItem>
                  ))}
                </DropdownMenuContent>
              </DropdownMenu>

              <VarInput
                key={activeTab.id}
                value={activeTab.url}
                onChange={handleUrlChange}
                envMap={envMap}
                globalsMap={globalsMap}
                onVarClick={handleVarClick}
                placeholder="https://api.example.com/endpoint"
                className="h-9 rounded-l-none focus-visible:z-10"
                containerClassName="flex-1"
              />
            </div>
            <Button
              type="submit"
              size="lg"
              disabled={activeTab.loading}
              className="gap-1.5 px-6"
            >
              Send
              {activeTab.loading && <Loader2 className="size-4 animate-spin" />}
            </Button>
          </form>

          <ResizablePanelGroup orientation={layoutMode} className="flex-1">
            <ResizablePanel defaultSize="45%" minSize="15%">
              <div className="flex h-full flex-col gap-2 overflow-auto p-4 pt-2">
                <RequestTabs
                  active={activeTab.bottomTab ?? "params"}
                  onChange={(t) => updateActive({ bottomTab: t })}
                  paramsCount={(activeTab.params ?? []).filter((p) => p.enabled && p.key).length}
                />
                {(activeTab.bottomTab ?? "params") === "params" && (
                  <ParamsEditor
                    params={activeTab.params ?? []}
                    onChange={handleParamsChange}
                  />
                )}
              </div>
            </ResizablePanel>
            <ResizableHandle withHandle />
            <ResizablePanel defaultSize="55%" minSize="15%">
              <div className="flex h-full flex-col gap-2 overflow-auto p-4 pt-2">
                {activeTab.error && (
                  <RequestErrorState error={activeTab.error} onRetry={sendRequest} />
                )}
                {activeTab.response && (
                  <>
                    <ResponseBar
                      status={activeTab.status}
                      durationMs={activeTab.duration_ms}
                      sizeBytes={activeTab.responseSize}
                      active={activeTab.responseTab ?? "body"}
                      headersCount={activeTab.responseHeaders.length}
                      onChange={(t) => updateActive({ responseTab: t })}
                    />
                    {(activeTab.responseTab ?? "body") === "body" ? (
                      <ResponseView raw={activeTab.response} />
                    ) : (
                      <HeadersView headers={activeTab.responseHeaders} />
                    )}
                  </>
                )}
              </div>
            </ResizablePanel>
          </ResizablePanelGroup>
        </div>}
      </SidebarInset>
    </SidebarProvider>
  );
}

function TabItem({
  tab,
  isActive,
  methodColor,
  onActivate,
  onClose,
  onRename,
}: {
  tab: Tab;
  isActive: boolean;
  methodColor: Record<string, string>;
  onActivate: () => void;
  onClose: () => void;
  onRename: (name: string) => void;
}) {
  const [editing, setEditing] = React.useState(false);
  const [draft, setDraft] = React.useState("");
  const inputRef = React.useRef<HTMLInputElement>(null);

  function startEdit(e: React.MouseEvent) {
    e.stopPropagation();
    setDraft(tab.name);
    setEditing(true);
    setTimeout(() => inputRef.current?.select(), 0);
  }

  function commit() {
    const name = draft.trim() || tab.name;
    onRename(name);
    setEditing(false);
  }

  return (
    <button
      type="button"
      onClick={onActivate}
      className={cn(
        "group flex h-7 shrink-0 items-center gap-1.5 rounded-md px-2.5 text-xs font-medium transition-colors",
        isActive
          ? "bg-accent text-accent-foreground"
          : "text-muted-foreground hover:bg-accent/50 hover:text-foreground",
      )}
    >
      {tab.kind === "settings" ? (
        <SettingsIcon className="size-3 shrink-0 text-muted-foreground" />
      ) : tab.kind === "environment" ? (
        <Layers className="size-3 shrink-0 text-muted-foreground" />
      ) : (
        <span className={cn("text-[10px] font-bold", methodColor[tab.method])}>
          {tab.method}
        </span>
      )}

      {editing ? (
        <input
          ref={inputRef}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
            if (e.key === "Escape") setEditing(false);
            e.stopPropagation();
          }}
          onClick={(e) => e.stopPropagation()}
          className="w-[100px] bg-transparent outline-none"
          autoFocus
        />
      ) : (
        <span
          className="max-w-[120px] truncate"
          onDoubleClick={tab.kind === "request" ? startEdit : undefined}
        >
          {tab.name}
        </span>
      )}

      {tab.loading && <Loader2 className="size-3 shrink-0 animate-spin" />}

      <span
        role="button"
        tabIndex={0}
        onClick={(e) => { e.stopPropagation(); onClose(); }}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") { e.stopPropagation(); onClose(); }
        }}
        className={cn(
          "flex size-4 shrink-0 items-center justify-center rounded hover:bg-muted-foreground/20",
          isActive ? "opacity-60 hover:opacity-100" : "opacity-0 group-hover:opacity-60",
        )}
      >
        <X className="size-2.5" />
      </span>
    </button>
  );
}

function ViewMenu({
  layoutMode,
  onLayoutChange,
}: {
  layoutMode: "vertical" | "horizontal";
  onLayoutChange: (m: "vertical" | "horizontal") => void;
}) {
  const Icon = layoutMode === "vertical" ? Rows2 : Columns2;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="size-8 shrink-0"
          title="View options"
        >
          <Icon className="size-4" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-48">
        <DropdownMenuLabel>Layout</DropdownMenuLabel>
        <DropdownMenuCheckboxItem
          checked={layoutMode === "vertical"}
          onCheckedChange={() => onLayoutChange("vertical")}
        >
          <Rows2 className="mr-2 size-4" />
          Stacked
        </DropdownMenuCheckboxItem>
        <DropdownMenuCheckboxItem
          checked={layoutMode === "horizontal"}
          onCheckedChange={() => onLayoutChange("horizontal")}
        >
          <Columns2 className="mr-2 size-4" />
          Side by side
        </DropdownMenuCheckboxItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem disabled>More options soon…</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MB`;
}

function ResponseBar({
  status,
  durationMs,
  sizeBytes,
  active,
  headersCount,
  onChange,
}: {
  status: number | null;
  durationMs: number | null;
  sizeBytes: number | null;
  active: ResponseTab;
  headersCount: number;
  onChange: (t: ResponseTab) => void;
}) {
  const statusColor =
    status === null
      ? "text-muted-foreground"
      : status < 300
        ? "text-teal-500"
        : status < 400
          ? "text-amber-500"
          : "text-rose-500";

  const tabs: { id: ResponseTab; label: string; count?: number }[] = [
    { id: "body", label: "Body" },
    { id: "headers", label: "Headers", count: headersCount },
  ];

  return (
    <div className="flex items-center justify-between gap-2 border-b">
      <div className="flex items-center gap-0.5">
        {tabs.map((t) => {
          const isActive = active === t.id;
          return (
            <button
              key={t.id}
              type="button"
              onClick={() => onChange(t.id)}
              className={cn(
                "relative flex h-9 items-center gap-1.5 border-b-2 border-transparent px-3 text-xs font-medium transition-colors",
                isActive
                  ? "border-foreground text-foreground"
                  : "text-muted-foreground hover:text-foreground",
              )}
            >
              {t.label}
              {t.count !== undefined && t.count > 0 && (
                <span className="rounded bg-accent px-1 py-0.5 text-[10px] font-bold text-accent-foreground">
                  {t.count}
                </span>
              )}
            </button>
          );
        })}
      </div>
      <div className="flex items-center gap-3 pr-1 text-xs">
        {status !== null && (
          <span className={cn("font-mono font-semibold", statusColor)}>
            {status}
          </span>
        )}
        {durationMs !== null && (
          <span className="text-muted-foreground">{durationMs} ms</span>
        )}
        {sizeBytes !== null && (
          <span className="text-muted-foreground">{formatBytes(sizeBytes)}</span>
        )}
      </div>
    </div>
  );
}

function HeadersView({ headers }: { headers: [string, string][] }) {
  if (headers.length === 0) {
    return (
      <div className="rounded-md border bg-card p-4 text-xs text-muted-foreground">
        No headers.
      </div>
    );
  }
  return (
    <div className="overflow-auto rounded-md border bg-card">
      <div className="grid grid-cols-[200px_1fr] border-b bg-muted/40 text-xs font-medium text-muted-foreground">
        <div className="px-3 py-1.5">Name</div>
        <div className="px-3 py-1.5">Value</div>
      </div>
      {headers.map(([k, v], i) => (
        <div
          key={`${k}-${i}`}
          className="grid grid-cols-[200px_1fr] border-b border-border/60 last:border-b-0 font-mono text-xs"
        >
          <div className="break-all px-3 py-1.5 font-semibold">{k}</div>
          <div className="break-all px-3 py-1.5 text-muted-foreground">{v}</div>
        </div>
      ))}
    </div>
  );
}

function ResponseView({ raw }: { raw: string }) {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return (
      <pre className="overflow-auto rounded-md border bg-card p-4 font-mono text-sm whitespace-pre-wrap break-words text-foreground">
        {raw}
      </pre>
    );
  }
  return (
    <div className="overflow-auto rounded-md border bg-card p-4">
      <JsonViewer data={parsed} />
    </div>
  );
}

function RequestErrorState({ error, onRetry }: { error: string; onRetry: () => void }) {
  const short = error.length > 120 ? error.slice(0, 120) + "…" : error;
  return (
    <Empty className="h-full min-h-[240px] bg-muted/30">
      <EmptyHeader>
        <EmptyMedia variant="icon" className="bg-destructive/10 text-destructive">
          <AlertCircle />
        </EmptyMedia>
        <EmptyTitle>Request Failed</EmptyTitle>
        <EmptyDescription className="max-w-sm text-pretty font-mono text-xs">
          {short}
        </EmptyDescription>
      </EmptyHeader>
      <EmptyContent>
        <Button variant="outline" onClick={onRetry}>
          <RefreshCcw className="size-4" />
          Retry
        </Button>
      </EmptyContent>
    </Empty>
  );
}
