import { execFile, spawn } from "child_process";
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

type TreeNode =
  | { kind: "layer"; layerIndex: number; layer: LayerStatus }
  | { kind: "wtGroup"; modified: FileEntry[]; untracked: string[] }
  | { kind: "file"; layerIndex?: number; layerName?: string; relPath: string; status: string }
  | { kind: "info"; label: string; command?: vscode.Command };

const SCHEME = "git-drafts";

let cachedGdBin: { root: string; bin: string } | undefined;
let extensionStorageDir: string | undefined;

function getManagedGdPath(): string | undefined {
  if (!extensionStorageDir) {
    return undefined;
  }
  const ext = process.platform === "win32" ? ".exe" : "";
  return path.join(extensionStorageDir, "bin", `gd${ext}`);
}

function getReleaseAssetName(): string | undefined {
  const key = `${process.platform}:${process.arch}`;
  switch (key) {
    case "linux:x64":
      return "git-drafts-x86_64-unknown-linux-musl.tar.gz";
    case "darwin:arm64":
      return "git-drafts-aarch64-apple-darwin.tar.gz";
    case "darwin:x64":
      return "git-drafts-x86_64-apple-darwin.tar.gz";
    case "win32:x64":
      return "git-drafts-x86_64-pc-windows-msvc.zip";
    default:
      return undefined;
  }
}

async function downloadPrebuiltGd(): Promise<string> {
  if (!extensionStorageDir) {
    throw new Error("Extension storage directory unavailable");
  }
  const asset = getReleaseAssetName();
  if (!asset) {
    throw new Error(
      `No prebuilt binary for ${process.platform}/${process.arch}; install via cargo`
    );
  }
  const binDir = path.join(extensionStorageDir, "bin");
  fs.mkdirSync(binDir, { recursive: true });
  const archivePath = path.join(binDir, asset);
  const url = `https://github.com/Girgis98/git-drafts/releases/latest/download/${asset}`;

  await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: "Git Drafts: Installing gd CLI...",
      cancellable: false,
    },
    async () => {
      const res = await fetch(url);
      if (!res.ok) {
        throw new Error(`Download failed (${res.status} ${res.statusText})`);
      }
      const bytes = Buffer.from(await res.arrayBuffer());
      fs.writeFileSync(archivePath, bytes);

      await new Promise<void>((resolve, reject) => {
        const args = asset.endsWith(".zip")
          ? ["-xf", archivePath, "-C", binDir]
          : ["-xzf", archivePath, "-C", binDir];
        execFile("tar", args, (err, _out, stderr) => {
          if (err) {
            reject(new Error((stderr || err.message).trim()));
          } else {
            resolve();
          }
        });
      });
      try {
        fs.unlinkSync(archivePath);
      } catch {
        // ignore cleanup failure
      }
      const binPath = getManagedGdPath();
      if (!binPath || !fs.existsSync(binPath)) {
        throw new Error("Extracted archive did not contain gd binary");
      }
      if (process.platform !== "win32") {
        fs.chmodSync(binPath, 0o755);
      }
    }
  );

  cachedGdBin = undefined;
  return getManagedGdPath()!;
}

function resolveGdExecutable(workspaceRoot: string): string {
  const configured = vscode.workspace
    .getConfiguration("gitDrafts")
    .get<string>("executablePath", "gd");
  if (configured && configured !== "gd") {
    return configured;
  }
  if (cachedGdBin && cachedGdBin.root === workspaceRoot) {
    return cachedGdBin.bin;
  }
  const ext = process.platform === "win32" ? ".exe" : "";
  const localRelease = path.join(workspaceRoot, "target", "release", `gd${ext}`);
  const managed = getManagedGdPath();
  const cargoBin = path.join(os.homedir(), ".cargo", "bin", `gd${ext}`);
  let resolved = "gd";
  if (fs.existsSync(localRelease)) {
    resolved = localRelease;
  } else if (managed && fs.existsSync(managed)) {
    resolved = managed;
  } else if (fs.existsSync(cargoBin)) {
    resolved = cargoBin;
  }
  cachedGdBin = { root: workspaceRoot, bin: resolved };
  return resolved;
}

function runGd(cwd: string, args: string[], stdinText?: string): Promise<string> {
  const bin = resolveGdExecutable(cwd);
  if (stdinText !== undefined) {
    return new Promise((resolve, reject) => {
      const child = spawn(bin, args, { cwd });
      let out = "";
      let err = "";
      child.stdout.on("data", (d) => (out += d.toString()));
      child.stderr.on("data", (d) => (err += d.toString()));
      child.on("error", (e) => reject(e));
      child.on("close", (code) => {
        if (code === 0) {
          resolve(out);
        } else {
          reject(new Error((err || out || `gd exited ${code}`).trim()));
        }
      });
      child.stdin.write(stdinText);
      child.stdin.end();
    });
  }

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

function makeLayerUri(spec: string, relPath: string): vscode.Uri {
  return vscode.Uri.from({
    scheme: SCHEME,
    path: `/${encodeURIComponent(spec)}/${relPath.replace(/\\/g, "/")}`,
  });
}

function parseLayerUri(uri: vscode.Uri): { spec: string; relPath: string } | undefined {
  if (uri.scheme !== SCHEME) {
    return undefined;
  }
  const raw = uri.path.replace(/^\/+/, "");
  const slash = raw.indexOf("/");
  if (slash < 0) {
    return undefined;
  }
  return {
    spec: decodeURIComponent(raw.slice(0, slash)),
    relPath: raw.slice(slash + 1),
  };
}

function extractHunkAtLine(
  diffText: string,
  targetLine1Based: number,
  matchNewSide: boolean
): string | undefined {
  const lines = diffText.split(/\r?\n/);
  const hunkStarts: number[] = [];
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].startsWith("@@ ")) {
      hunkStarts.push(i);
    }
  }
  if (hunkStarts.length === 0) {
    return undefined;
  }
  const header = lines.slice(0, hunkStarts[0]).join("\n");
  const hunks: { text: string; start: number; end: number }[] = [];
  for (let h = 0; h < hunkStarts.length; h++) {
    const startIdx = hunkStarts[h];
    const endIdx = h + 1 < hunkStarts.length ? hunkStarts[h + 1] : lines.length;
    const m = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(lines[startIdx]);
    if (!m) {
      continue;
    }
    const oldStart = Number(m[1]);
    const oldCount = m[2] !== undefined ? Number(m[2]) : 1;
    const newStart = Number(m[3]);
    const newCount = m[4] !== undefined ? Number(m[4]) : 1;
    const s = matchNewSide ? newStart : oldStart;
    const c = Math.max(1, matchNewSide ? newCount : oldCount);
    hunks.push({
      text: lines.slice(startIdx, endIdx).join("\n"),
      start: s,
      end: s + c - 1,
    });
  }
  if (hunks.length === 1) {
    return `${header}\n${hunks[0].text}\n`;
  }
  const hit = hunks.find((hk) => targetLine1Based >= hk.start && targetLine1Based <= hk.end);
  return hit ? `${header}\n${hit.text}\n` : undefined;
}

class DraftBlobProvider implements vscode.TextDocumentContentProvider {
  private readonly _onDidChange = new vscode.EventEmitter<vscode.Uri>();
  readonly onDidChange = this._onDidChange.event;

  constructor(private cwd: string | undefined) {}

  setCwd(cwd: string | undefined): void {
    this.cwd = cwd;
  }

  fireAll(uris: vscode.Uri[]): void {
    for (const uri of uris) {
      this._onDidChange.fire(uri);
    }
  }

  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    const parsed = parseLayerUri(uri);
    if (!parsed || !this.cwd) {
      return "";
    }
    try {
      return await runGd(this.cwd, ["blob", parsed.spec, parsed.relPath, "--allow-missing"]);
    } catch {
      return "";
    }
  }
}

class DraftsTreeProvider implements vscode.TreeDataProvider<TreeNode> {
  private readonly _onDidChangeTreeData = new vscode.EventEmitter<TreeNode | undefined>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;
  private status: DraftsJson | undefined;
  private initRequired = false;
  private errorMsg: string | undefined;

  constructor(private root: string | undefined) {}

  setRoot(root: string | undefined): void {
    this.root = root;
  }

  update(status: DraftsJson | undefined, initRequired: boolean, errorMsg?: string, noWorkspace?: boolean): void {
    this.status = status;
    this.initRequired = initRequired;
    this.errorMsg = errorMsg;
    this.noWorkspace = Boolean(noWorkspace);
    this._onDidChangeTreeData.fire(undefined);
  }

  private noWorkspace = false;

  getTreeItem(node: TreeNode): vscode.TreeItem {
    if (node.kind === "info") {
      const item = new vscode.TreeItem(node.label, vscode.TreeItemCollapsibleState.None);
      item.iconPath = new vscode.ThemeIcon("layers");
      item.command = node.command;
      return item;
    }
    if (node.kind === "layer") {
      const count = node.layer.files.length;
      const isReady = node.layerIndex === 0;
      const item = new vscode.TreeItem(
        `[L${node.layer.index}] ${node.layer.name}`,
        count > 0
          ? vscode.TreeItemCollapsibleState.Expanded
          : vscode.TreeItemCollapsibleState.Collapsed
      );
      item.description = `${count} file${count === 1 ? "" : "s"}${isReady ? " · commit target" : ""}`;
      item.contextValue = isReady ? "readyLayer" : "upperLayer";
      item.iconPath = new vscode.ThemeIcon(
        isReady ? "check-all" : "layers",
        isReady ? new vscode.ThemeColor("charts.green") : new vscode.ThemeColor("charts.blue")
      );
      return item;
    }
    if (node.kind === "wtGroup") {
      const total = node.modified.length + node.untracked.length;
      const item = new vscode.TreeItem(
        "[WT] Working Tree",
        total > 0
          ? vscode.TreeItemCollapsibleState.Expanded
          : vscode.TreeItemCollapsibleState.Collapsed
      );
      item.description = `${node.modified.length} modified · ${node.untracked.length} untracked`;
      item.contextValue = "wtGroup";
      item.iconPath = new vscode.ThemeIcon("edit", new vscode.ThemeColor("charts.orange"));
      return item;
    }

    const fileUri = vscode.Uri.file(path.join(this.root ?? "", node.relPath));
    const item = new vscode.TreeItem(
      path.basename(node.relPath),
      vscode.TreeItemCollapsibleState.None
    );
    const dir = path.dirname(node.relPath);
    item.description = dir === "." ? node.status : `${node.status} · ${dir}`;
    item.tooltip = `${node.relPath} (${node.status})`;
    item.resourceUri = fileUri;

    if (node.layerName === undefined) {
      item.contextValue = "wtFile";
      if (node.status === "?") {
        item.command = { command: "vscode.open", title: "Open File", arguments: [fileUri] };
      } else {
        const topLayer =
          this.status?.layers[this.status.layers.length - 1]?.name ?? "head";
        item.command = {
          command: "gitDrafts.openDiff",
          title: "Open Diff",
          arguments: [
            makeLayerUri(topLayer, node.relPath),
            fileUri,
            `${node.relPath} (${topLayer} ↔ WT)`,
          ],
        };
      }
    } else {
      const isReady = node.layerIndex === 0;
      item.contextValue = isReady ? "readyFile" : "upperFile";
      const leftSpec =
        node.layerIndex && node.layerIndex > 0 && this.status
          ? this.status.layers[node.layerIndex - 1].name
          : "head";
      item.command = {
        command: "gitDrafts.openDiff",
        title: "Open Diff",
        arguments: [
          makeLayerUri(leftSpec, node.relPath),
          makeLayerUri(node.layerName, node.relPath),
          `${node.relPath} (${leftSpec} ↔ ${node.layerName})`,
        ],
      };
    }
    return item;
  }

  getChildren(element?: TreeNode): TreeNode[] {
    if (this.noWorkspace) {
      return [
        {
          kind: "info",
          label: "Open a Git repository folder...",
          command: { command: "workbench.action.files.openFolder", title: "Open Folder" },
        },
      ];
    }
    if (this.initRequired) {
      return [
        {
          kind: "info",
          label: "Initialize Git Drafts (gd init)...",
          command: { command: "gitDrafts.init", title: "Initialize Git Drafts" },
        },
      ];
    }
    if (this.errorMsg) {
      return [{ kind: "info", label: this.errorMsg }];
    }
    if (!this.status) {
      return [];
    }
    if (!element) {
      const nodes: TreeNode[] = this.status.layers.map((layer, idx) => ({
        kind: "layer",
        layerIndex: idx,
        layer,
      }));
      nodes.push({
        kind: "wtGroup",
        modified: this.status.wt.modified,
        untracked: this.status.wt.untracked,
      });
      return nodes;
    }
    if (element.kind === "layer") {
      return element.layer.files.map((f) => ({
        kind: "file",
        layerIndex: element.layerIndex,
        layerName: element.layer.name,
        relPath: f.path,
        status: f.status,
      }));
    }
    if (element.kind === "wtGroup") {
      const out: TreeNode[] = element.modified.map((f) => ({
        kind: "file",
        relPath: f.path,
        status: f.status,
      }));
      for (const u of element.untracked) {
        out.push({ kind: "file", relPath: u, status: "?" });
      }
      return out;
    }
    return [];
  }
}

class DraftsPipelineViewProvider implements vscode.WebviewViewProvider {
  private view: vscode.WebviewView | undefined;
  private status: DraftsJson | undefined;
  private initRequired = false;
  private noWorkspace = false;
  private errorMsg: string | undefined;

  resolveWebviewView(webviewView: vscode.WebviewView): void {
    this.view = webviewView;
    webviewView.webview.options = { enableScripts: true };
    webviewView.webview.onDidReceiveMessage((msg: { type: string; arg?: string }) => {
      switch (msg.type) {
        case "openFolder":
          void vscode.commands.executeCommand("workbench.action.files.openFolder");
          break;
        case "installCli":
          void vscode.commands.executeCommand("gitDrafts.installCli");
          break;
        case "downloadCli":
          void vscode.commands.executeCommand("gitDrafts.downloadCli");
          break;
        case "init":
          void vscode.commands.executeCommand("gitDrafts.init", msg.arg);
          break;
        case "commitReady":
          void vscode.commands.executeCommand("gitDrafts.commitReady");
          break;
        case "stageAll":
          void vscode.commands.executeCommand("gitDrafts.stage");
          break;
        case "promoteLayer":
          void vscode.commands.executeCommand("gitDrafts.promote", msg.arg);
          break;
        case "demoteLayer":
          void vscode.commands.executeCommand("gitDrafts.demote", msg.arg);
          break;
        case "layerAdd":
          void vscode.commands.executeCommand("gitDrafts.layerAdd");
          break;
        case "undo":
          void vscode.commands.executeCommand("gitDrafts.undo");
          break;
        case "toggleSync":
          void vscode.commands.executeCommand("gitDrafts.toggleSyncGitIndex");
          break;
        case "refresh":
          void vscode.commands.executeCommand("gitDrafts.refresh");
          break;
      }
    });
    this.render();
  }

  update(status: DraftsJson | undefined, initRequired: boolean, errorMsg?: string, noWorkspace?: boolean): void {
    this.status = status;
    this.initRequired = initRequired;
    this.errorMsg = errorMsg;
    this.noWorkspace = Boolean(noWorkspace);
    this.render();
  }

  private render(): void {
    if (!this.view) {
      return;
    }
    const esc = (s: string) =>
      s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

    let body = "";
    if (this.noWorkspace) {
      body = `
        <div class="card">
          <div class="title">No Folder Open</div>
          <p class="muted">Open a Git repository folder to stage, review, and promote layered AI-agent drafts.</p>
          <button class="btn primary full" onclick="send('openFolder')">Open Folder...</button>
        </div>`;
    } else if (this.initRequired) {
      body = `
        <div class="card">
          <div class="title">Not Initialized</div>
          <p class="muted">Create layered staging phases on top of HEAD to review AI edits safely before committing.</p>
          <input id="initInput" class="input" value="ready reviewed draft" />
          <button class="btn primary full" onclick="send('init', document.getElementById('initInput').value)">Initialize Draft Layers</button>
        </div>`;
    } else if (this.errorMsg) {
      const missingCli =
        this.errorMsg.includes("ENOENT") || this.errorMsg.includes("not recognized");
      if (missingCli) {
        body = `
          <div class="card">
            <div class="title">gd CLI Not Installed</div>
            <p class="muted">Download the prebuilt binary automatically, or install via Cargo:</p>
            <button class="btn primary full" onclick="send('downloadCli')">Download Prebuilt gd Binary</button>
            <button class="btn full" style="margin-top:4px;" onclick="send('installCli')">Install via Cargo in Terminal</button>
            <button class="btn full" style="margin-top:4px;" onclick="send('refresh')">Refresh</button>
          </div>`;
      } else {
        body = `<div class="card"><div class="title">Unavailable</div><p class="muted">${esc(this.errorMsg)}</p></div>`;
      }
    } else if (this.status) {
      const s = this.status;
      const wtCount = s.wt.modified.length + s.wt.untracked.length;
      const layerCards = s.layers
        .map((l, idx) => {
          const isReady = idx === 0;
          const badgeCls = isReady ? "badge ready" : "badge";
          const actions = isReady
            ? `<button class="btn small primary" onclick="send('commitReady')" ${l.files.length === 0 ? "disabled" : ""}>Commit</button>
               <button class="btn small" title="Demote to upper layer" onclick="send('demoteLayer', '${esc(l.name)}')" ${l.files.length === 0 ? "disabled" : ""}>↓</button>`
            : `<button class="btn small" title="Promote toward ready" onclick="send('promoteLayer', '${esc(l.name)}')" ${l.files.length === 0 ? "disabled" : ""}>↑</button>
               <button class="btn small" title="Demote toward WT" onclick="send('demoteLayer', '${esc(l.name)}')" ${l.files.length === 0 ? "disabled" : ""}>↓</button>`;
          return `
            <div class="layer ${isReady ? "is-ready" : ""}">
              <div class="row">
                <div>
                  <span class="tag">L${l.index}</span>
                  <strong>${esc(l.name)}</strong>
                  ${isReady ? '<span class="pill">commit target</span>' : ""}
                </div>
                <span class="${badgeCls}">${l.files.length}</span>
              </div>
              <div class="actions">${actions}</div>
            </div>`;
        })
        .join("");

      body = `
        <div class="toolbar">
          <button class="btn small" onclick="send('layerAdd')">+ Layer</button>
          <button class="btn small" onclick="send('undo')">Undo</button>
          <button class="btn small ${s.sync_git_index ? "active" : ""}" onclick="send('toggleSync')" title="Mirror [L1] ready to .git/index">
            Mirror: ${s.sync_git_index ? "ON" : "OFF"}
          </button>
          <button class="btn small" onclick="send('refresh')">↻</button>
        </div>
        <div class="head-bar">HEAD <code>${esc(s.base_head.slice(0, 7))}</code></div>
        ${layerCards}
        <div class="layer is-wt">
          <div class="row">
            <div><span class="tag">WT</span> <strong>working-tree</strong></div>
            <span class="badge">${wtCount}</span>
          </div>
          <div class="actions">
            <button class="btn small" onclick="send('stageAll')" ${wtCount === 0 ? "disabled" : ""}>Stage All ↑</button>
          </div>
        </div>`;
    }

    this.view.webview.html = `<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8" />
<style>
  body { font-family: var(--vscode-font-family); color: var(--vscode-foreground); padding: 8px; margin: 0; font-size: 12px; }
  .toolbar { display: flex; gap: 4px; flex-wrap: wrap; margin-bottom: 8px; }
  .head-bar { font-size: 11px; color: var(--vscode-descriptionForeground); margin-bottom: 6px; padding-left: 2px; }
  .card, .layer { background: var(--vscode-sideBarSectionHeader-background, rgba(127,127,127,0.08)); border: 1px solid var(--vscode-panel-border, rgba(127,127,127,0.2)); border-radius: 6px; padding: 8px; margin-bottom: 6px; }
  .layer.is-ready { border-left: 3px solid var(--vscode-testing-iconPassed, #22c55e); }
  .layer.is-wt { border-left: 3px solid var(--vscode-editorWarning-foreground, #f59e0b); }
  .row { display: flex; justify-content: space-between; align-items: center; gap: 6px; }
  .actions { display: flex; gap: 4px; margin-top: 6px; justify-content: flex-end; }
  .tag { font-size: 10px; opacity: 0.75; margin-right: 4px; font-weight: 600; }
  .pill { font-size: 10px; padding: 1px 5px; border-radius: 99px; background: rgba(34,197,94,0.18); color: var(--vscode-testing-iconPassed, #22c55e); margin-left: 4px; }
  .badge { font-size: 11px; font-weight: 600; padding: 1px 7px; border-radius: 99px; background: var(--vscode-badge-background); color: var(--vscode-badge-foreground); }
  .badge.ready { background: var(--vscode-testing-iconPassed, #22c55e); color: #0b1410; }
  .btn { background: var(--vscode-button-secondaryBackground); color: var(--vscode-button-secondaryForeground); border: none; border-radius: 4px; padding: 4px 8px; cursor: pointer; font-size: 11px; }
  .btn:hover:not(:disabled) { background: var(--vscode-button-secondaryHoverBackground); }
  .btn.primary { background: var(--vscode-button-background); color: var(--vscode-button-foreground); }
  .btn.primary:hover:not(:disabled) { background: var(--vscode-button-hoverBackground); }
  .btn.active { outline: 1px solid var(--vscode-focusBorder); }
  .btn.small { padding: 3px 7px; }
  .btn.full { width: 100%; margin-top: 8px; padding: 6px; }
  .btn:disabled { opacity: 0.4; cursor: default; }
  .input { width: 100%; box-sizing: border-box; background: var(--vscode-input-background); color: var(--vscode-input-foreground); border: 1px solid var(--vscode-input-border, transparent); border-radius: 4px; padding: 5px 6px; margin-top: 6px; }
  .title { font-weight: 600; margin-bottom: 4px; }
  .muted { color: var(--vscode-descriptionForeground); margin: 4px 0; }
</style>
</head>
<body>
${body}
<script>
const vscode = acquireVsCodeApi();
function send(type, arg) { vscode.postMessage({ type, arg }); }
</script>
</body>
</html>`;
  }
}

class DraftsController implements vscode.Disposable {
  private scm: vscode.SourceControl | undefined;
  readonly blobProvider: DraftBlobProvider;
  readonly treeProvider: DraftsTreeProvider;
  readonly pipelineProvider: DraftsPipelineViewProvider;
  readonly statusBar: vscode.StatusBarItem;

  private root: string | undefined;
  private groups: vscode.SourceControlResourceGroup[] = [];
  private metaMap = new WeakMap<vscode.SourceControlResourceState, ResourceMeta>();
  private lastStatus: DraftsJson | undefined;
  private lastRawJson: string | undefined;
  private autoDownloadAttempted = false;
  private refreshInFlight = false;
  private pendingRefresh = false;
  private refreshTimer: NodeJS.Timeout | undefined;
  private watcherDisposables: vscode.Disposable[] = [];
  private readonly disposables: vscode.Disposable[] = [];

  constructor(initialRoot: string | undefined) {
    this.root = initialRoot;
    this.blobProvider = new DraftBlobProvider(initialRoot);
    this.treeProvider = new DraftsTreeProvider(initialRoot);
    this.pipelineProvider = new DraftsPipelineViewProvider();

    this.disposables.push(
      vscode.workspace.registerTextDocumentContentProvider(SCHEME, this.blobProvider),
      vscode.window.createTreeView("gitDrafts.layersView", {
        treeDataProvider: this.treeProvider,
        canSelectMany: true,
      }),
      vscode.window.createTreeView("gitDrafts.scmLayers", {
        treeDataProvider: this.treeProvider,
        canSelectMany: true,
      }),
      vscode.window.registerWebviewViewProvider("gitDrafts.pipeline", this.pipelineProvider)
    );

    this.statusBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 95);
    this.statusBar.command = "gitDrafts.focus";
    this.statusBar.show();
    this.disposables.push(this.statusBar);

    this.setupWorkspace(initialRoot);
    this.hookBuiltinGitApi();
    this.disposables.push(
      vscode.workspace.onDidChangeWorkspaceFolders(() => {
        const nextRoot = this.detectRepoRoot();
        this.setupWorkspace(nextRoot);
      }),
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration("gitDrafts.executablePath")) {
          cachedGdBin = undefined;
          this.lastRawJson = undefined;
          void this.refresh();
        }
      })
    );
  }

  private detectRepoRoot(): string | undefined {
    try {
      const gitExt = vscode.extensions.getExtension<{ getAPI(v: number): { repositories: { rootUri: vscode.Uri }[] } }>("vscode.git");
      const gitRoot = gitExt?.isActive ? gitExt.exports.getAPI(1).repositories[0]?.rootUri.fsPath : undefined;
      if (gitRoot) {
        return gitRoot;
      }
    } catch {
      // ignore
    }
    return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  }

  private hookBuiltinGitApi(): void {
    const gitExt = vscode.extensions.getExtension<{
      getAPI(v: number): {
        repositories: { rootUri: vscode.Uri; state: { onDidChange: vscode.Event<void> } }[];
        onDidOpenRepository: vscode.Event<{ rootUri: vscode.Uri; state: { onDidChange: vscode.Event<void> } }>;
      };
    }>("vscode.git");
    if (!gitExt) {
      return;
    }
    const attach = async () => {
      try {
        const exports = gitExt.isActive ? gitExt.exports : await gitExt.activate();
        const api = exports.getAPI(1);
        const bindRepo = (repo: { rootUri: vscode.Uri; state: { onDidChange: vscode.Event<void> } }) => {
          if (!this.root || this.root !== repo.rootUri.fsPath) {
            this.setupWorkspace(repo.rootUri.fsPath);
          }
          this.disposables.push(repo.state.onDidChange(() => this.scheduleRefresh()));
        };
        if (api.repositories[0]) {
          bindRepo(api.repositories[0]);
        }
        this.disposables.push(api.onDidOpenRepository(bindRepo));
      } catch {
        // ignore if vscode.git unavailable
      }
    };
    void attach();
  }

  private setupWorkspace(nextRoot: string | undefined): void {
    this.root = nextRoot;
    this.blobProvider.setCwd(nextRoot);
    this.treeProvider.setRoot(nextRoot);

    for (const d of this.watcherDisposables) {
      d.dispose();
    }
    this.watcherDisposables = [];
    this.clearGroups();
    if (this.scm) {
      this.scm.dispose();
      this.scm = undefined;
    }

    if (nextRoot) {
      const scm = vscode.scm.createSourceControl(
        "git-drafts",
        "Git Drafts",
        vscode.Uri.file(nextRoot)
      );
      scm.inputBox.placeholder = "Commit message for [L1] ready (Ctrl+Enter to commit)";
      scm.acceptInputCommand = {
        command: "gitDrafts.commitReady",
        title: "Commit Ready Layer",
      };
      this.scm = scm;

      const gitWatcher = vscode.workspace.createFileSystemWatcher(
        new vscode.RelativePattern(path.join(nextRoot, ".git"), "{index,HEAD,drafts/**}")
      );
      const schedule = () => this.scheduleRefresh();
      this.watcherDisposables.push(
        vscode.workspace.onDidSaveTextDocument(schedule),
        gitWatcher,
        gitWatcher.onDidChange(schedule),
        gitWatcher.onDidCreate(schedule),
        gitWatcher.onDidDelete(schedule)
      );
    }

    this.lastRawJson = undefined;
    void this.refresh();
  }

  scheduleRefresh(): void {
    if (this.refreshTimer) {
      clearTimeout(this.refreshTimer);
    }
    this.refreshTimer = setTimeout(() => {
      void this.refresh();
    }, 350);
  }

  async refresh(): Promise<void> {
    if (this.refreshInFlight) {
      this.pendingRefresh = true;
      return;
    }
    this.refreshInFlight = true;
    try {
      await this.doRefresh();
    } finally {
      this.refreshInFlight = false;
      if (this.pendingRefresh) {
        this.pendingRefresh = false;
        this.scheduleRefresh();
      }
    }
  }

  private async doRefresh(): Promise<void> {
    if (!this.root) {
      this.lastStatus = undefined;
      this.lastRawJson = undefined;
      this.clearGroups();
      this.treeProvider.update(undefined, false, undefined, true);
      this.pipelineProvider.update(undefined, false, undefined, true);
      this.statusBar.text = "$(layers) Drafts: no folder";
      this.statusBar.tooltip = "Open a Git repository folder to use Git Drafts";
      return;
    }
    try {
      const raw = await runGd(this.root, ["status", "--json"]);
      if (raw === this.lastRawJson) {
        return;
      }
      this.lastRawJson = raw;
      const data = JSON.parse(raw) as DraftsJson;
      this.lastStatus = data;
      this.rebuildGroups(data);
      this.treeProvider.update(data, false);
      this.pipelineProvider.update(data, false);

      const openLayerUris = vscode.workspace.textDocuments
        .filter((d) => d.uri.scheme === SCHEME)
        .map((d) => d.uri);
      this.blobProvider.fireAll(openLayerUris);

      const summary = data.layers.map((l) => `${l.name}(${l.files.length})`).join(" · ");
      this.statusBar.text = `$(layers) Drafts: ${summary}`;
      this.statusBar.tooltip = `Git Drafts (${data.base_head.slice(0, 7)}) — click to open`;
    } catch (err) {
      this.lastRawJson = undefined;
      const msg = err instanceof Error ? err.message : String(err);
      this.lastStatus = undefined;
      if (msg.includes("gd init")) {
        this.showUninitializedScm();
        this.treeProvider.update(undefined, true);
        this.pipelineProvider.update(undefined, true);
        this.statusBar.text = "$(layers) Drafts: uninitialized";
        this.statusBar.tooltip = "Click to initialize Git Drafts";
      } else if (
        !this.autoDownloadAttempted &&
        (msg.includes("ENOENT") || msg.includes("not recognized"))
      ) {
        this.autoDownloadAttempted = true;
        try {
          await downloadPrebuiltGd();
          await this.doRefresh();
          return;
        } catch (dlErr) {
          const dlMsg = dlErr instanceof Error ? dlErr.message : String(dlErr);
          this.clearGroups();
          this.treeProvider.update(undefined, false, dlMsg);
          this.pipelineProvider.update(undefined, false, msg);
          this.statusBar.text = "$(layers) Drafts: CLI missing";
          this.statusBar.tooltip = dlMsg;
        }
      } else {
        this.clearGroups();
        this.treeProvider.update(undefined, false, msg);
        this.pipelineProvider.update(undefined, false, msg);
        this.statusBar.text = "$(layers) Drafts";
        this.statusBar.tooltip = msg;
      }
    }
  }

  private clearGroups(): void {
    for (const g of this.groups) {
      g.dispose();
    }
    this.groups = [];
    if (this.scm) {
      this.scm.count = 0;
    }
  }

  private showUninitializedScm(): void {
    this.clearGroups();
    if (!this.scm || !this.root) {
      return;
    }
    const grp = this.scm.createResourceGroup("uninit", "Not Initialized");
    grp.hideWhenEmpty = false;
    grp.resourceStates = [
      {
        resourceUri: vscode.Uri.file(path.join(this.root, ".git")),
        decorations: { tooltip: "Click to run `gd init`" },
        command: { command: "gitDrafts.init", title: "Initialize Git Drafts" },
      },
    ];
    this.groups.push(grp);
  }

  private rebuildGroups(data: DraftsJson): void {
    this.clearGroups();
    if (!this.scm || !this.root) {
      return;
    }
    const rootDir = this.root;
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
        const leftUri = makeLayerUri(leftSpec, f.path);
        const rightUri = makeLayerUri(layer.name, f.path);

        const state: vscode.SourceControlResourceState = {
          resourceUri: vscode.Uri.file(path.join(rootDir, f.path)),
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
      const leftUri = makeLayerUri(topLayer, f.path);
      const rightUri = vscode.Uri.file(path.join(rootDir, f.path));

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
      const fileUri = vscode.Uri.file(path.join(rootDir, relPath));
      const state: vscode.SourceControlResourceState = {
        resourceUri: fileUri,
        decorations: { tooltip: `[WT] untracked: ${relPath}` },
        command: { command: "vscode.open", title: "Open File", arguments: [fileUri] },
      };
      this.metaMap.set(state, { groupId: "wt", relPath, status: "?" });
      wtStates.push(state);
    }

    wtGrp.resourceStates = wtStates;
    this.groups.push(wtGrp);
    this.scm.count = total;
  }

  resolveActionTarget(arg: unknown): { layerName?: string; relPath?: string } {
    if (typeof arg === "string") {
      return { layerName: arg };
    }
    if (arg && typeof arg === "object") {
      const meta = this.metaMap.get(arg as vscode.SourceControlResourceState);
      if (meta) {
        return { layerName: meta.layerName, relPath: meta.relPath };
      }
      if ("id" in arg) {
        const id = (arg as vscode.SourceControlResourceGroup).id;
        if (id.startsWith("layer:")) {
          const idx = Number(id.slice("layer:".length));
          return { layerName: this.lastStatus?.layers[idx]?.name };
        }
      }
      if ("kind" in arg) {
        const node = arg as TreeNode;
        if (node.kind === "layer") {
          return { layerName: node.layer.name };
        }
        if (node.kind === "file") {
          return { layerName: node.layerName, relPath: node.relPath };
        }
      }
    }
    return {};
  }

  resolveActionTargets(args: unknown[]): {
    byLayer: Map<string, string[]>;
    wtPaths: string[];
    wholeLayers: string[];
    wtAll: boolean;
  } {
    const flat: unknown[] = [];
    for (const a of args) {
      if (Array.isArray(a)) {
        flat.push(...a);
      } else if (a !== undefined) {
        flat.push(a);
      }
    }
    const byLayer = new Map<string, string[]>();
    const wtPaths: string[] = [];
    const wholeLayers: string[] = [];
    let wtAll = false;

    for (const item of flat) {
      if (item && typeof item === "object" && "kind" in item && (item as TreeNode).kind === "wtGroup") {
        wtAll = true;
        continue;
      }
      if (item && typeof item === "object" && "id" in item && (item as vscode.SourceControlResourceGroup).id === "wt") {
        wtAll = true;
        continue;
      }
      const t = this.resolveActionTarget(item);
      if (t.layerName && t.relPath) {
        const list = byLayer.get(t.layerName) ?? [];
        if (!list.includes(t.relPath)) {
          list.push(t.relPath);
          byLayer.set(t.layerName, list);
        }
      } else if (t.layerName) {
        if (!wholeLayers.includes(t.layerName)) {
          wholeLayers.push(t.layerName);
        }
      } else if (t.relPath) {
        if (!wtPaths.includes(t.relPath)) {
          wtPaths.push(t.relPath);
        }
      }
    }
    return { byLayer, wtPaths, wholeLayers, wtAll };
  }

  getRoot(): string | undefined {
    return this.root;
  }

  getScmInput(): vscode.SourceControlInputBox | undefined {
    return this.scm?.inputBox;
  }

  getStatus(): DraftsJson | undefined {
    return this.lastStatus;
  }

  async refreshAndSyncGit(): Promise<void> {
    await this.refresh();
    if (this.lastStatus?.sync_git_index) {
      void vscode.commands.executeCommand("git.refresh");
    }
  }

  async execAndRefresh(args: string[], stdinText?: string): Promise<void> {
    if (!this.root) {
      throw new Error("Open a Git repository folder first.");
    }
    await runGd(this.root, args, stdinText);
    await this.refreshAndSyncGit();
  }

  dispose(): void {
    if (this.refreshTimer) {
      clearTimeout(this.refreshTimer);
    }
    for (const d of this.watcherDisposables) {
      d.dispose();
    }
    this.clearGroups();
    if (this.scm) {
      this.scm.dispose();
    }
    for (const d of this.disposables) {
      d.dispose();
    }
  }
}

function extractGitResourcePaths(rootDir: string, args: unknown[]): string[] {
  const flat: unknown[] = [];
  for (const a of args) {
    if (Array.isArray(a)) {
      flat.push(...a);
    } else if (a !== undefined) {
      flat.push(a);
    }
  }
  const seen = new Set<string>();
  const out: string[] = [];
  for (const item of flat) {
    let uri: vscode.Uri | undefined;
    if (item instanceof vscode.Uri) {
      uri = item;
    } else if (item && typeof item === "object" && "resourceUri" in item) {
      const r = (item as { resourceUri?: unknown }).resourceUri;
      if (r instanceof vscode.Uri) {
        uri = r;
      }
    }
    if (uri && uri.scheme === "file") {
      const rel = path.relative(rootDir, uri.fsPath).replace(/\\/g, "/");
      if (rel && !rel.startsWith("..") && !seen.has(rel)) {
        seen.add(rel);
        out.push(rel);
      }
    }
  }
  return out;
}

export function activate(context: vscode.ExtensionContext): void {
  extensionStorageDir = context.globalStorageUri.fsPath;
  const initialRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  const ctrl = new DraftsController(initialRoot);
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

  const applyActiveHunk = async (direction: "promote" | "demote") => {
    const editor = vscode.window.activeTextEditor;
    const status = ctrl.getStatus();
    const rootDir = ctrl.getRoot();
    if (!editor || !status || !rootDir) {
      return;
    }
    const parsed = parseLayerUri(editor.document.uri);
    if (!parsed) {
      return;
    }
    const layers = status.layers;
    const idx = layers.findIndex((l) => l.name === parsed.spec);
    if (parsed.spec === "head" || idx < 0) {
      void vscode.window.showInformationMessage(
        "Place cursor inside the target layer side of the diff to promote/demote its hunk."
      );
      return;
    }
    if (direction === "promote" && idx === 0) {
      void vscode.window.showInformationMessage("[L1] ready is already the lowest layer.");
      return;
    }

    const belowSpec = idx === 0 ? "head" : layers[idx - 1].name;
    const aboveSpec = idx + 1 < layers.length ? layers[idx + 1].name : "wt";
    const diffOut = await runGd(rootDir, [
      "diff",
      "--from",
      belowSpec,
      "--to",
      parsed.spec,
      "--",
      parsed.relPath,
    ]);
    const hunkPatch = extractHunkAtLine(
      diffOut,
      editor.selection.active.line + 1,
      true
    );
    if (!hunkPatch) {
      void vscode.window.showInformationMessage(
        "Move cursor onto a changed hunk in this layer to apply it."
      );
      return;
    }
    const toSpec = direction === "promote" ? belowSpec : aboveSpec;
    if (toSpec === "head") {
      return;
    }
    await ctrl.execAndRefresh(["patch-apply", "--from", parsed.spec, "--to", toSpec], hunkPatch);
  };

  context.subscriptions.push(
    vscode.commands.registerCommand("gitDrafts.focus", () => {
      void vscode.commands.executeCommand("workbench.view.extension.git-drafts");
    }),
    vscode.commands.registerCommand("gitDrafts.installCli", () => {
      const term = vscode.window.createTerminal("Install git-drafts");
      term.show();
      term.sendText("cargo install --git https://github.com/Girgis98/git-drafts.git");
    }),
    vscode.commands.registerCommand(
      "gitDrafts.downloadCli",
      wrap(async () => {
        await downloadPrebuiltGd();
        await ctrl.refresh();
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.openDiff",
      async (left: vscode.Uri, right: vscode.Uri, title: string) => {
        await vscode.commands.executeCommand("vscode.diff", left, right, title);
      }
    ),
    vscode.commands.registerCommand(
      "gitDrafts.init",
      wrap(async (preset?: unknown) => {
        let raw = typeof preset === "string" ? preset : undefined;
        if (raw === undefined) {
          raw = await vscode.window.showInputBox({
            prompt: "Draft layers (ordered commit-ready → raw, space-separated)",
            value: "ready reviewed draft",
          });
        }
        if (raw === undefined) {
          return;
        }
        const names = raw.trim().split(/\s+/).filter(Boolean);
        await ctrl.execAndRefresh(["init", "--sync-git-index", ...names]);
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.stageFromGit",
      wrap(async (...args: unknown[]) => {
        const rootDir = ctrl.getRoot();
        if (!rootDir) {
          return;
        }
        const paths = extractGitResourcePaths(rootDir, args);
        if (paths.length > 0) {
          await ctrl.execAndRefresh(["stage", "--", ...paths]);
        }
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.stageToLayer",
      wrap(async (...args: unknown[]) => {
        const rootDir = ctrl.getRoot();
        const status = ctrl.getStatus();
        if (!rootDir || !status) {
          return;
        }
        const paths = extractGitResourcePaths(rootDir, args);
        if (paths.length === 0) {
          return;
        }
        const picked = await vscode.window.showQuickPick(
          status.layers.map((l) => ({
            label: `[L${l.index}] ${l.name}`,
            layerName: l.name,
          })),
          { placeHolder: "Select target draft layer" }
        );
        if (!picked) {
          return;
        }
        await ctrl.execAndRefresh(["stage", "-l", picked.layerName, "--", ...paths]);
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.stageAllFromGit",
      wrap(async () => ctrl.execAndRefresh(["stage", "-A"]))
    ),
    vscode.commands.registerCommand(
      "gitDrafts.demoteFromGit",
      wrap(async (...args: unknown[]) => {
        const rootDir = ctrl.getRoot();
        const status = ctrl.getStatus();
        if (!rootDir || !status || status.layers.length === 0) {
          return;
        }
        const paths = extractGitResourcePaths(rootDir, args);
        if (paths.length > 0) {
          await ctrl.execAndRefresh(["demote", "--from", status.layers[0].name, "--", ...paths]);
        }
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.refresh",
      wrap(async () => ctrl.refresh())
    ),
    vscode.commands.registerCommand(
      "gitDrafts.stage",
      wrap(async (...args: unknown[]) => {
        const targets = ctrl.resolveActionTargets(args);
        if (targets.wtPaths.length > 0 && !targets.wtAll) {
          await ctrl.execAndRefresh(["stage", "--", ...targets.wtPaths]);
        } else {
          await ctrl.execAndRefresh(["stage", "-A"]);
        }
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.promote",
      wrap(async (...args: unknown[]) => {
        const targets = ctrl.resolveActionTargets(args);
        const rootDir = ctrl.getRoot();
        if (!rootDir) {
          return;
        }
        for (const [layer, paths] of targets.byLayer) {
          await runGd(rootDir, ["promote", "--from", layer, "--", ...paths]);
        }
        for (const layer of targets.wholeLayers) {
          await runGd(rootDir, ["promote", "--from", layer, "-A"]);
        }
        await ctrl.refreshAndSyncGit();
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.demote",
      wrap(async (...args: unknown[]) => {
        const targets = ctrl.resolveActionTargets(args);
        const rootDir = ctrl.getRoot();
        if (!rootDir) {
          return;
        }
        for (const [layer, paths] of targets.byLayer) {
          await runGd(rootDir, ["demote", "--from", layer, "--", ...paths]);
        }
        for (const layer of targets.wholeLayers) {
          await runGd(rootDir, ["demote", "--from", layer, "-A"]);
        }
        await ctrl.refreshAndSyncGit();
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.promoteHunk",
      wrap(async () => applyActiveHunk("promote"))
    ),
    vscode.commands.registerCommand(
      "gitDrafts.demoteHunk",
      wrap(async () => applyActiveHunk("demote"))
    ),
    vscode.commands.registerCommand(
      "gitDrafts.drop",
      wrap(async (...args: unknown[]) => {
        const targets = ctrl.resolveActionTargets(args);
        const rootDir = ctrl.getRoot();
        if (!rootDir) {
          return;
        }
        for (const [layer, paths] of targets.byLayer) {
          await runGd(rootDir, ["drop", layer, "--", ...paths]);
        }
        await ctrl.refreshAndSyncGit();
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.commitReady",
      wrap(async () => {
        const inputBox = ctrl.getScmInput();
        let msg = inputBox?.value.trim() ?? "";
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
        if (inputBox) {
          inputBox.value = "";
        }
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.commitFile",
      wrap(async (...args: unknown[]) => {
        const targets = ctrl.resolveActionTargets(args);
        const files: string[] = [];
        for (const paths of targets.byLayer.values()) {
          for (const p of paths) {
            if (!files.includes(p)) {
              files.push(p);
            }
          }
        }
        if (files.length === 0) {
          return;
        }
        const label = files.length === 1 ? `'${files[0]}'` : `${files.length} files`;
        const msg = await vscode.window.showInputBox({
          prompt: `Commit ${label} from [L1] ready`,
        });
        if (!msg?.trim()) {
          return;
        }
        await ctrl.execAndRefresh(["commit", ...files, "-m", msg.trim()]);
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
      "gitDrafts.layerRename",
      wrap(async (arg: unknown) => {
        const target = ctrl.resolveActionTarget(arg);
        if (!target.layerName) {
          return;
        }
        const next = await vscode.window.showInputBox({
          prompt: `Rename layer '${target.layerName}' to`,
          value: target.layerName,
        });
        if (!next?.trim() || next.trim() === target.layerName) {
          return;
        }
        await ctrl.execAndRefresh(["layer", "rename", target.layerName, next.trim()]);
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.layerRemove",
      wrap(async (arg: unknown) => {
        const target = ctrl.resolveActionTarget(arg);
        if (!target.layerName) {
          return;
        }
        const mode = await vscode.window.showQuickPick(
          [
            { label: "Merge changes into upper layer (--merge-up)", flag: "--merge-up" },
            { label: "Merge changes into lower layer (--merge-down)", flag: "--merge-down" },
            { label: "Drop changes in this layer (--drop)", flag: "--drop" },
          ],
          { placeHolder: `Remove layer '${target.layerName}'` }
        );
        if (!mode) {
          return;
        }
        await ctrl.execAndRefresh(["layer", "rm", target.layerName, mode.flag]);
      })
    ),
    vscode.commands.registerCommand(
      "gitDrafts.undo",
      wrap(async () => ctrl.execAndRefresh(["undo"]))
    ),
    vscode.commands.registerCommand(
      "gitDrafts.toggleSyncGitIndex",
      wrap(async () => {
        const next = !(ctrl.getStatus()?.sync_git_index ?? false);
        await ctrl.execAndRefresh(["config", "sync-git-index", String(next)]);
        void vscode.window.showInformationMessage(
          `Git Drafts: .git/index mirror ${next ? "enabled" : "disabled"}`
        );
      })
    )
  );
}

export function deactivate(): void {}
