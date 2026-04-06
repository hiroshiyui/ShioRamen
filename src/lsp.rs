// SPDX-License-Identifier: GPL-3.0-or-later
//! LSP client — queries a language server for hover, definition, references,
//! and diagnostics.
//!
//! Sessions are cached per server command so the same server process is reused
//! across tool calls within a shio session.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

// ── Session ───────────────────────────────────────────────────────────────────

struct LspSession {
    _child: Child,
    stdin: BufWriter<ChildStdin>,
    next_id: i64,
    rx: mpsc::Receiver<Value>,
    opened_files: HashSet<String>,
}

fn sessions() -> &'static Mutex<HashMap<String, LspSession>> {
    static S: OnceLock<Mutex<HashMap<String, LspSession>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Query an LSP server.
///
/// - `operation`: `"hover"` | `"definition"` | `"references"` | `"diagnostics"`
/// - `file`: path to the source file (relative or absolute)
/// - `line` / `column`: **1-indexed** position (converted to 0-indexed for LSP)
/// - `user_servers`: optional overrides from `[lsp.servers]` in `shio.toml`
pub fn query(
    operation: &str,
    file: &str,
    line: u32,
    column: u32,
    user_servers: &HashMap<String, String>,
) -> String {
    let abs_file = match canonicalize_path(file) {
        Ok(p) => p,
        Err(e) => return format!("Error resolving path '{file}': {e}"),
    };

    let ext = Path::new(&abs_file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let server_cmd = match resolve_server(ext, &abs_file, user_servers) {
        Some(cmd) => cmd,
        None => {
            let hint = lsp_candidates(ext)
                .first()
                .copied()
                .unwrap_or("(language not supported)");
            return format!(
                "No LSP server found for .{ext} files. Install one of: {hint}\n\
                 Or set [lsp.servers] in shio.toml: {lang} = \"<server-command>\"\n",
                lang = lang_from_ext(ext),
            );
        }
    };

    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());

    if !map.contains_key(&server_cmd) {
        match start_session(&server_cmd, &abs_file) {
            Ok(s) => {
                map.insert(server_cmd.clone(), s);
            }
            Err(e) => return format!("Failed to start LSP server '{server_cmd}': {e}"),
        }
    }

    // Ensure the file is open in this session.
    let sess = map.get_mut(&server_cmd).expect("inserted above");

    let content_for_open = if !sess.opened_files.contains(&abs_file) {
        match std::fs::read_to_string(&abs_file) {
            Ok(c) => Some(c),
            Err(e) => return format!("Error reading '{abs_file}': {e}"),
        }
    } else {
        None
    };

    if let Some(content) = content_for_open
        && let Err(e) = did_open(sess, &abs_file, &content)
    {
        return format!("LSP didOpen failed: {e}");
    }

    // Convert from 1-indexed (user-facing) to 0-indexed (LSP protocol).
    let line0 = line.saturating_sub(1);
    let col0 = column.saturating_sub(1);

    match operation {
        "hover" => do_hover(sess, &abs_file, line0, col0),
        "definition" => do_definition(sess, &abs_file, line0, col0),
        "references" => do_references(sess, &abs_file, line0, col0),
        "diagnostics" => do_diagnostics(sess, &abs_file),
        op => format!(
            "Unknown LSP operation '{op}'. Use: hover, definition, references, diagnostics."
        ),
    }
}

// ── Server selection ──────────────────────────────────────────────────────────

fn resolve_server(
    ext: &str,
    abs_file: &str,
    user_servers: &HashMap<String, String>,
) -> Option<String> {
    // Tier 1: user config (by language name or extension).
    let lang = lang_from_ext(ext);
    if let Some(cmd) = user_servers.get(lang).or_else(|| user_servers.get(ext)) {
        return Some(cmd.clone());
    }

    // Tier 2: project-root detection (walk up from the file's directory).
    if let Some(cmd) = detect_project_server(ext, abs_file) {
        return Some(cmd);
    }

    // Tier 3: check PATH for known candidates.
    for candidate in lsp_candidates(ext) {
        if is_in_path(candidate) {
            return Some(candidate.to_string());
        }
    }

    None
}

fn lang_from_ext(ext: &str) -> &str {
    match ext {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "go" => "go",
        "lua" => "lua",
        "zig" => "zig",
        "rb" => "ruby",
        "java" => "java",
        "sh" | "bash" => "bash",
        _ => ext,
    }
}

fn detect_project_server(ext: &str, abs_file: &str) -> Option<String> {
    let file_path = Path::new(abs_file);
    let mut dir = file_path.parent()?;
    loop {
        let server: Option<&str> = match ext {
            "rs" if dir.join("Cargo.toml").exists() => Some("rust-analyzer"),
            "py" | "pyi" if dir.join("pyrightconfig.json").exists() => {
                Some("pyright-langserver --stdio")
            }
            "py" | "pyi" if dir.join("pyproject.toml").exists() => {
                Some("pyright-langserver --stdio")
            }
            "ts" | "tsx" | "js" | "jsx" | "mjs"
                if dir.join("tsconfig.json").exists() || dir.join("package.json").exists() =>
            {
                Some("typescript-language-server --stdio")
            }
            "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hxx"
                if dir.join(".clangd").exists()
                    || dir.join("compile_commands.json").exists()
                    || dir.join("CMakeLists.txt").exists() =>
            {
                Some("clangd")
            }
            "go" if dir.join("go.mod").exists() => Some("gopls"),
            _ => None,
        };
        if let Some(cmd) = server
            && is_in_path(cmd)
        {
            return Some(cmd.to_string());
        }
        match dir.parent() {
            Some(p) if p != dir => dir = p,
            _ => break,
        }
    }
    None
}

fn lsp_candidates(ext: &str) -> &'static [&'static str] {
    match ext {
        "rs" => &["rust-analyzer"],
        "py" | "pyi" => &["pyright-langserver --stdio", "pylsp"],
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => &["typescript-language-server --stdio"],
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hxx" => &["clangd", "ccls"],
        "go" => &["gopls"],
        "lua" => &["lua-language-server"],
        "zig" => &["zls"],
        "rb" => &["solargraph stdio"],
        "java" => &["jdtls"],
        "sh" | "bash" => &["bash-language-server start"],
        _ => &[],
    }
}

fn is_in_path(program: &str) -> bool {
    let bin = program.split_whitespace().next().unwrap_or(program);
    std::env::var_os("PATH")
        .map(|path_var| {
            std::env::split_paths(&path_var).any(|dir| is_executable_file(&dir.join(bin)))
        })
        .unwrap_or(false)
}

/// Returns `true` if `path` is a regular file with at least one executable bit set.
/// Falls back to `is_file()` on non-Unix platforms.
fn is_executable_file(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn canonicalize_path(file: &str) -> Result<String, String> {
    let path = Path::new(file);
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    Ok(match abs.canonicalize() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => abs.to_string_lossy().into_owned(),
    })
}

fn file_uri(path: &str) -> String {
    // Absolute paths on Linux start with '/', so file:// + /path = file:///path.
    format!("file://{path}")
}

fn find_project_root(abs_file: &str) -> String {
    let mut dir = match Path::new(abs_file).parent() {
        Some(d) => d,
        None => return abs_file.to_string(),
    };
    loop {
        if dir.join("Cargo.toml").exists()
            || dir.join("go.mod").exists()
            || dir.join("package.json").exists()
            || dir.join(".git").exists()
        {
            return dir.to_string_lossy().into_owned();
        }
        match dir.parent() {
            Some(p) if p != dir => dir = p,
            _ => break,
        }
    }
    Path::new(abs_file)
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string())
}

// ── Session lifecycle ─────────────────────────────────────────────────────────

fn start_session(server_cmd: &str, root_file: &str) -> Result<LspSession, String> {
    let mut parts = server_cmd.split_whitespace();
    let bin = parts.next().ok_or("empty server command")?;
    let extra_args: Vec<&str> = parts.collect();

    let root_path = find_project_root(root_file);
    let root_uri = file_uri(&root_path);

    let mut child = Command::new(bin)
        .args(&extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn failed: {e}"))?;

    let stdout = child.stdout.take().ok_or("child has no stdout")?;
    let stdin = child.stdin.take().ok_or("child has no stdin")?;

    // Reader thread: forward messages through a channel.
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(msg) = read_lsp_message(&mut reader) {
            if tx.send(msg).is_err() {
                break;
            }
        }
    });

    let mut sess = LspSession {
        _child: child,
        stdin: BufWriter::new(stdin),
        next_id: 1,
        rx,
        opened_files: HashSet::new(),
    };

    // initialize request.
    let init_id = sess.next_id;
    sess.next_id += 1;
    send_message(
        &mut sess.stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": init_id,
            "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": root_uri,
                "workspaceFolders": [{"uri": root_uri, "name": "workspace"}],
                "capabilities": {
                    "textDocument": {
                        "hover": {
                            "contentFormat": ["plaintext", "markdown"]
                        },
                        "definition": {},
                        "references": {},
                        "publishDiagnostics": {}
                    }
                }
            }
        }),
    )
    .map_err(|e| format!("initialize send failed: {e}"))?;

    wait_for_id(&mut sess, init_id, Duration::from_secs(15))
        .map_err(|e| format!("initialize response: {e}"))?;

    // initialized notification (required to complete handshake).
    send_message(
        &mut sess.stdin,
        &json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
    )
    .map_err(|e| format!("initialized notification failed: {e}"))?;

    Ok(sess)
}

fn did_open(sess: &mut LspSession, abs_file: &str, content: &str) -> Result<(), String> {
    let ext = Path::new(abs_file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    send_message(
        &mut sess.stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": file_uri(abs_file),
                    "languageId": lang_from_ext(ext),
                    "version": 1,
                    "text": content
                }
            }
        }),
    )
    .map_err(|e| format!("didOpen send failed: {e}"))?;
    sess.opened_files.insert(abs_file.to_string());
    Ok(())
}

// ── JSON-RPC framing ──────────────────────────────────────────────────────────

fn send_message(stdin: &mut BufWriter<ChildStdin>, msg: &Value) -> Result<(), String> {
    let body = msg.to_string();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin
        .write_all(header.as_bytes())
        .map_err(|e| e.to_string())?;
    stdin
        .write_all(body.as_bytes())
        .map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())
}

fn read_lsp_message<R: BufRead>(reader: &mut R) -> Result<Value, String> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("EOF".to_string());
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().ok();
        }
    }
    let len = content_length.ok_or("missing Content-Length header")?;
    const MAX_LSP_MSG: usize = 10 * 1024 * 1024; // 10 MB — sane upper bound
    if len > MAX_LSP_MSG {
        return Err(format!(
            "LSP message too large: {len} bytes (max {MAX_LSP_MSG})"
        ));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map_err(|e| format!("JSON parse error: {e}"))
}

fn wait_for_id(sess: &mut LspSession, id: i64, timeout: Duration) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("timeout waiting for id={id}"));
        }
        match sess
            .rx
            .recv_timeout(remaining.min(Duration::from_millis(200)))
        {
            Ok(msg) => {
                if msg.get("id").and_then(|v| v.as_i64()) == Some(id) {
                    if let Some(err) = msg.get("error") {
                        return Err(format!("LSP error: {err}"));
                    }
                    return Ok(msg);
                }
                // Not our response — discard notifications / other responses.
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("LSP server process died".to_string());
            }
        }
    }
}

fn wait_for_notification(
    sess: &mut LspSession,
    method: &str,
    uri_filter: &str,
    timeout: Duration,
) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match sess
            .rx
            .recv_timeout(remaining.min(Duration::from_millis(300)))
        {
            Ok(msg) => {
                let is_match = msg.get("method").and_then(|v| v.as_str()) == Some(method);
                if is_match {
                    let uri = msg
                        .pointer("/params/uri")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if uri_filter.is_empty() || uri == uri_filter {
                        return Some(msg);
                    }
                }
                // Not what we want — keep reading.
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
}

// ── Operations ────────────────────────────────────────────────────────────────

fn do_hover(sess: &mut LspSession, abs_file: &str, line: u32, column: u32) -> String {
    let id = sess.next_id;
    sess.next_id += 1;
    if let Err(e) = send_message(
        &mut sess.stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": file_uri(abs_file) },
                "position": { "line": line, "character": column }
            }
        }),
    ) {
        return format!("Error sending hover request: {e}");
    }
    match wait_for_id(sess, id, Duration::from_secs(10)) {
        Err(e) => format!("Hover error: {e}"),
        Ok(resp) => extract_hover_text(&resp),
    }
}

fn extract_hover_text(resp: &Value) -> String {
    let result = match resp.get("result") {
        Some(v) if !v.is_null() => v,
        _ => return "No hover information at this position.".to_string(),
    };
    let contents = &result["contents"];
    if contents.is_null() {
        return "No hover information at this position.".to_string();
    }
    // MarkupContent: { kind: "plaintext"|"markdown", value: "..." }
    if let Some(obj) = contents.as_object()
        && let Some(val) = obj.get("value").and_then(|v| v.as_str())
    {
        return val.to_string();
    }
    // Plain string (deprecated but still common)
    if let Some(s) = contents.as_str() {
        return s.to_string();
    }
    // Array of MarkedString | MarkupContent
    if let Some(arr) = contents.as_array() {
        let parts: Vec<String> = arr
            .iter()
            .filter_map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .or_else(|| item["value"].as_str().map(str::to_string))
            })
            .collect();
        if !parts.is_empty() {
            return parts.join("\n---\n");
        }
    }
    "No hover information at this position.".to_string()
}

fn do_definition(sess: &mut LspSession, abs_file: &str, line: u32, column: u32) -> String {
    let id = sess.next_id;
    sess.next_id += 1;
    if let Err(e) = send_message(
        &mut sess.stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": file_uri(abs_file) },
                "position": { "line": line, "character": column }
            }
        }),
    ) {
        return format!("Error sending definition request: {e}");
    }
    match wait_for_id(sess, id, Duration::from_secs(10)) {
        Err(e) => format!("Definition error: {e}"),
        Ok(resp) => format_locations(&resp),
    }
}

fn do_references(sess: &mut LspSession, abs_file: &str, line: u32, column: u32) -> String {
    let id = sess.next_id;
    sess.next_id += 1;
    if let Err(e) = send_message(
        &mut sess.stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/references",
            "params": {
                "textDocument": { "uri": file_uri(abs_file) },
                "position": { "line": line, "character": column },
                "context": { "includeDeclaration": true }
            }
        }),
    ) {
        return format!("Error sending references request: {e}");
    }
    match wait_for_id(sess, id, Duration::from_secs(15)) {
        Err(e) => format!("References error: {e}"),
        Ok(resp) => format_locations(&resp),
    }
}

fn format_locations(resp: &Value) -> String {
    let result = match resp.get("result") {
        Some(v) if !v.is_null() => v,
        _ => return "No locations found.".to_string(),
    };
    // Avoid cloning the whole array; borrow the slice or wrap the single value.
    let single = std::slice::from_ref(result);
    let locs: &[Value] = result.as_array().map(Vec::as_slice).unwrap_or(single);
    if locs.is_empty() {
        return "No locations found.".to_string();
    }
    let mut out = String::new();
    for loc in locs {
        // Location: { uri, range } or LocationLink: { targetUri, targetRange }
        let uri = loc
            .get("uri")
            .or_else(|| loc.get("targetUri"))
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let path = uri.strip_prefix("file://").unwrap_or(uri);
        let range = loc
            .get("range")
            .or_else(|| loc.get("targetRange"))
            .unwrap_or(&Value::Null);
        let start_line = range
            .pointer("/start/line")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            + 1; // convert to 1-indexed
        let start_col = range
            .pointer("/start/character")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            + 1;
        out.push_str(&format!("{path}:{start_line}:{start_col}\n"));
    }
    out.trim_end().to_string()
}

fn do_diagnostics(sess: &mut LspSession, abs_file: &str) -> String {
    let uri = file_uri(abs_file);
    // Trigger fresh diagnostics with a didSave notification.
    let _ = send_message(
        &mut sess.stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": { "textDocument": { "uri": uri } }
        }),
    );
    match wait_for_notification(
        sess,
        "textDocument/publishDiagnostics",
        &uri,
        Duration::from_secs(10),
    ) {
        None => "No diagnostics received (the server may not have responded in time; \
                 try again after the server finishes indexing)."
            .to_string(),
        Some(notif) => format_diagnostics(&notif, abs_file),
    }
}

fn format_diagnostics(notif: &Value, abs_file: &str) -> String {
    let diags = match notif
        .pointer("/params/diagnostics")
        .and_then(|v| v.as_array())
    {
        Some(arr) => arr,
        None => return "No diagnostics.".to_string(),
    };
    if diags.is_empty() {
        return format!("{abs_file}: no issues found.");
    }
    let mut out = String::new();
    for d in diags {
        let line = d
            .pointer("/range/start/line")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            + 1;
        let col = d
            .pointer("/range/start/character")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            + 1;
        let severity = match d["severity"].as_u64() {
            Some(1) => "error",
            Some(2) => "warning",
            Some(3) => "info",
            Some(4) => "hint",
            _ => "diagnostic",
        };
        let msg = d["message"].as_str().unwrap_or("?");
        out.push_str(&format!("{abs_file}:{line}:{col}: {severity}: {msg}\n"));
    }
    out.trim_end().to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_from_ext_rust() {
        assert_eq!(lang_from_ext("rs"), "rust");
    }

    #[test]
    fn lang_from_ext_python() {
        assert_eq!(lang_from_ext("py"), "python");
        assert_eq!(lang_from_ext("pyi"), "python");
    }

    #[test]
    fn lang_from_ext_typescript() {
        assert_eq!(lang_from_ext("ts"), "typescript");
        assert_eq!(lang_from_ext("tsx"), "typescript");
    }

    #[test]
    fn lang_from_ext_unknown_returns_ext() {
        assert_eq!(lang_from_ext("xyz"), "xyz");
    }

    #[test]
    fn file_uri_absolute_path() {
        assert_eq!(file_uri("/home/user/foo.rs"), "file:///home/user/foo.rs");
    }

    #[test]
    fn lsp_candidates_rust() {
        let c = lsp_candidates("rs");
        assert!(c.contains(&"rust-analyzer"));
    }

    #[test]
    fn lsp_candidates_unsupported_ext_is_empty() {
        assert!(lsp_candidates("xyz").is_empty());
    }

    #[test]
    fn is_in_path_finds_sh() {
        // 'sh' is always in PATH on Linux.
        assert!(is_in_path("sh"));
    }

    #[test]
    fn is_in_path_missing_binary_returns_false() {
        assert!(!is_in_path("__nonexistent_binary_xyzzy__"));
    }

    #[test]
    fn is_in_path_handles_command_with_flags() {
        // 'sh --login' → binary is 'sh', which exists.
        assert!(is_in_path("sh --login"));
    }

    #[test]
    fn find_project_root_walks_up_to_git() {
        // The ShioRamen repo has a .git directory.
        let this_file = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lsp.rs");
        let root = find_project_root(this_file);
        let root_path = Path::new(&root);
        assert!(
            root_path.join(".git").exists() || root_path.join("Cargo.toml").exists(),
            "expected project root with .git or Cargo.toml, got: {root}"
        );
    }

    #[test]
    fn detect_project_server_rust_file_in_cargo_project() {
        // lsp.rs lives inside a Cargo project; if rust-analyzer is in PATH,
        // detect_project_server should return it.
        let this_file = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lsp.rs");
        let result = detect_project_server("rs", this_file);
        if is_in_path("rust-analyzer") {
            assert_eq!(result, Some("rust-analyzer".to_string()));
        } else {
            // rust-analyzer not installed — function must return None, not another server.
            assert_eq!(result, None);
        }
    }

    #[test]
    fn detect_project_server_unsupported_ext_returns_none() {
        let this_file = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lsp.rs");
        // .xyz has no project-marker rule, so the result is always None.
        assert_eq!(detect_project_server("xyz", this_file), None);
    }

    #[test]
    fn is_executable_file_detects_executable() {
        // /bin/sh is always an executable file on Linux.
        assert!(is_executable_file(Path::new("/bin/sh")));
    }

    #[test]
    fn is_executable_file_rejects_non_executable() {
        // A freshly written file has no execute bit.
        let path = std::env::temp_dir().join("shio_lsp_not_exec.txt");
        std::fs::write(&path, "data").unwrap();
        // Ensure execute bit is cleared.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o644);
            std::fs::set_permissions(&path, perms).unwrap();
            assert!(!is_executable_file(&path));
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn canonicalize_path_absolute_passthrough() {
        let p = canonicalize_path("/tmp").unwrap();
        assert!(p.starts_with('/'));
    }

    #[test]
    fn format_locations_empty_array() {
        let resp = json!({"jsonrpc": "2.0", "id": 1, "result": []});
        assert_eq!(format_locations(&resp), "No locations found.");
    }

    #[test]
    fn format_locations_null_result() {
        let resp = json!({"jsonrpc": "2.0", "id": 1, "result": null});
        assert_eq!(format_locations(&resp), "No locations found.");
    }

    #[test]
    fn format_locations_single() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "uri": "file:///home/user/foo.rs",
                "range": {"start": {"line": 9, "character": 3}, "end": {"line": 9, "character": 10}}
            }
        });
        let out = format_locations(&resp);
        assert!(out.contains("/home/user/foo.rs:10:4"), "got: {out}");
    }

    #[test]
    fn format_diagnostics_empty() {
        let notif = json!({"params": {"uri": "file:///foo.rs", "diagnostics": []}});
        assert!(format_diagnostics(&notif, "/foo.rs").contains("no issues found"));
    }

    #[test]
    fn format_diagnostics_error() {
        let notif = json!({
            "params": {
                "uri": "file:///foo.rs",
                "diagnostics": [{
                    "range": {"start": {"line": 4, "character": 0}},
                    "severity": 1,
                    "message": "expected `;`"
                }]
            }
        });
        let out = format_diagnostics(&notif, "/foo.rs");
        assert!(
            out.contains("/foo.rs:5:1: error: expected `;`"),
            "got: {out}"
        );
    }

    #[test]
    fn extract_hover_text_markup_content() {
        let resp = json!({
            "result": {
                "contents": {"kind": "markdown", "value": "fn foo() -> i32"}
            }
        });
        assert_eq!(extract_hover_text(&resp), "fn foo() -> i32");
    }

    #[test]
    fn extract_hover_text_null() {
        let resp = json!({"result": null});
        assert!(extract_hover_text(&resp).contains("No hover"));
    }

    // ── lang_from_ext — additional variants ──────────────────────────────────

    #[test]
    fn lang_from_ext_javascript_variants() {
        for ext in ["js", "jsx", "mjs", "cjs"] {
            assert_eq!(lang_from_ext(ext), "javascript", "failed for .{ext}");
        }
    }

    #[test]
    fn lang_from_ext_go() {
        assert_eq!(lang_from_ext("go"), "go");
    }

    #[test]
    fn lang_from_ext_c_and_header() {
        assert_eq!(lang_from_ext("c"), "c");
        assert_eq!(lang_from_ext("h"), "c");
    }

    #[test]
    fn lang_from_ext_cpp_variants() {
        for ext in ["cpp", "cc", "cxx", "hpp", "hxx"] {
            assert_eq!(lang_from_ext(ext), "cpp", "failed for .{ext}");
        }
    }

    #[test]
    fn lang_from_ext_other_languages() {
        assert_eq!(lang_from_ext("lua"), "lua");
        assert_eq!(lang_from_ext("zig"), "zig");
        assert_eq!(lang_from_ext("rb"), "ruby");
        assert_eq!(lang_from_ext("java"), "java");
        assert_eq!(lang_from_ext("sh"), "bash");
        assert_eq!(lang_from_ext("bash"), "bash");
    }

    // ── extract_hover_text — plain string and array forms ────────────────────

    #[test]
    fn extract_hover_text_plain_string() {
        let resp = json!({ "result": { "contents": "fn foo() -> i32" } });
        assert_eq!(extract_hover_text(&resp), "fn foo() -> i32");
    }

    #[test]
    fn extract_hover_text_array_of_strings() {
        let resp = json!({
            "result": {
                "contents": ["first part", "second part"]
            }
        });
        let out = extract_hover_text(&resp);
        assert!(out.contains("first part"), "got: {out}");
        assert!(out.contains("second part"), "got: {out}");
        assert!(out.contains("---"), "got: {out}");
    }

    #[test]
    fn extract_hover_text_array_of_markup_objects() {
        let resp = json!({
            "result": {
                "contents": [
                    { "language": "rust", "value": "fn bar()" },
                    { "kind": "markdown", "value": "docs here" }
                ]
            }
        });
        let out = extract_hover_text(&resp);
        assert!(out.contains("fn bar()"), "got: {out}");
        assert!(out.contains("docs here"), "got: {out}");
    }

    #[test]
    fn extract_hover_text_null_contents() {
        let resp = json!({ "result": { "contents": null } });
        assert!(extract_hover_text(&resp).contains("No hover"));
    }

    // ── format_locations — LocationLink (targetUri / targetRange) form ────────

    #[test]
    fn format_locations_location_link_form() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": [{
                "targetUri": "file:///home/user/bar.rs",
                "targetRange": {
                    "start": { "line": 19, "character": 7 },
                    "end":   { "line": 19, "character": 14 }
                }
            }]
        });
        let out = format_locations(&resp);
        assert!(out.contains("/home/user/bar.rs:20:8"), "got: {out}");
    }

    #[test]
    fn format_locations_array_of_multiple() {
        let resp = json!({
            "result": [
                {
                    "uri": "file:///a.rs",
                    "range": { "start": { "line": 0, "character": 0 } }
                },
                {
                    "uri": "file:///b.rs",
                    "range": { "start": { "line": 9, "character": 3 } }
                }
            ]
        });
        let out = format_locations(&resp);
        assert!(out.contains("/a.rs:1:1"), "got: {out}");
        assert!(out.contains("/b.rs:10:4"), "got: {out}");
    }

    // ── format_diagnostics — all severity variants ────────────────────────────

    #[test]
    fn format_diagnostics_warning() {
        let notif = json!({
            "params": {
                "uri": "file:///foo.rs",
                "diagnostics": [{
                    "range": { "start": { "line": 1, "character": 0 } },
                    "severity": 2,
                    "message": "unused variable"
                }]
            }
        });
        let out = format_diagnostics(&notif, "/foo.rs");
        assert!(out.contains("warning: unused variable"), "got: {out}");
    }

    #[test]
    fn format_diagnostics_info_and_hint() {
        let notif = json!({
            "params": {
                "uri": "file:///foo.rs",
                "diagnostics": [
                    {
                        "range": { "start": { "line": 2, "character": 0 } },
                        "severity": 3,
                        "message": "consider using X"
                    },
                    {
                        "range": { "start": { "line": 3, "character": 0 } },
                        "severity": 4,
                        "message": "rename suggestion"
                    }
                ]
            }
        });
        let out = format_diagnostics(&notif, "/foo.rs");
        assert!(out.contains("info: consider using X"), "got: {out}");
        assert!(out.contains("hint: rename suggestion"), "got: {out}");
    }

    #[test]
    fn format_diagnostics_unknown_severity() {
        let notif = json!({
            "params": {
                "uri": "file:///foo.rs",
                "diagnostics": [{
                    "range": { "start": { "line": 0, "character": 0 } },
                    "message": "something"
                }]
            }
        });
        let out = format_diagnostics(&notif, "/foo.rs");
        assert!(out.contains("diagnostic: something"), "got: {out}");
    }

    // ── read_lsp_message — framing parser ────────────────────────────────────

    #[test]
    fn read_lsp_message_parses_valid_message() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let raw = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = std::io::BufReader::new(raw.as_bytes());
        let msg = read_lsp_message(&mut reader).unwrap();
        assert_eq!(msg["id"].as_i64(), Some(1));
    }

    #[test]
    fn read_lsp_message_handles_extra_headers() {
        let body = r#"{"jsonrpc":"2.0","method":"ping"}"#;
        let raw = format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let mut reader = std::io::BufReader::new(raw.as_bytes());
        let msg = read_lsp_message(&mut reader).unwrap();
        assert_eq!(msg["method"].as_str(), Some("ping"));
    }

    #[test]
    fn read_lsp_message_missing_content_length_returns_error() {
        let raw = "\r\n{\"jsonrpc\":\"2.0\"}";
        let mut reader = std::io::BufReader::new(raw.as_bytes());
        let err = read_lsp_message(&mut reader).unwrap_err();
        assert!(
            err.contains("Content-Length"),
            "expected Content-Length error, got: {err}"
        );
    }

    #[test]
    fn read_lsp_message_rejects_oversized_message() {
        const MAX: usize = 10 * 1024 * 1024;
        let raw = format!("Content-Length: {}\r\n\r\n", MAX + 1);
        let mut reader = std::io::BufReader::new(raw.as_bytes());
        let err = read_lsp_message(&mut reader).unwrap_err();
        assert!(err.contains("too large"), "expected size error, got: {err}");
    }

    #[test]
    fn read_lsp_message_eof_returns_error() {
        let raw = "";
        let mut reader = std::io::BufReader::new(raw.as_bytes());
        let err = read_lsp_message(&mut reader).unwrap_err();
        assert_eq!(err, "EOF");
    }
}
