import { execFile } from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";

interface FileEntry {
  status: string;
  path: string;
}

interface LayerStatus {
  index: number;
  name: string;
  tree: string;
  files: FileEntry[];
}

interface DraftsJson {
  base_head: string;
  head_tree: string;
  synced: boolean;
  sync_git_index: boolean;
  layers: LayerStatus[];
  wt: {
    modified: FileEntry[];
    untracked: string[];
  };
}

interface ResourceMeta {
  groupId: string;
  layerName?: string;
  relPath: string;
  status: string;
}

const SCHEME = "git-drafts";

function resolveGdExecutable(workspaceRoot: string): string {
  const configured = vscode.workspace
    .getConfiguration("gitDrafts")
    .get<string>("executablePath", "gd");
  if (configured && configured !== "gd") {
    return configured;
  }
  const ext = process.platform === "win32" ? ".exe" : "";
  const localRelease = path.join(workspaceRoot, "target", "release", `gd${ext}`);
  if (fs.existsSync(localRelease)) {
    return localRelease;
  }
  const cargoBin = path.join(os.homedir(), ".cargo", "bin", `gd${ext}`);
  if (fs.existsSync(cargoBin)) {
    return cargoBin;
  }
  return "gd";
}

function runGd(cwd: string, args: string[]): Promise<string> {
  const bin = resolveGdExecutable(cwd);
  return new Promise((resolve, reject) => {
    execFile(
      bin,
      args,
      { cwd, maxBuffer: 20 * 1024 * 1024, encoding: "utf8" },
      (err, stdout, stderr) => {
        if (err) {
          const msg = (stderr || stdout || err.message).trim();
          reject(new Error(msg));
        } else {
          resolve(stdout);
        }
      }
    );
  });
}

class DraftBlobProvider implements vscode.TextDocumentContentProvider {
  private readonly _onDidChange = new vscode.EventEmitter<vscode.Uri>();
  readonly onDidChange = this._onDidChange.event;

  constructor(private readonly cwd: string) {}

  fireAll(uris: vscode.Uri[]): void {
    for (const uri of uris) {
      this._onDidChange.fire(uri);
    }
  }

  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    // Format: git-drafts:/<spec>/<relPath>
    const raw = uri.path.replace(/^\/+/, "");
    const slash = raw.indexOf("/");
    if (slash < 0) {
      return "";
    }
    const spec = decodeURIComponent(raw.slice(0, slash));
    const relPath = raw.slice(slash + 1);
    try {
      return await runGd(this.cwd, ["blob", spec, relPath, "--allow-missing"]);
    } catch {
      return "";
    }
  }
}

class DraftsScmController implements vscode.Disposable {
  readonly scm: vscode.SourceControl;
  readonly blobProvider: DraftBlobProvider;
  private groups: vscode.SourceControlResourceGroup[] = [];
  private metaMap = new WeakMap<vscode.SourceControlResourceState, ResourceMeta>();
  private lastStatus: DraftsJson | undefined;
  private trackedUris: vscode.Uri[] = [];
  private refreshTimer: NodeJS.Timeout | undefined;
  private readonly disposables: vscode.Disposable[] = [];

  constructor(private readonly root: string) {
    this.blobProvider = new DraftBlobProvider(root);
    this.disposables.push(
      vscode.workspace.registerTextDocumentContentProvider(SCHEME, this.blobProvider)
    );

    this.scm = vscode.scm.createSourceControl(
      "git-drafts",
      "Git Drafts",
      vscode.Uri.file(root)
    );
    this.scm.inputBox.placeholder = "Commit message for [L1] ready (Ctrl+Enter to commit)";
    this.scm.acceptInputCommand = {
      command: "gitDrafts.commitReady",
      title: "Commit Ready Layer",
    };
    this.disposables.push(this.scm);

    const watcher = vscode.workspace.createFileSystemWatcher(
      new vscode.RelativePattern(root, "**/*")
    );
    const schedule = () => this.scheduleRefresh();
    this.disposables.push(
      watcher,
      watcher.onDidChange(schedule),
      watcher.onDidCreate(schedule),
      watcher.onDidDelete(schedule)
    );

    void this.refresh();
  }

  scheduleRefresh(): void {
    if (this.refreshTimer) {
      clearTimeout(this.refreshTimer);
    }
    this.refreshTimer = setTimeout(() => {
      void this.refresh();
    }, 180);
  }

  async refresh(): Promise<void> {
    try {
      const raw = await runGd(this.root, ["status", "--json"]);
      const data = JSON.parse(raw) as DraftsJson;
      this.lastStatus = data;
      this.rebuildGroups(data);
      this.blobProvider.fireAll(this.trackedUris);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      if (msg.includes("gd init")) {
        this.clearGroups();
      }
    }
  }

  private clearGroups(): void {
    for (const g of this.groups) {
      g.dispose();
    }
    this.groups = [];
    this.scm.count = 0;
  }

  private rebuildGroups(data: DraftsJson): void {
    this.clearGroups();
    this.trackedUris = [];
    let total = 0;

    for (let i = 0; i < data.layers.length; i++) {
      const layer = data.layers[i];
      const suffix = i === 0 ? " (commit target)" : "";
      const grp = this.scm.createResourceGroup(
        `layer:${i}`,
        `[L${layer.index}] ${layer.name}${suffix}`
      );
      grp.hideWhenEmpty = false;
      grp.resourceStates = layer.files.map((f) => {
        total++;
        const leftSpec = i === 0 ? "head" : data.layers[i - 1].name;
        const leftUri = this.makeUri(leftSpec, f.path);
        const rightUri = this.makeUri(layer.name, f.path);
        this.trackedUris.push(leftUri, rightUri);

        const state: vscode.SourceControlResourceState = {
          resourceUri: vscode.Uri.file(path.join(this.root, f.path)),
          decorations: {
            strikeThrough: f.status === "D",
            tooltip: `[L${layer.index}] ${layer.name}: ${f.status} ${f.path}`,
          },
          command: {
            command: "gitDrafts.openDiff",
            title: "Open Layer Diff",
            arguments: [leftUri, rightUri, `${f.path} (${leftSpec} ↔ ${layer.name})`],
          },
        };
        this.metaMap.set(state, {
          groupId: `layer:${i}`,
          layerName: layer.name,
          relPath: f.path,
          status: f.status,
        });
        return state;
      });
      this.groups.push(grp);
    }

    const topLayer = data.layers[data.layers.length - 1]?.name ?? "head";
    const wtGrp = this.scm.createResourceGroup("wt", "[WT] Working Tree");
    wtGrp.hideWhenEmpty = false;
    const wtStates: vscode.SourceControlResourceState[] = [];

    for (const f of data.wt.modified) {
      total++;
      const leftUri = this.makeUri(topLayer, f.path);
      const rightUri = vscode.Uri.file(path.join(this.root, f.path));
      this.trackedUris.push(leftUri);

      const state: vscode.SourceControlResourceState = {
        resourceUri: rightUri,
        decorations: {
          strikeThrough: f.status === "D",
          tooltip: `[WT] modified: ${f.path}`,
        },
        command: {
          command: "gitDrafts.openDiff",
          title: "Open Working Tree Diff",
          arguments: [leftUri, rightUri, `${f.path} (${topLayer} ↔ WT)`],
        },
      };
      this.metaMap.set(state, {
        groupId: "wt",
        relPath: f.path,
        status: f.status,
      });
      wtStates.push(state);
    }

    for (const relPath of data.wt.untracked) {
      total++;
      const fileUri = vscode.Uri.file(path.join(this.root, relPath));
      const state: vscode.SourceControlResourceState = {
        resourceUri: fileUri,
        decorations: {
          tooltip: `[WT] untracked: ${relPath}`,
        },
        command: {
          command: "vscode.open",
          title: "Open File",
          arguments: [fileUri],
        },
      };
      this.metaMap.set(state, {
        groupId: "wt",
        relPath,
        status: "?",
      });
      wtStates.push(state);
    }

    wtGrp.resourceStates = wtStates;
    this.groups.push(wtGrp);
    this.scm.count = total;
  }

  private makeUri(spec: string, relPath: string): vscode.Uri {
    return vscode.Uri.from({
      scheme: SCHEME,
      path: `/${encodeURIComponent(spec)}/${relPath.replace(/\\/g, "/")}`,
    });
  }

  resolveMeta(arg: unknown): ResourceMeta | undefined {
    if (arg && typeof arg === "object") {
      return this.metaMap.get(arg as vscode.SourceControlResourceState);
    }
    return undefined;
  }

  resolveLayerFromGroup(arg: unknown): string | undefined {
    if (arg && typeof arg === "object" && "id" in arg) {
      const id = (arg as vscode.SourceControlResourceGroup).id;
      if (id.startsWith("layer:")) {
        const idx = Number(id.slice("layer:".length));
        return this.lastStatus?.layers[idx]?.name;
      }
    }
    return undefined;
  }

  async execAndRefresh(args: string[]): Promise<void> {
    await runGd(this.root, args);
    await this.refresh();
  }

  getSyncState(): boolean {
    return this.lastStatus?.sync_git_index ?? false;
  }

  dispose(): void {
    if (this.refreshTimer) {
      clearTimeout(this.refreshTimer);
    }
    this.clearGroups();
    for (const d of this.disposables) {
      d.dispose();
    }
  }
}

export function activate(context: vscode.ExtensionContext): void {
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (!folder) {
    return;
  }
  const root = folder.uri.fsPath;
  const ctrl = new DraftsScmController(root);
  context.subscriptions.push(ctrl);

  const wrap = (fn: (...args: unknown[]) => Promise<void>) =>
    async (...args: unknown[]) => {
      try {
        await fn(...args);
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        void vscode.window.showErrorMessage(`Git Drafts: ${msg}`);
      }
    };

  context.subscriptions.push(
    vscode.commands.registerCommand(
      "gitDrafts.openDiff",
      async (left: vscode.Uri, right: vscode.Uri, title: string) => {
        await vscode.commands.executeCommand("vscode.diff", left, right, title);
      }
    ),
    vscode.commands.registerCommand(
      "gitDrafts.init",
      wrap(async () => {
        const layers = await vscode.window.showInputBox({
          prompt: "Draft layers (ordered commit-ready → raw, space-separated)",
          value: "ready reviewed draft",
        });
        if (layers === undefined) {
          return;
        }
        const names = layers.trim().split(/\s+/).filter(Boolean);
        await ctrl.execAndRefresh(["init", ...names]);
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.refresh",
      wrap(async () => ctrl.refresh())
    ),
    vscode.commands.registerCommand(
      "gitDrafts.stage",
      wrap(async (arg: unknown) => {
        const meta = ctrl.resolveMeta(arg);
        if (meta) {
          await ctrl.execAndRefresh(["stage", "--", meta.relPath]);
        } else {
          await ctrl.execAndRefresh(["stage", "-A"]);
        }
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.promote",
      wrap(async (arg: unknown) => {
        const meta = ctrl.resolveMeta(arg);
        if (meta?.layerName) {
          await ctrl.execAndRefresh(["promote", "--from", meta.layerName, "--", meta.relPath]);
          return;
        }
        const grpLayer = ctrl.resolveLayerFromGroup(arg);
        if (grpLayer) {
          await ctrl.execAndRefresh(["promote", "--from", grpLayer, "-A"]);
        }
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.demote",
      wrap(async (arg: unknown) => {
        const meta = ctrl.resolveMeta(arg);
        if (meta?.layerName) {
          await ctrl.execAndRefresh(["demote", "--from", meta.layerName, "--", meta.relPath]);
          return;
        }
        const grpLayer = ctrl.resolveLayerFromGroup(arg);
        if (grpLayer) {
          await ctrl.execAndRefresh(["demote", "--from", grpLayer, "-A"]);
        }
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.drop",
      wrap(async (arg: unknown) => {
        const meta = ctrl.resolveMeta(arg);
        if (meta?.layerName) {
          await ctrl.execAndRefresh(["drop", meta.layerName, "--", meta.relPath]);
        }
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.commitReady",
      wrap(async () => {
        let msg = ctrl.scm.inputBox.value.trim();
        if (!msg) {
          const input = await vscode.window.showInputBox({
            prompt: "Commit message for [L1] ready",
          });
          if (!input?.trim()) {
            return;
          }
          msg = input.trim();
        }
        await ctrl.execAndRefresh(["commit", "-m", msg]);
        ctrl.scm.inputBox.value = "";
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.commitFile",
      wrap(async (arg: unknown) => {
        const meta = ctrl.resolveMeta(arg);
        if (!meta) {
          return;
        }
        const msg = await vscode.window.showInputBox({
          prompt: `Commit '${meta.relPath}' from [L1] ready`,
        });
        if (!msg?.trim()) {
          return;
        }
        await ctrl.execAndRefresh(["commit", meta.relPath, "-m", msg.trim()]);
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.layerAdd",
      wrap(async () => {
        const name = await vscode.window.showInputBox({
          prompt: "New draft layer name (added at top)",
        });
        if (!name?.trim()) {
          return;
        }
        await ctrl.execAndRefresh(["layer", "add", name.trim()]);
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.undo",
      wrap(async () => ctrl.execAndRefresh(["undo"]))
    ),
    vscode.commands.registerCommand(
      "gitDrafts.toggleSyncGitIndex",
      wrap(async () => {
        const next = !ctrl.getSyncState();
        await ctrl.execAndRefresh(["config", "sync-git-index", String(next)]);
        void vscode.window.showInformationMessage(
          `Git Drafts: .git/index mirror ${next ? "enabled" : "disabled"}`
        );
      })
    )
  );
}

export function deactivate(): void {}
