//! Imports MCP server definitions other apps already have configured
//! (Claude Desktop, Claude Code, Cursor, Windsurf, VS Code, Zed, Gemini CLI,
//! Codex) or that the user pastes as JSON.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::mcp::client::{McpServerConfig, McpTransport};

/// A server definition found in another app's config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedServer {
    /// App the definition came from, e.g. "Cursor".
    pub source: String,
    /// File it was read from (empty for pasted JSON).
    pub path: String,
    pub config: McpServerConfig,
}

/// Scans well-known config files of other apps. Does blocking file I/O;
/// call it from `spawn_blocking` on async runtimes.
pub fn discover() -> Vec<ImportedServer> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    discover_with(&home, dirs::config_dir().as_deref())
}

/// [`discover`] against an explicit home directory.
pub fn discover_in(home: &Path) -> Vec<ImportedServer> {
    discover_with(home, None)
}

fn discover_with(home: &Path, config_dir: Option<&Path>) -> Vec<ImportedServer> {
    let mut config_dirs =
        vec![home.join(".config"), home.join("Library/Application Support"), home.join("AppData/Roaming")];
    if let Some(dir) = config_dir {
        if !config_dirs.iter().any(|d| d == dir) {
            config_dirs.push(dir.to_path_buf());
        }
    }

    let mut found = Vec::new();
    for dir in &config_dirs {
        scan(&mut found, "Claude Desktop", &dir.join("Claude/claude_desktop_config.json"), &[&["mcpServers"]]);
        scan(&mut found, "VS Code", &dir.join("Code/User/mcp.json"), &[&["servers"]]);
        scan(&mut found, "VS Code", &dir.join("Code/User/settings.json"), &[&["mcp", "servers"], &["mcp.servers"]]);
        scan(&mut found, "Zed", &dir.join("zed/settings.json"), &[&["context_servers"]]);
    }
    let claude_json = home.join(".claude.json");
    scan(&mut found, "Claude Code", &claude_json, &[&["mcpServers"]]);
    if let Some(projects) = read_json(&claude_json).and_then(|v| v.get("projects").cloned()) {
        for (project, cfg) in projects.as_object().into_iter().flatten() {
            if let Some(map) = cfg.get("mcpServers").and_then(Value::as_object) {
                found.extend(from_map(map, &format!("Claude Code ({project})"), &claude_json));
            }
        }
    }
    scan(&mut found, "Claude Code", &home.join(".claude/settings.json"), &[&["mcpServers"]]);
    scan(&mut found, "Cursor", &home.join(".cursor/mcp.json"), &[&["mcpServers"]]);
    scan(&mut found, "Windsurf", &home.join(".codeium/windsurf/mcp_config.json"), &[&["mcpServers"]]);
    scan(&mut found, "Gemini CLI", &home.join(".gemini/settings.json"), &[&["mcpServers"]]);
    let codex = home.join(".codex/config.toml");
    if let Ok(text) = std::fs::read_to_string(&codex) {
        let map: Map<String, Value> = parse_codex_toml(&text).into_iter().collect();
        found.extend(from_map(&map, "Codex", &codex));
    }
    finalize(found)
}

/// Parse pasted JSON in any of the common shapes: `{"mcpServers":{...}}`, `{"servers":{...}}`,
/// a bare `{"name": {...}}` map, or a single server object `{"command":..}` / `{"url":..}`.
pub fn parse_config_json(text: &str, source: &str) -> anyhow::Result<Vec<ImportedServer>> {
    let value = parse_jsonc(text)?;
    let Some(obj) = value.as_object() else { anyhow::bail!("expected a JSON object") };
    let path = Path::new("");
    let map = ["mcpServers", "servers", "context_servers", "mcp_servers"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_object))
        .or_else(|| obj.get("mcp").and_then(|m| m.get("servers")).and_then(Value::as_object));
    let found = if let Some(map) = map {
        from_map(map, source, path)
    } else if looks_like_server(&value) {
        let name = obj.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| derive_name(&value));
        from_entry(&name, &value, source, path).into_iter().collect()
    } else {
        from_map(obj, source, path)
    };
    if found.is_empty() {
        anyhow::bail!("no MCP server definitions found");
    }
    Ok(finalize(found))
}

fn scan(found: &mut Vec<ImportedServer>, source: &str, path: &Path, keys: &[&[&str]]) {
    let Some(value) = read_json(path) else { return };
    for key_path in keys {
        let map = key_path.iter().try_fold(&value, |v, k| v.get(*k)).and_then(Value::as_object);
        if let Some(map) = map {
            found.extend(from_map(map, source, path));
        }
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    match parse_jsonc(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::debug!("mcp import: skipping {}: {e}", path.display());
            None
        }
    }
}

fn from_map(map: &Map<String, Value>, source: &str, path: &Path) -> Vec<ImportedServer> {
    map.iter().filter_map(|(name, entry)| from_entry(name, entry, source, path)).collect()
}

fn from_entry(name: &str, entry: &Value, source: &str, path: &Path) -> Option<ImportedServer> {
    let (transport, enabled) = transport_of(entry)?;
    Some(ImportedServer {
        source: source.to_string(),
        path: path.display().to_string(),
        config: McpServerConfig { id: slug(name), name: name.to_string(), enabled, transport, auto_approve: false },
    })
}

fn looks_like_server(v: &Value) -> bool {
    ["command", "url", "serverUrl", "httpUrl"].iter().any(|k| v.get(*k).is_some())
}

/// Maps one app-specific entry to a transport and its enabled flag.
fn transport_of(entry: &Value) -> Option<(McpTransport, bool)> {
    let obj = entry.as_object()?;
    let enabled = !obj.get("disabled").and_then(Value::as_bool).unwrap_or(false)
        && obj.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let str_field = |k: &str| obj.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    // Zed's legacy shape nests the command: {"command": {"path", "args", "env"}}.
    let command_obj = obj.get("command").and_then(Value::as_object);
    let field = |k: &str| command_obj.and_then(|c| c.get(k)).or_else(|| obj.get(k));
    let command = str_field("command").or_else(|| command_obj.and_then(|c| c.get("path")).and_then(Value::as_str));
    if let Some(command) = command {
        let args =
            field("args").and_then(Value::as_array).map(|a| a.iter().map(scalar_string).collect()).unwrap_or_default();
        let transport = McpTransport::Stdio {
            command: command.to_string(),
            args,
            env: string_map(field("env")),
            cwd: str_field("cwd").map(str::to_string),
        };
        return Some((transport, enabled));
    }
    let url = str_field("url").or_else(|| str_field("serverUrl")).or_else(|| str_field("httpUrl"))?;
    let headers = string_map(obj.get("headers").or_else(|| obj.get("http_headers")));
    Some((McpTransport::Http { url: url.to_string(), headers }, enabled))
}

fn scalar_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn string_map(v: Option<&Value>) -> BTreeMap<String, String> {
    v.and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), scalar_string(v))).collect())
        .unwrap_or_default()
}

/// Guesses a name for a pasted single server, e.g. the npm package it runs.
fn derive_name(v: &Value) -> String {
    if let Some(Ok(url)) = v.get("url").or_else(|| v.get("serverUrl")).and_then(Value::as_str).map(reqwest::Url::parse)
    {
        if let Some(host) = url.host_str() {
            return host.to_string();
        }
    }
    let command = v.get("command").and_then(Value::as_str).unwrap_or("server");
    let base = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let runners =
        ["npx", "uvx", "bunx", "pnpx", "pipx", "uv", "pnpm", "yarn", "docker", "deno", "node", "python", "python3"];
    if runners.contains(&base) {
        let skip = ["run", "dlx", "exec", "x", "tool", "-y", "--yes"];
        let args = v.get("args").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str);
        if let Some(arg) = args.into_iter().find(|a| !a.starts_with('-') && !skip.contains(a)) {
            let unversioned = match arg.rfind('@') {
                Some(i) if i > 0 => &arg[..i],
                _ => arg,
            };
            let last = unversioned.rsplit(['/', '\\']).next().unwrap_or(unversioned);
            return last.trim_end_matches(".js").trim_end_matches(".py").to_string();
        }
    }
    base.to_string()
}

/// Stable id slug: lowercase alphanumerics separated by single dashes.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "server".to_string()
    } else {
        out
    }
}

/// Drops entries with an already-seen transport and makes ids unique.
fn finalize(found: Vec<ImportedServer>) -> Vec<ImportedServer> {
    let mut seen: Vec<McpTransport> = Vec::new();
    let mut ids = HashSet::new();
    let mut out = Vec::new();
    for mut server in found {
        if seen.contains(&server.config.transport) {
            continue;
        }
        seen.push(server.config.transport.clone());
        let base = server.config.id.clone();
        let mut n = 2;
        while !ids.insert(server.config.id.clone()) {
            server.config.id = format!("{base}-{n}");
            n += 1;
        }
        out.push(server);
    }
    out
}

/// Parses JSON that may contain `//` and `/* */` comments and trailing commas.
pub(crate) fn parse_jsonc(text: &str) -> anyhow::Result<Value> {
    let text = text.trim_start_matches('\u{feff}');
    if let Ok(v) = serde_json::from_str(text) {
        return Ok(v);
    }
    Ok(serde_json::from_str(&strip_trailing_commas(&strip_comments(text)))?)
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

fn strip_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(next) = chars.get(i + 1) {
                    out.push(*next);
                    i += 1;
                }
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if !matches!(next, Some('}') | Some(']')) {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// Extracts `[mcp_servers.<name>]` tables from a Codex `config.toml`. Handles
/// the subset Codex configs use: strings, booleans, numbers, (multi-line)
/// arrays, inline tables and `[mcp_servers.<name>.env]` sub-tables.
fn parse_codex_toml(text: &str) -> Vec<(String, Value)> {
    let mut servers: Vec<(String, Map<String, Value>)> = Vec::new();
    let mut current: Option<(usize, Option<String>)> = None;
    let mut pending = String::new();
    for raw in text.lines() {
        pending.push_str(strip_toml_comment(raw));
        pending.push('\n');
        if !balanced(&pending) {
            continue;
        }
        let line = std::mem::take(&mut pending);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = None;
            let parts = split_key(header.trim());
            if parts.first().map(String::as_str) == Some("mcp_servers") && (2..=3).contains(&parts.len()) {
                let idx = match servers.iter().position(|(n, _)| *n == parts[1]) {
                    Some(i) => i,
                    None => {
                        servers.push((parts[1].clone(), Map::new()));
                        servers.len() - 1
                    }
                };
                current = Some((idx, parts.get(2).cloned()));
            }
            continue;
        }
        let Some((idx, sub)) = &current else { continue };
        let Some((key, value)) = line.split_once('=') else { continue };
        let Some(value) = TomlValue::parse(value.trim()) else { continue };
        let key = split_key(key.trim()).join(".");
        let table = &mut servers[*idx].1;
        match sub {
            Some(sub) => {
                let entry = table.entry(sub.clone()).or_insert_with(|| Value::Object(Map::new()));
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert(key, value);
                }
            }
            None => {
                table.insert(key, value);
            }
        }
    }
    servers.into_iter().map(|(name, map)| (name, Value::Object(map))).collect()
}

fn strip_toml_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' && q == '"' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' => return &line[..i],
            None => {}
        }
    }
    line
}

/// True when brackets and braces outside strings are balanced.
fn balanced(text: &str) -> bool {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in text.chars() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' && q == '"' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '[' | '{' => depth += 1,
                ']' | '}' => depth -= 1,
                _ => {}
            },
        }
    }
    depth <= 0
}

/// Splits a dotted TOML key, honouring quoted segments.
fn split_key(key: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in key.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '.' => parts.push(std::mem::take(&mut cur).trim().to_string()),
            None => cur.push(c),
        }
    }
    parts.push(cur.trim().to_string());
    parts
}

struct TomlValue<'a> {
    rest: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> TomlValue<'a> {
    fn parse(text: &'a str) -> Option<Value> {
        TomlValue { rest: text.chars().peekable() }.value()
    }

    fn skip_ws(&mut self) {
        while self.rest.next_if(|c| c.is_whitespace()).is_some() {}
    }

    fn value(&mut self) -> Option<Value> {
        self.skip_ws();
        match *self.rest.peek()? {
            '"' | '\'' => self.string().map(Value::String),
            '[' => {
                self.rest.next();
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.rest.next_if_eq(&']').is_some() {
                        return Some(Value::Array(items));
                    }
                    items.push(self.value()?);
                    self.skip_ws();
                    self.rest.next_if_eq(&',');
                }
            }
            '{' => {
                self.rest.next();
                let mut map = Map::new();
                loop {
                    self.skip_ws();
                    if self.rest.next_if_eq(&'}').is_some() {
                        return Some(Value::Object(map));
                    }
                    let key = match self.rest.peek()? {
                        '"' | '\'' => self.string()?,
                        _ => self.bare(|c| c != '=' && !c.is_whitespace()),
                    };
                    self.skip_ws();
                    self.rest.next_if_eq(&'=')?;
                    map.insert(key, self.value()?);
                    self.skip_ws();
                    self.rest.next_if_eq(&',');
                }
            }
            _ => {
                let word = self.bare(|c| !matches!(c, ',' | ']' | '}') && !c.is_whitespace());
                match word.as_str() {
                    "true" => Some(Value::Bool(true)),
                    "false" => Some(Value::Bool(false)),
                    w => w.replace('_', "").parse::<serde_json::Number>().ok().map(Value::Number),
                }
            }
        }
    }

    fn bare(&mut self, keep: impl Fn(char) -> bool) -> String {
        let mut out = String::new();
        while let Some(c) = self.rest.next_if(|c| keep(*c)) {
            out.push(c);
        }
        out
    }

    fn string(&mut self) -> Option<String> {
        let quote = self.rest.next()?;
        let mut out = String::new();
        loop {
            let c = self.rest.next()?;
            if c == quote {
                return Some(out);
            }
            if c == '\\' && quote == '"' {
                match self.rest.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'u' => {
                        let hex: String = (0..4).filter_map(|_| self.rest.next()).collect();
                        out.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)?);
                    }
                    other => out.push(other),
                }
            } else {
                out.push(c);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(s: &ImportedServer) -> (&str, &[String]) {
        match &s.config.transport {
            McpTransport::Stdio { command, args, .. } => (command, args),
            other => panic!("expected stdio, got {other:?}"),
        }
    }

    #[test]
    fn parses_common_shapes() {
        let wrapped = r#"{"mcpServers": {"Git Hub": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"TOKEN": "x", "N": 1}}}}"#;
        let out = parse_config_json(wrapped, "paste").expect("wrapped");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].config.id, "git-hub");
        assert_eq!(out[0].config.name, "Git Hub");
        assert!(out[0].config.enabled && !out[0].config.auto_approve);
        match &out[0].config.transport {
            McpTransport::Stdio { env, .. } => assert_eq!(env.get("N").map(String::as_str), Some("1")),
            _ => panic!("stdio"),
        }

        let vscode = r#"{"servers": {"remote": {"type": "http", "url": "https://x.example/mcp", "headers": {"Authorization": "Bearer t"}}}}"#;
        let out = parse_config_json(vscode, "paste").expect("servers");
        let expected = McpTransport::Http {
            url: "https://x.example/mcp".into(),
            headers: BTreeMap::from([("Authorization".into(), "Bearer t".into())]),
        };
        assert_eq!(out[0].config.transport, expected);

        let bare = r#"{"a": {"command": "uvx", "args": ["mcp-server-time"]}, "b": {"serverUrl": "https://b.example/sse", "type": "sse", "disabled": true}}"#;
        let out = parse_config_json(bare, "paste").expect("bare");
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].config.id, "b");
        assert!(!out[1].config.enabled);

        let single = r#"{"command": "npx", "args": ["-y", "@scope/weather-server@1.2.0"]}"#;
        let out = parse_config_json(single, "paste").expect("single");
        assert_eq!(out[0].config.name, "weather-server");
        assert_eq!(stdio(&out[0]).0, "npx");

        let single_url = r#"{"url": "https://mcp.linear.app/mcp"}"#;
        assert_eq!(parse_config_json(single_url, "paste").expect("url")[0].config.name, "mcp.linear.app");

        assert!(parse_config_json(r#"{"theme": "dark"}"#, "paste").is_err());
        assert!(parse_config_json("[1]", "paste").is_err());
    }

    #[test]
    fn tolerates_jsonc() {
        let text = r#"
        // user settings
        {
            "url": "http://a//b", /* block
            comment */ "x": "has // no comment",
            "mcpServers": {
                "fs": { "command": "npx", "args": ["a", "b",], }, // trailing
            },
        }"#;
        let out = parse_config_json(text, "paste").expect("jsonc");
        assert_eq!(out.len(), 1);
        assert_eq!(stdio(&out[0]).1, ["a", "b"]);
        assert_eq!(parse_jsonc(r#"{"s": "q\"//,]"}"#).expect("escaped")["s"], "q\"//,]");
    }

    #[test]
    fn dedupes_and_uniquifies() {
        let text = r#"{"a": {"command": "x"}, "A": {"command": "y"}, "dup": {"command": "x"}}"#;
        let out = parse_config_json(text, "paste").expect("parse");
        assert_eq!(out.iter().map(|s| s.config.id.as_str()).collect::<Vec<_>>(), ["a", "a-2"]);
    }

    #[test]
    fn parses_codex_toml() {
        let toml = r#"
model = "o3" # comment
[mcp_servers.docs]
command = "npx"
args = [
  "-y", # the flag
  "docs-mcp",
]
env = { "API_KEY" = "k#1", OTHER = 'lit' }

[mcp_servers."remote.one"]
url = "https://r.example/mcp"
http_headers = { X = "y" }
enabled = false

[mcp_servers.docs.env]
EXTRA = "e"

[profiles.x]
command = "ignored"
"#;
        let servers = parse_codex_toml(toml);
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].0, "docs");
        assert_eq!(servers[0].1["args"], serde_json::json!(["-y", "docs-mcp"]));
        assert_eq!(servers[0].1["env"]["API_KEY"], "k#1");
        assert_eq!(servers[0].1["env"]["EXTRA"], "e");
        assert_eq!(servers[1].0, "remote.one");
        let (transport, enabled) = transport_of(&servers[1].1).expect("transport");
        assert!(!enabled);
        assert_eq!(
            transport,
            McpTransport::Http {
                url: "https://r.example/mcp".into(),
                headers: BTreeMap::from([("X".into(), "y".into())])
            }
        );
    }

    #[test]
    fn discovers_configs_under_home() {
        let home = tempfile::tempdir().expect("tempdir");
        let write = |rel: &str, text: &str| {
            let path = home.path().join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, text).expect("write");
        };
        write(
            ".config/Claude/claude_desktop_config.json",
            r#"{"mcpServers": {"fs": {"command": "npx", "args": ["fs"]}}}"#,
        );
        write(
            ".claude.json",
            r#"{"mcpServers": {"fs": {"type": "stdio", "command": "npx", "args": ["fs"]}},
                "projects": {"/work/app": {"mcpServers": {"db": {"type": "http", "url": "http://localhost:9000/mcp"}}}}}"#,
        );
        write(".cursor/mcp.json", r#"{"mcpServers": {"web": {"url": "https://web.example/mcp"}}}"#);
        write(
            ".codeium/windsurf/mcp_config.json",
            r#"{"mcpServers": {"wind": {"serverUrl": "https://w.example/mcp"}}}"#,
        );
        write(
            ".config/Code/User/mcp.json",
            "// vscode\n{\"servers\": {\"vs\": {\"type\": \"stdio\", \"command\": \"vs-mcp\",},},}",
        );
        write(
            ".config/Code/User/settings.json",
            r#"{"editor.fontSize": 12, "mcp": {"servers": {"vs2": {"command": "vs2"}}}}"#,
        );
        write(
            ".config/zed/settings.json",
            r#"{"context_servers": {"zed-legacy": {"command": {"path": "zmcp", "args": ["--stdio"], "env": {}}, "settings": {}}}}"#,
        );
        write(".gemini/settings.json", r#"{"mcpServers": {"gem": {"httpUrl": "https://g.example/mcp"}}}"#);
        write(".codex/config.toml", "[mcp_servers.codex-one]\ncommand = \"cx\"\nargs = [\"a\"]\n");
        write(".config/zed/broken.json", "{");

        let found = discover_in(home.path());
        let summary: Vec<(String, String)> = found.iter().map(|s| (s.source.clone(), s.config.id.clone())).collect();
        let expect = [
            ("Claude Desktop", "fs"),
            ("VS Code", "vs"),
            ("VS Code", "vs2"),
            ("Zed", "zed-legacy"),
            ("Claude Code (/work/app)", "db"),
            ("Cursor", "web"),
            ("Windsurf", "wind"),
            ("Gemini CLI", "gem"),
            ("Codex", "codex-one"),
        ];
        assert_eq!(summary, expect.map(|(a, b)| (a.to_string(), b.to_string())));
        let zed = &found[3];
        assert_eq!(stdio(zed), ("zmcp", &["--stdio".to_string()][..]));
        assert!(found[0].path.ends_with("claude_desktop_config.json"));
    }
}
