import * as React from "react";
import { FolderOpen, FolderPlus, FilePlus, MoreHorizontal, Pencil, Plus, Trash2 } from "lucide-react";
import { TreeView, type TreeDataItem, type TreeRenderItemParams } from "@/components/tree-view";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  createCollection,
  createFolder,
  createRequest,
  deleteCollection,
  deleteFolder,
  deleteRequest,
  getCollectionsTree,
  renameRequest,
  type TreeCollection,
  type TreeFolder,
} from "@/lib/db";

const HTTP_METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] as const;

const METHOD_COLOR: Record<string, string> = {
  GET: "text-sky-600 dark:text-sky-400",
  POST: "text-teal-600 dark:text-teal-400",
  PUT: "text-indigo-600 dark:text-indigo-400",
  PATCH: "text-fuchsia-600 dark:text-fuchsia-400",
  DELETE: "text-rose-600 dark:text-rose-400",
  HEAD: "text-cyan-600 dark:text-cyan-400",
  OPTIONS: "text-slate-500 dark:text-slate-400",
};

type SelectedNode =
  | { kind: "collection"; collectionId: number; name: string }
  | { kind: "folder"; collectionId: number; folderId: number; name: string }
  | { kind: "request"; collectionId: number; folderId: number | null; requestId: number; name: string }
  | null;

type DialogState =
  | { type: "new-collection" }
  | { type: "new-folder" }
  | { type: "new-request" }
  | { type: "delete" }
  | { type: "rename" }
  | null;

type Props = {
  onLoadRequest: (req: { method: string; url: string; name?: string; requestId?: string; params?: string | null }) => void;
  openRequestIds: Set<string>;
  activeRequestId?: string;
  query?: string;
  refreshKey?: number;
};

export function CollectionsPanel({ onLoadRequest, query = "", refreshKey }: Props) {
  const [tree, setTree] = React.useState<TreeCollection[]>([]);
  const [selected, setSelected] = React.useState<SelectedNode>(null);
  const [dialog, setDialog] = React.useState<DialogState>(null);

  const refresh = React.useCallback(async () => {
    try {
      setTree(await getCollectionsTree());
    } catch (e) {
      console.error("collections load failed", e);
    }
  }, []);

  React.useEffect(() => {
    refresh();
  }, [refresh, refreshKey]);

  const data = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    const filtered = q ? filterTree(tree, q) : tree;
    return buildTreeData(filtered, onLoadRequest, setSelected);
  }, [tree, onLoadRequest, query]);

  const canAddFolder = selected && selected.kind !== "request";
  const canDelete = selected !== null;

  const renderItem = React.useCallback(
    ({ item, isLeaf, isSelected }: TreeRenderItemParams) => {
      const Icon = isLeaf ? (item.icon ?? null) : (item.icon ?? FolderOpen);
      const method = isLeaf ? item.meta?.method : undefined;
      const isRequest = isLeaf && item.meta?.kind === "request";

      function buildNode(): SelectedNode | null {
        if (!item.meta?.kind) return null;
        if (item.meta.kind === "request") {
          return {
            kind: "request",
            collectionId: parseInt(item.meta.collectionId),
            folderId: item.meta.folderId ? parseInt(item.meta.folderId) : null,
            requestId: parseInt(item.meta.requestId),
            name: item.name,
          };
        }
        return null;
      }

      return (
        <span className="flex flex-1 items-center gap-1.5 overflow-hidden">
          {Icon && (
            <Icon className={cn("size-4 shrink-0", isSelected ? "text-accent-foreground" : "text-muted-foreground")} />
          )}
          <span className="flex-1 truncate text-sm font-normal">{item.name}</span>
          {method && (
            <span className={cn("shrink-0 text-[10px] font-bold", METHOD_COLOR[method] ?? "text-foreground")}>
              {method}
            </span>
          )}
          {isRequest && (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <button
                  type="button"
                  className="shrink-0 rounded p-0.5 opacity-0 group-hover:opacity-100 hover:bg-accent-foreground/10 focus:opacity-100"
                  onClick={(e) => e.stopPropagation()}
                >
                  <MoreHorizontal className="size-3.5" />
                </button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" onClick={(e) => e.stopPropagation()}>
                <DropdownMenuItem
                  onClick={(e) => {
                    e.stopPropagation();
                    const node = buildNode();
                    if (node) setSelected(node);
                    setDialog({ type: "rename" });
                  }}
                >
                  <Pencil className="size-4" />
                  Rename
                </DropdownMenuItem>
                <DropdownMenuItem
                  className="text-destructive focus:text-destructive"
                  onClick={(e) => {
                    e.stopPropagation();
                    const node = buildNode();
                    if (node) setSelected(node);
                    setDialog({ type: "delete" });
                  }}
                >
                  <Trash2 className="size-4" />
                  Delete
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </span>
      );
    },
    [setSelected, setDialog],
  );

  return (
    <div className="flex h-full flex-col">
      <div className="flex flex-wrap items-center gap-1 border-b p-2">
        <Button
          size="sm"
          variant="ghost"
          onClick={() => setDialog({ type: "new-collection" })}
          title="New collection"
        >
          <Plus className="size-4" />
          Collection
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={() => setDialog({ type: "new-folder" })}
          disabled={!canAddFolder}
          title="New folder"
        >
          <FolderPlus className="size-4" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={() => setDialog({ type: "new-request" })}
          disabled={!canAddFolder}
          title="New request"
        >
          <FilePlus className="size-4" />
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={() => setDialog({ type: "delete" })}
          disabled={!canDelete}
          title="Delete"
          className="ml-auto text-destructive hover:text-destructive"
        >
          <Trash2 className="size-4" />
        </Button>
      </div>

      <div className="flex-1 overflow-auto">
        {tree.length === 0 ? (
          <div className="p-4 text-sm text-muted-foreground">
            No collections. Click + Collection to create one.
          </div>
        ) : (
          <TreeView
            data={data}
            defaultNodeIcon={FolderOpen}
            renderItem={renderItem}
          />
        )}
      </div>

      <NewCollectionDialog
        open={dialog?.type === "new-collection"}
        onOpenChange={(open) => !open && setDialog(null)}
        onConfirm={async (name) => {
          await createCollection(name);
          setDialog(null);
          await refresh();
        }}
      />

      <NewFolderDialog
        open={dialog?.type === "new-folder"}
        onOpenChange={(open) => !open && setDialog(null)}
        onConfirm={async (name) => {
          if (!selected || selected.kind === "request") return;
          await createFolder({
            collection_id: selected.collectionId,
            parent_folder_id: selected.kind === "folder" ? selected.folderId : null,
            name,
          });
          setDialog(null);
          await refresh();
        }}
      />

      <NewRequestDialog
        open={dialog?.type === "new-request"}
        onOpenChange={(open) => !open && setDialog(null)}
        onConfirm={async ({ name, method, url }) => {
          if (!selected || selected.kind === "request") return;
          await createRequest({
            collection_id: selected.collectionId,
            folder_id: selected.kind === "folder" ? selected.folderId : null,
            name,
            method,
            url,
            params: null,
          });
          setDialog(null);
          await refresh();
        }}
      />

      <DeleteDialog
        open={dialog?.type === "delete"}
        itemName={selected?.name ?? ""}
        onOpenChange={(open) => !open && setDialog(null)}
        onConfirm={async () => {
          if (!selected) return;
          if (selected.kind === "collection") await deleteCollection(selected.collectionId);
          else if (selected.kind === "folder") await deleteFolder(selected.folderId);
          else await deleteRequest(selected.requestId);
          setSelected(null);
          setDialog(null);
          await refresh();
        }}
      />

      <RenameDialog
        open={dialog?.type === "rename"}
        initialName={selected?.name ?? ""}
        onOpenChange={(open) => !open && setDialog(null)}
        onConfirm={async (name) => {
          if (!selected || selected.kind !== "request") return;
          await renameRequest(selected.requestId, name);
          setSelected({ ...selected, name });
          setDialog(null);
          await refresh();
        }}
      />
    </div>
  );
}

function NewCollectionDialog({
  open,
  onOpenChange,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: (name: string) => Promise<void>;
}) {
  const [name, setName] = React.useState("");
  const [loading, setLoading] = React.useState(false);

  React.useEffect(() => {
    if (open) setName("");
  }, [open]);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim()) return;
    setLoading(true);
    try {
      await onConfirm(name.trim());
    } finally {
      setLoading(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New Collection</DialogTitle>
        </DialogHeader>
        <form onSubmit={submit} className="flex flex-col gap-4">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="col-name">Name</Label>
            <Input
              id="col-name"
              placeholder="My API"
              value={name}
              onChange={(e) => setName(e.currentTarget.value)}
              autoFocus
            />
          </div>
          <DialogFooter>
            <Button type="submit" disabled={!name.trim() || loading}>
              Create
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function NewFolderDialog({
  open,
  onOpenChange,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: (name: string) => Promise<void>;
}) {
  const [name, setName] = React.useState("");
  const [loading, setLoading] = React.useState(false);

  React.useEffect(() => {
    if (open) setName("");
  }, [open]);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim()) return;
    setLoading(true);
    try {
      await onConfirm(name.trim());
    } finally {
      setLoading(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New Folder</DialogTitle>
        </DialogHeader>
        <form onSubmit={submit} className="flex flex-col gap-4">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="folder-name">Name</Label>
            <Input
              id="folder-name"
              placeholder="Auth"
              value={name}
              onChange={(e) => setName(e.currentTarget.value)}
              autoFocus
            />
          </div>
          <DialogFooter>
            <Button type="submit" disabled={!name.trim() || loading}>
              Create
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function NewRequestDialog({
  open,
  onOpenChange,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: (input: { name: string; method: string; url: string }) => Promise<void>;
}) {
  const [name, setName] = React.useState("");
  const [method, setMethod] = React.useState("GET");
  const [url, setUrl] = React.useState("https://");
  const [loading, setLoading] = React.useState(false);

  React.useEffect(() => {
    if (open) {
      setName("");
      setMethod("GET");
      setUrl("https://");
    }
  }, [open]);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim() || !url.trim()) return;
    setLoading(true);
    try {
      await onConfirm({ name: name.trim(), method, url: url.trim() });
    } finally {
      setLoading(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New Request</DialogTitle>
        </DialogHeader>
        <form onSubmit={submit} className="flex flex-col gap-4">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="req-name">Name</Label>
            <Input
              id="req-name"
              placeholder="Get users"
              value={name}
              onChange={(e) => setName(e.currentTarget.value)}
              autoFocus
            />
          </div>
          <div className="flex gap-2">
            <div className="flex flex-col gap-1.5">
              <Label>Method</Label>
              <Select value={method} onValueChange={setMethod}>
                <SelectTrigger className="w-[110px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {HTTP_METHODS.map((m) => (
                    <SelectItem key={m} value={m}>
                      {m}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="flex flex-1 flex-col gap-1.5">
              <Label htmlFor="req-url">URL</Label>
              <Input
                id="req-url"
                placeholder="https://api.example.com/users"
                value={url}
                onChange={(e) => setUrl(e.currentTarget.value)}
              />
            </div>
          </div>
          <DialogFooter>
            <Button type="submit" disabled={!name.trim() || !url.trim() || loading}>
              Create
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function DeleteDialog({
  open,
  itemName,
  onOpenChange,
  onConfirm,
}: {
  open: boolean;
  itemName: string;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => Promise<void>;
}) {
  const [loading, setLoading] = React.useState(false);

  async function handleConfirm() {
    setLoading(true);
    try {
      await onConfirm();
    } finally {
      setLoading(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Delete "{itemName}"?</DialogTitle>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">
          This will permanently delete the item and all its contents.
        </p>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={loading}>
            Cancel
          </Button>
          <Button variant="destructive" onClick={handleConfirm} disabled={loading}>
            Delete
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function filterTree(tree: TreeCollection[], q: string): TreeCollection[] {
  return tree.flatMap((c) => {
    const folders = filterFolders(c.folders, q);
    const requests = c.requests.filter(
      (r) =>
        r.name.toLowerCase().includes(q) ||
        r.url.toLowerCase().includes(q) ||
        r.method.toLowerCase().includes(q),
    );
    if (c.name.toLowerCase().includes(q) || folders.length || requests.length)
      return [{ ...c, folders, requests }];
    return [];
  });
}

function filterFolders(folders: TreeFolder[], q: string): TreeFolder[] {
  return folders.flatMap((f) => {
    const subs = filterFolders(f.folders, q);
    const requests = f.requests.filter(
      (r) =>
        r.name.toLowerCase().includes(q) ||
        r.url.toLowerCase().includes(q) ||
        r.method.toLowerCase().includes(q),
    );
    if (f.name.toLowerCase().includes(q) || subs.length || requests.length)
      return [{ ...f, folders: subs, requests }];
    return [];
  });
}

function RenameDialog({
  open,
  initialName,
  onOpenChange,
  onConfirm,
}: {
  open: boolean;
  initialName: string;
  onOpenChange: (open: boolean) => void;
  onConfirm: (name: string) => Promise<void>;
}) {
  const [name, setName] = React.useState("");
  const [loading, setLoading] = React.useState(false);

  React.useEffect(() => {
    if (open) setName(initialName);
  }, [open, initialName]);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim()) return;
    setLoading(true);
    try {
      await onConfirm(name.trim());
    } finally {
      setLoading(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Rename</DialogTitle>
        </DialogHeader>
        <form onSubmit={submit} className="flex flex-col gap-4">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="rename-name">Name</Label>
            <Input
              id="rename-name"
              value={name}
              onChange={(e) => setName(e.currentTarget.value)}
              autoFocus
            />
          </div>
          <DialogFooter>
            <Button type="submit" disabled={!name.trim() || loading}>
              Rename
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function buildTreeData(
  collections: TreeCollection[],
  onLoadRequest: (req: { method: string; url: string; name?: string; requestId?: string; params?: string | null }) => void,
  setSelected: (s: SelectedNode) => void,
): TreeDataItem[] {
  return collections.map((c) => ({
    id: `c:${c.id}`,
    name: c.name,
    icon: FolderOpen,
    onClick: () => setSelected({ kind: "collection", collectionId: c.id, name: c.name }),
    children: [
      ...c.folders.map((f) => folderToTreeItem(f, c.id, onLoadRequest, setSelected)),
      ...c.requests.map((r) => ({
        id: `r:${r.id}`,
        name: r.name,
        meta: { method: r.method, kind: "request", collectionId: String(c.id), folderId: "", requestId: String(r.id) },
        onClick: () => {
          setSelected({
            kind: "request",
            collectionId: c.id,
            folderId: null,
            requestId: r.id,
            name: r.name,
          });
          onLoadRequest({ method: r.method, url: r.url, name: r.name, requestId: `r:${r.id}`, params: r.params });
        },
      })),
    ],
  }));
}

function folderToTreeItem(
  f: TreeFolder,
  collectionId: number,
  onLoadRequest: (req: { method: string; url: string; name?: string; requestId?: string; params?: string | null }) => void,
  setSelected: (s: SelectedNode) => void,
): TreeDataItem {
  return {
    id: `f:${f.id}`,
    name: f.name,
    icon: FolderOpen,
    onClick: () =>
      setSelected({ kind: "folder", collectionId, folderId: f.id, name: f.name }),
    children: [
      ...f.folders.map((sub) =>
        folderToTreeItem(sub, collectionId, onLoadRequest, setSelected),
      ),
      ...f.requests.map((r) => ({
        id: `r:${r.id}`,
        name: r.name,
        meta: { method: r.method, kind: "request", collectionId: String(collectionId), folderId: String(f.id), requestId: String(r.id) },
        onClick: () => {
          setSelected({
            kind: "request",
            collectionId,
            folderId: f.id,
            requestId: r.id,
            name: r.name,
          });
          onLoadRequest({ method: r.method, url: r.url, name: r.name, requestId: `r:${r.id}`, params: r.params });
        },
      })),
    ],
  };
}
