<script lang="ts">
  import { onMount } from "svelte";
  import { ClipboardPaste, Copy, Eye, EyeOff, FolderSearch, Pencil, Plug, Plus, RefreshCw, Server, Terminal, Trash2, Zap } from "@lucide/svelte";
  import { api, errorText } from "../../lib/api";
  import type { ImportedServer, McpServerConfig, McpSettings, McpStatus, McpToolInfo } from "../../lib/types";
  import { confirmDialog, toast, toastError } from "../../lib/ui.svelte";
  import Toggle from "../Toggle.svelte";

  let s = $state<McpSettings | null>(null);
  let status = $state<McpStatus | null>(null);
  let showToken = $state(false);
  let snippet = $state<"claude" | "http" | "stdio">("claude");
  let portDraft = $state("");

  // Server editor
  let edit = $state<null | {
    id: string;
    name: string;
    kind: "stdio" | "http";
    command: string;
    args: string;
    env: string;
    cwd: string | null;
    url: string;
    headers: string;
    autoApprove: boolean;
    enabled: boolean;
  }>(null);
  let testing = $state<string | null>(null);
  let tools = $state<Record<string, McpToolInfo[]>>({});

  // Import
  let importing = $state<null | { list: ImportedServer[]; picked: Record<number, boolean>; paste: boolean; text: string }>(null);
  let discovering = $state(false);

  onMount(async () => {
    try {
      [s, status] = await Promise.all([api.mcpSettings(), api.mcpStatus()]);
      portDraft = String(s.port);
    } catch (e) {
      toastError(e);
    }
  });

  async function save(next: McpSettings) {
    try {
      status = await api.setMcpSettings(next);
      // Re-read: the backend fills in ids and normalizes names.
      s = await api.mcpSettings();
      if (status.error && next.serverEnabled) toast(status.error, { kind: "error", duration: 6000 });
    } catch (e) {
      toastError(e);
      s = await api.mcpSettings();
    }
  }

  const snap = () => $state.snapshot(s) as McpSettings;
  const newId = () => crypto.randomUUID?.() ?? `mcp_${Date.now().toString(36)}${Math.random().toString(36).slice(2)}`;

  function setServer(enabled: boolean) {
    void save({ ...snap(), serverEnabled: enabled });
  }

  function setPerm(k: keyof McpSettings["permissions"], v: boolean) {
    const cur = snap();
    void save({ ...cur, permissions: { ...cur.permissions, [k]: v } });
  }

  function applyPort() {
    const p = Number(portDraft);
    if (!Number.isInteger(p) || p < 1024 || p > 65535) {
      toast("Use a port between 1024 and 65535", { kind: "error" });
      portDraft = String(s?.port ?? 47311);
      return;
    }
    if (p !== s?.port) void save({ ...snap(), port: p });
  }

  async function regenerate() {
    const r = await confirmDialog({
      title: "Create a new access token?",
      message: "Apps connected with the current token will lose access until you update their configuration.",
      confirmLabel: "New token",
      danger: true,
    });
    if (r !== "confirm") return;
    try {
      status = await api.mcpRegenerateToken();
      toast("New token created");
    } catch (e) {
      toastError(e);
    }
  }

  async function copy(text: string, what = "Copied") {
    try {
      await navigator.clipboard.writeText(text);
      toast(what);
    } catch {
      toast("Couldn't copy to the clipboard", { kind: "error" });
    }
  }

  const url = $derived(status?.url ?? `http://127.0.0.1:${s?.port ?? 47311}/mcp`);
  const token = $derived(status?.token ?? "");
  const snippets = $derived({
    claude: `claude mcp add --transport http remail ${url} --header "Authorization: Bearer ${token}"`,
    http: JSON.stringify({ mcpServers: { remail: { type: "http", url, headers: { Authorization: `Bearer ${token}` } } } }, null, 2),
    stdio: JSON.stringify({ mcpServers: { remail: { command: status?.command ?? "remail", args: ["--mcp"] } } }, null, 2),
  });

  // ------------------------------------------------------------------ servers

  /** Split a command line, honouring quotes. */
  function splitArgs(line: string): string[] {
    const out: string[] = [];
    let cur = "";
    let quote: string | null = null;
    let any = false;
    for (const ch of line) {
      if (quote) {
        if (ch === quote) quote = null;
        else cur += ch;
      } else if (ch === '"' || ch === "'") {
        quote = ch;
        any = true;
      } else if (/\s/.test(ch)) {
        if (cur || any) out.push(cur);
        cur = "";
        any = false;
      } else cur += ch;
    }
    if (cur || any) out.push(cur);
    return out;
  }

  const joinArgs = (args: string[]) => args.map((a) => (/[\s"']/.test(a) || a === "" ? JSON.stringify(a) : a)).join(" ");

  function parsePairs(text: string, sep: "=" | ":"): Record<string, string> {
    const out: Record<string, string> = {};
    for (const line of text.split("\n")) {
      const i = line.indexOf(sep);
      if (i > 0) out[line.slice(0, i).trim()] = line.slice(i + 1).trim();
    }
    return out;
  }

  const pairs = (o: Record<string, string>, sep: string) =>
    Object.entries(o)
      .map(([k, v]) => `${k}${sep}${v}`)
      .join("\n");

  function describe(c: McpServerConfig): string {
    return c.transport.type === "stdio" ? [c.transport.command, ...c.transport.args].join(" ") : c.transport.url;
  }

  function openEditor(c: McpServerConfig | null) {
    if (!c) {
      edit = { id: "", name: "", kind: "stdio", command: "", args: "", env: "", cwd: null, url: "", headers: "", autoApprove: false, enabled: true };
      return;
    }
    const t = c.transport;
    edit = {
      id: c.id,
      name: c.name,
      kind: t.type,
      command: t.type === "stdio" ? joinArgs([t.command]) : "",
      args: t.type === "stdio" ? joinArgs(t.args) : "",
      env: t.type === "stdio" ? pairs(t.env, "=") : "",
      cwd: t.type === "stdio" ? t.cwd : null,
      url: t.type === "http" ? t.url : "",
      headers: t.type === "http" ? pairs(t.headers, ": ") : "",
      autoApprove: c.autoApprove,
      enabled: c.enabled,
    };
  }

  function fromEditor(): McpServerConfig | null {
    if (!edit) return null;
    if (!edit.name.trim()) {
      toast("Give the server a name", { kind: "error" });
      return null;
    }
    if (edit.kind === "stdio" && !edit.command.trim()) {
      toast("Enter the command that starts the server", { kind: "error" });
      return null;
    }
    if (edit.kind === "http" && !/^https?:\/\//i.test(edit.url.trim())) {
      toast("Enter the server's http(s) URL", { kind: "error" });
      return null;
    }
    // "npx -y pkg" typed into the command box: move the rest into the arguments.
    const cmd = splitArgs(edit.command.trim());
    return {
      id: edit.id || newId(),
      name: edit.name.trim(),
      enabled: edit.enabled,
      autoApprove: edit.autoApprove,
      transport:
        edit.kind === "stdio"
          ? { type: "stdio", command: cmd[0], args: [...cmd.slice(1), ...splitArgs(edit.args)], env: parsePairs(edit.env, "="), cwd: edit.cwd }
          : { type: "http", url: edit.url.trim(), headers: parsePairs(edit.headers, ":") },
    };
  }

  async function saveServer() {
    const c = fromEditor();
    if (!c || !s) return;
    const servers = snap().servers;
    const i = servers.findIndex((x) => x.id === c.id);
    if (i >= 0) servers[i] = c;
    else servers.push(c);
    await save({ ...snap(), servers });
    edit = null;
  }

  async function removeServer(c: McpServerConfig) {
    const r = await confirmDialog({ title: `Remove ${c.name}?`, message: "The assistant will no longer be able to use its tools.", confirmLabel: "Remove", danger: true });
    if (r !== "confirm") return;
    await save({ ...snap(), servers: snap().servers.filter((x) => x.id !== c.id) });
  }

  function patchServer(c: McpServerConfig, patch: Partial<McpServerConfig>) {
    void save({ ...snap(), servers: snap().servers.map((x) => (x.id === c.id ? { ...x, ...patch } : x)) });
  }

  async function test(c: McpServerConfig | null) {
    const cfg = c ?? fromEditor();
    if (!cfg) return;
    const key = c ? c.id : edit?.id || "new";
    testing = key;
    try {
      const r = await api.mcpTest(cfg.transport);
      tools[key] = r.tools;
      toast(`${r.server.name || cfg.name} is working: ${r.tools.length} tool${r.tools.length === 1 ? "" : "s"}`, { kind: "success" });
    } catch (e) {
      toast(errorText(e), { kind: "error", duration: 9000 });
    } finally {
      testing = null;
    }
  }

  async function discover() {
    discovering = true;
    try {
      const list = await api.mcpDiscover();
      const known = new Set(s?.servers.map((x) => JSON.stringify(x.transport)));
      const fresh = list.filter((x) => !known.has(JSON.stringify(x.config.transport)));
      if (fresh.length === 0) {
        toast(list.length ? "Everything found is already added" : "No MCP servers found in other apps");
        return;
      }
      importing = { list: fresh, picked: Object.fromEntries(fresh.map((x, i) => [i, x.config.enabled])), paste: false, text: "" };
    } catch (e) {
      toastError(e);
    } finally {
      discovering = false;
    }
  }

  async function parsePaste() {
    if (!importing) return;
    try {
      const list = await api.mcpParse(importing.text);
      importing = { ...importing, list, picked: Object.fromEntries(list.map((x, i) => [i, x.config.enabled])), paste: false };
      if (list.length === 0) toast("No servers found in that text", { kind: "error" });
    } catch (e) {
      toastError(e);
    }
  }

  async function importPicked() {
    if (!importing || !s) return;
    const add = importing.list.filter((_, i) => importing!.picked[i]).map((x) => ({ ...x.config, id: newId() }));
    const names = new Set(s.servers.map((x) => x.name));
    for (const a of add) {
      let n = a.name;
      for (let i = 2; names.has(n); i++) n = `${a.name} ${i}`;
      a.name = n;
      names.add(n);
    }
    await save({ ...snap(), servers: [...snap().servers, ...add] });
    importing = null;
    toast(`Imported ${add.length} server${add.length === 1 ? "" : "s"}`);
  }
</script>

{#if s}
  <h1>Connections</h1>
  <p class="intro">Remail speaks the Model Context Protocol (MCP): AI agents in other apps can work with your email, and Remail's assistant can use tools from other apps.</p>

  <!-- ------------------------------------------------------------ server -->
  <div class="group-title"><Server size={12} /> Let AI agents use Remail</div>
  <div class="row">
    <div class="row-text">
      <div class="row-label">MCP server</div>
      <div class="row-desc">Claude Code, Claude Desktop, Cursor and other MCP clients on this computer can search, read and draft your email.</div>
    </div>
    <Toggle label="MCP server" checked={s.serverEnabled} onchange={setServer} />
  </div>

  {#if s.serverEnabled}
    <div class="status" class:bad={!status?.running}>
      <span class="led"></span>
      {#if status?.running}Running at <code class="selectable">{status.url}</code>{:else}Not running{status?.error ? `: ${status.error}` : ""}{/if}
    </div>

    <div class="sub">What agents may do</div>
    <div class="perm"><span>Search and read email</span><Toggle label="Search and read email" checked={true} disabled onchange={() => {}} /></div>
    <div class="perm"><span>Create drafts</span><Toggle label="Create drafts" checked={s.permissions.drafts} onchange={(v) => setPerm("drafts", v)} /></div>
    <div class="perm"><span>Send email <em>(goes through your undo-send delay)</em></span><Toggle label="Send email" checked={s.permissions.send} onchange={(v) => setPerm("send", v)} /></div>
    <div class="perm"><span>Archive, delete, move and snooze</span><Toggle label="Organize email" checked={s.permissions.organize} onchange={(v) => setPerm("organize", v)} /></div>

    <div class="sub">Connect an app</div>
    <div class="token">
      <span class="lbl">Access token</span>
      <code class="selectable">{showToken ? token : token.slice(0, 6) + "•".repeat(24)}</code>
      <button class="icon-btn" title={showToken ? "Hide" : "Show"} onclick={() => (showToken = !showToken)}>{#if showToken}<EyeOff size={14} />{:else}<Eye size={14} />{/if}</button>
      <button class="icon-btn" title="Copy token" onclick={() => copy(token, "Token copied")}><Copy size={14} /></button>
      <button class="icon-btn" title="New token" onclick={regenerate}><RefreshCw size={14} /></button>
    </div>
    <div class="tabs" role="tablist">
      <button role="tab" aria-selected={snippet === "claude"} class:on={snippet === "claude"} onclick={() => (snippet = "claude")}>Claude Code</button>
      <button role="tab" aria-selected={snippet === "http"} class:on={snippet === "http"} onclick={() => (snippet = "http")}>HTTP config (Cursor, VS Code…)</button>
      <button role="tab" aria-selected={snippet === "stdio"} class:on={snippet === "stdio"} onclick={() => (snippet = "stdio")}>Command config (Claude Desktop…)</button>
    </div>
    <div class="snippet">
      <pre class="selectable">{snippets[snippet]}</pre>
      <button class="btn copy" onclick={() => copy(snippets[snippet])}><Copy size={13} /> Copy</button>
    </div>
    <p class="hint">
      {#if snippet === "claude"}Run this in a terminal. Claude Code then has Remail's tools in every project.
      {:else if snippet === "http"}Add this to the app's MCP configuration (for Claude Desktop, use the command config instead).
      {:else}The command starts a small bridge that forwards to Remail and opens Remail in the tray if it isn't running.{/if}
    </p>
    <div class="perm">
      <span>Port</span>
      <input class="input port" bind:value={portDraft} onblur={applyPort} onkeydown={(e) => e.key === "Enter" && e.currentTarget.blur()} />
    </div>
  {/if}

  <!-- ---------------------------------------------------------- clients -->
  <div class="group-title"><Plug size={12} /> Tools for Remail's assistant</div>
  <p class="desc">Connect MCP servers (calendar, notes, tasks, CRM…) and the assistant can use their tools while it works on your email.</p>

  <div class="list">
    {#each s.servers as c (c.id)}
      <div class="srv">
        <div class="srv-main">
          <div class="srv-name">{#if c.transport.type === "stdio"}<Terminal size={13} />{:else}<Zap size={13} />{/if} {c.name}</div>
          <div class="srv-desc selectable">{describe(c)}</div>
          {#if tools[c.id]}
            <div class="tools">{tools[c.id].map((t) => t.name).join(" · ") || "No tools"}</div>
          {/if}
          <label class="check"><input type="checkbox" checked={c.autoApprove} onchange={(e) => patchServer(c, { autoApprove: e.currentTarget.checked })} /> Run its tools without asking</label>
        </div>
        <div class="srv-actions">
          <Toggle label="Enabled" checked={c.enabled} onchange={(v) => patchServer(c, { enabled: v })} />
          <button class="icon-btn" title="Test" disabled={testing === c.id} onclick={() => test(c)}>{#if testing === c.id}<span class="spinner"></span>{:else}<Zap size={14} />{/if}</button>
          <button class="icon-btn" title="Edit" onclick={() => openEditor(c)}><Pencil size={14} /></button>
          <button class="icon-btn" title="Remove" onclick={() => removeServer(c)}><Trash2 size={14} /></button>
        </div>
      </div>
    {:else}
      <div class="empty">No servers yet.</div>
    {/each}
  </div>

  {#if edit}
    <div class="card">
      <div class="card-title">{edit.id ? "Edit server" : "Add server"}</div>
      <div class="field"><label for="mcp-name">Name</label><input id="mcp-name" class="input" bind:value={edit.name} placeholder="Calendar" /></div>
      <div class="seg" role="radiogroup" aria-label="Connection type">
        <button role="radio" aria-checked={edit.kind === "stdio"} class:on={edit.kind === "stdio"} onclick={() => (edit!.kind = "stdio")}>Command</button>
        <button role="radio" aria-checked={edit.kind === "http"} class:on={edit.kind === "http"} onclick={() => (edit!.kind = "http")}>URL</button>
      </div>
      {#if edit.kind === "stdio"}
        <div class="field"><label for="mcp-cmd">Command</label><input id="mcp-cmd" class="input mono" bind:value={edit.command} placeholder="npx" spellcheck="false" /></div>
        <div class="field"><label for="mcp-args">Arguments</label><input id="mcp-args" class="input mono" bind:value={edit.args} placeholder="-y @modelcontextprotocol/server-filesystem ~/Documents" spellcheck="false" /></div>
        <div class="field"><label for="mcp-env">Environment (KEY=value per line)</label><textarea id="mcp-env" class="input area mono" bind:value={edit.env} spellcheck="false"></textarea></div>
      {:else}
        <div class="field"><label for="mcp-url">Server URL</label><input id="mcp-url" class="input mono" bind:value={edit.url} placeholder="https://example.com/mcp" spellcheck="false" /></div>
        <div class="field"><label for="mcp-h">Headers (Name: value per line)</label><textarea id="mcp-h" class="input area mono" bind:value={edit.headers} placeholder="Authorization: Bearer …" spellcheck="false"></textarea></div>
      {/if}
      <label class="check"><input type="checkbox" bind:checked={edit.autoApprove} /> Run its tools without asking</label>
      {#if tools[edit.id || "new"]}<div class="tools">{tools[edit.id || "new"].map((t) => t.name).join(" · ")}</div>{/if}
      <div class="form-actions">
        <button class="btn" disabled={testing !== null} onclick={() => test(null)}>{#if testing === (edit.id || "new")}<span class="spinner"></span>{:else}<Zap size={13} />{/if} Test</button>
        <span class="grow"></span>
        <button class="btn" onclick={() => (edit = null)}>Cancel</button>
        <button class="btn primary" onclick={saveServer}>Save</button>
      </div>
    </div>
  {:else if importing}
    <div class="card">
      {#if importing.paste}
        <div class="card-title">Paste an MCP configuration</div>
        <textarea class="input area mono tall" bind:value={importing.text} placeholder={'{ "mcpServers": { "name": { "command": "npx", "args": ["-y", "…"] } } }'} spellcheck="false"></textarea>
        <div class="form-actions">
          <span class="grow"></span>
          <button class="btn" onclick={() => (importing = null)}>Cancel</button>
          <button class="btn primary" disabled={!importing.text.trim()} onclick={parsePaste}>Continue</button>
        </div>
      {:else}
        <div class="card-title">Import MCP servers</div>
        <div class="imports">
          {#each importing.list as it, i (i)}
            <label class="imp">
              <input type="checkbox" bind:checked={importing.picked[i]} />
              <span class="imp-main">
                <span class="srv-name">{it.config.name} <span class="src">{it.source}</span>{#if !it.config.enabled} <span class="src">disabled there</span>{/if}</span>
                <span class="srv-desc">{describe(it.config)}</span>
              </span>
            </label>
          {/each}
        </div>
        <div class="form-actions">
          <span class="grow"></span>
          <button class="btn" onclick={() => (importing = null)}>Cancel</button>
          <button class="btn primary" disabled={!Object.values(importing.picked).some(Boolean)} onclick={importPicked}>Import</button>
        </div>
      {/if}
    </div>
  {:else}
    <div class="add-row">
      <button class="btn" onclick={() => openEditor(null)}><Plus size={14} /> Add server</button>
      <button class="btn" disabled={discovering} onclick={discover}>{#if discovering}<span class="spinner"></span>{:else}<FolderSearch size={14} />{/if} Import from other apps</button>
      <button class="btn" onclick={() => (importing = { list: [], picked: {}, paste: true, text: "" })}><ClipboardPaste size={14} /> Paste config</button>
    </div>
    <p class="hint">Import looks for servers you've set up in Claude Desktop, Claude Code, Cursor, VS Code, Windsurf, Zed, Gemini CLI and Codex.</p>
  {/if}
{/if}

<style>
  h1 {
    margin: 0 0 18px;
    font-size: 20px;
    line-height: 26px;
    font-weight: 600;
  }
  .intro,
  .desc {
    margin: -6px 0 12px;
    color: var(--content-subtle);
    font-size: 13px;
    line-height: 19px;
  }
  .desc {
    margin: 0 0 10px;
  }
  .group-title {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 26px 0 6px;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--content-muted);
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    padding: 12px 0;
    border-bottom: 0.5px solid var(--separator-subtle);
  }
  .row-label {
    font-size: 13px;
    font-weight: 500;
  }
  .row-desc {
    margin-top: 2px;
    font-size: 12px;
    line-height: 16px;
    color: var(--content-subtle);
  }
  .status {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 12px 0 4px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .led {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--indicator-success);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--indicator-success) 20%, transparent);
  }
  .status.bad .led {
    background: var(--indicator-error);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--indicator-error) 20%, transparent);
  }
  code,
  pre,
  .mono {
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .sub {
    margin: 16px 0 4px;
    font-size: 12px;
    font-weight: 600;
  }
  .perm {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 7px 0;
    font-size: 13px;
  }
  .perm em {
    font-style: normal;
    color: var(--content-subtle);
    font-size: 12px;
  }
  .port {
    width: 90px;
  }
  .token {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 4px 0 10px;
  }
  .token code {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 5px 8px;
    border-radius: 6px;
    background: var(--surface-hover);
  }
  .lbl,
  .field label {
    font-size: 12px;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-bottom: 6px;
  }
  .tabs button {
    border: none;
    background: none;
    padding: 4px 10px;
    border-radius: 999px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .tabs button.on {
    background: var(--accent-soft);
    color: var(--accent);
    font-weight: 500;
  }
  .snippet {
    position: relative;
  }
  .snippet pre {
    margin: 0;
    padding: 12px 14px;
    padding-right: 80px;
    border-radius: 10px;
    background: var(--canvas-subtle);
    border: 0.5px solid var(--separator);
    white-space: pre-wrap;
    word-break: break-all;
    line-height: 17px;
  }
  .copy {
    position: absolute;
    top: 8px;
    right: 8px;
  }
  .hint {
    margin: 6px 0 4px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .list {
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-xl);
    background: var(--surface-elevated);
    overflow: hidden;
    margin-bottom: 12px;
  }
  .list > :global(* + *) {
    border-top: 0.5px solid var(--separator-subtle);
  }
  .empty {
    padding: 18px;
    text-align: center;
    color: var(--content-subtle);
  }
  .srv {
    display: flex;
    gap: 12px;
    padding: 11px 14px;
    align-items: flex-start;
  }
  .srv-main,
  .imp-main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .srv-name {
    display: flex;
    align-items: center;
    gap: 6px;
    font-weight: 500;
  }
  .srv-desc {
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--content-subtle);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tools {
    font-size: 11.5px;
    color: var(--content-muted);
  }
  .srv-actions {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--content-subtle);
  }
  .card {
    border: 0.5px solid var(--separator);
    border-radius: var(--radius-xl);
    background: var(--surface-elevated);
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .card-title {
    font-weight: 600;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .area {
    min-height: 56px;
    resize: vertical;
    padding: 8px 10px;
    user-select: text;
  }
  .area.tall {
    min-height: 140px;
  }
  .seg {
    align-self: flex-start;
    display: inline-flex;
    padding: 2px;
    border-radius: var(--radius-lg);
    background: var(--surface-hover);
  }
  .seg button {
    border: none;
    background: none;
    padding: 4px 12px;
    border-radius: 6px;
    font-size: 12px;
    font-weight: 500;
    color: var(--content-subtle);
  }
  .seg button.on {
    background: var(--surface-overlay);
    color: var(--content);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.12);
  }
  .form-actions,
  .add-row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .grow {
    flex: 1;
  }
  .imports {
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-height: 300px;
    overflow-y: auto;
  }
  .imp {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    padding: 6px 4px;
  }
  .src {
    font-weight: 400;
    font-size: 11px;
    color: var(--content-muted);
  }
</style>
