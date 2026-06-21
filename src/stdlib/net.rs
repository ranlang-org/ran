//! # Ran HTTP Server - High-Performance Network Module
//!
//! A FastHTTP-inspired HTTP/1.1 server for the Ran programming language.
//! Design principles:
//!   - Pre-allocated buffers (no per-request allocations)
//!   - Multi-threaded connection handling
//!   - Keep-alive (persistent connections)
//!   - Zero-copy parsing with &str slices into read buffer
//!   - Path parameter routing (/users/:id)
//!   - Middleware pipeline
//!   - Static file serving with MIME detection
//!   - Built-in CORS support
//!
//! Uses only std library - no external crates.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

// --- Constants ---------------------------------------------------------------

/// Pre-allocated read buffer size per connection (64KB)
const READ_BUFFER_SIZE: usize = 65_536;

/// Maximum request body size (10MB)
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

/// Keep-alive timeout in seconds
const KEEP_ALIVE_TIMEOUT_SECS: u64 = 30;

/// Maximum requests per keep-alive connection
const MAX_KEEP_ALIVE_REQUESTS: u32 = 100;

/// Default number of worker threads when not overridden via `RAN_WORKERS`.
const DEFAULT_WORKER_THREADS: usize = 256;

/// Resolve the worker-pool size. Honors the `RAN_WORKERS` environment variable,
/// otherwise scales with available CPU parallelism (bounded to a safe default).
fn worker_count() -> usize {
    if let Ok(v) = std::env::var("RAN_WORKERS") {
        if let Ok(n) = v.parse::<usize>() {
            if n > 0 {
                return n;
            }
        }
    }
    std::thread::available_parallelism()
        .map(|n| (n.get() * 32).min(DEFAULT_WORKER_THREADS))
        .unwrap_or(DEFAULT_WORKER_THREADS)
}

// --- Public Types ------------------------------------------------------------

/// HTTP methods supported by the server.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
    Options,
}

impl HttpMethod {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "GET" => Some(Self::Get),
            "POST" => Some(Self::Post),
            "PUT" => Some(Self::Put),
            "DELETE" => Some(Self::Delete),
            "PATCH" => Some(Self::Patch),
            "HEAD" => Some(Self::Head),
            "OPTIONS" => Some(Self::Options),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
        }
    }
}

/// A single route definition exposed to the Ran runtime.
#[derive(Debug, Clone)]
pub struct Route {
    pub method: HttpMethod,
    pub path: String,
    pub handler_name: String,
}

/// Parsed HTTP request with zero-copy references where possible.
/// For the public API we use owned Strings so the request can outlive the buffer.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: HttpMethod,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub query_params: HashMap<String, String>,
    pub path_params: HashMap<String, String>,
    pub cookies: HashMap<String, String>,
}

impl Request {
    /// Parse the raw body as a UTF-8 JSON string.
    pub fn body_as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.body).ok()
    }

    /// Parse URL-encoded form data from the body.
    pub fn form_data(&self) -> HashMap<String, String> {
        self.body_as_str()
            .map(parse_query_string)
            .unwrap_or_default()
    }

    /// Get a specific header value (case-insensitive lookup).
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_lowercase() == lower)
            .map(|(_, v)| v.as_str())
    }
}

/// HTTP response with builder pattern for ergonomic construction.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Response {
    /// 200 OK with plain text body.
    pub fn ok(body: &str) -> Self {
        Self {
            status: 200,
            headers: Self::default_headers("text/plain; charset=utf-8"),
            body: body.as_bytes().to_vec(),
        }
    }

    /// 200 OK with JSON body.
    pub fn json(body: &str) -> Self {
        Self {
            status: 200,
            headers: Self::default_headers("application/json; charset=utf-8"),
            body: body.as_bytes().to_vec(),
        }
    }

    /// 200 OK with HTML body.
    pub fn html(body: &str) -> Self {
        Self {
            status: 200,
            headers: Self::default_headers("text/html; charset=utf-8"),
            body: body.as_bytes().to_vec(),
        }
    }

    /// Create a response with a custom status code.
    pub fn status(code: u16, body: &str) -> Self {
        Self {
            status: code,
            headers: Self::default_headers("text/plain; charset=utf-8"),
            body: body.as_bytes().to_vec(),
        }
    }

    /// 404 Not Found.
    pub fn not_found() -> Self {
        Self::status(404, "Not Found")
    }

    /// 500 Internal Server Error.
    pub fn internal_error() -> Self {
        Self::status(500, "Internal Server Error")
    }

    /// 301 redirect.
    pub fn redirect(location: &str) -> Self {
        let mut resp = Self::status(301, "");
        resp.headers.insert("Location".to_string(), location.to_string());
        resp
    }

    /// Builder: add a header.
    pub fn header(mut self, key: &str, value: &str) -> Self {
        self.headers.insert(key.to_string(), value.to_string());
        self
    }

    /// Builder: set a cookie.
    pub fn cookie(mut self, name: &str, value: &str, max_age: Option<u64>) -> Self {
        let mut cookie_str = format!("{}={}; Path=/; HttpOnly", name, value);
        if let Some(age) = max_age {
            cookie_str.push_str(&format!("; Max-Age={}", age));
        }
        self.headers.insert("Set-Cookie".to_string(), cookie_str);
        self
    }

    /// Serialize response to HTTP/1.1 wire format.
    fn to_bytes(&self) -> Vec<u8> {
        let status_text = status_reason(self.status);
        let mut buf = Vec::with_capacity(256 + self.body.len());

        // Status line
        buf.extend_from_slice(
            format!("HTTP/1.1 {} {}\r\n", self.status, status_text).as_bytes(),
        );
        // Content-Length (always include for correctness)
        buf.extend_from_slice(
            format!("Content-Length: {}\r\n", self.body.len()).as_bytes(),
        );
        // Headers
        for (key, value) in &self.headers {
            buf.extend_from_slice(format!("{}: {}\r\n", key, value).as_bytes());
        }
        // End of headers
        buf.extend_from_slice(b"\r\n");
        // Body
        buf.extend_from_slice(&self.body);
        buf
    }

    fn default_headers(content_type: &str) -> HashMap<String, String> {
        let mut h = HashMap::with_capacity(4);
        h.insert("Content-Type".to_string(), content_type.to_string());
        h.insert("Connection".to_string(), "keep-alive".to_string());
        h
    }
}

// --- Middleware --------------------------------------------------------------

/// Middleware function signature: takes request + next handler, returns response.
/// Middlewares can short-circuit by returning early without calling `next`.
pub type MiddlewareFn = fn(&Request, &dyn Fn(&Request) -> Response) -> Response;

/// CORS configuration.
#[derive(Debug, Clone)]
pub struct CorsConfig {
    pub allow_origins: Vec<String>,
    pub allow_methods: Vec<String>,
    pub allow_headers: Vec<String>,
    pub max_age: u64,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allow_origins: vec!["*".to_string()],
            allow_methods: vec![
                "GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            allow_headers: vec!["Content-Type", "Authorization"]
                .into_iter()
                .map(String::from)
                .collect(),
            max_age: 86400,
        }
    }
}

// --- FastServer --------------------------------------------------------------

/// Type alias for the handler dispatch function to keep clippy happy.
pub type DispatchFn = dyn Fn(&str, &Request) -> Response + Send + Sync;

/// The main high-performance HTTP server.
/// Manages thread pool, route table, middleware stack, and static file config.
pub struct FastServer {
    pub routes: Vec<Route>,
    middlewares: Vec<MiddlewareFn>,
    static_dir: Option<String>,
    cors: Option<CorsConfig>,
    /// When enabled, a GET that matches neither a route nor an existing static
    /// file serves `index.html` from the web root with status 200 (SPA routing,
    /// R2.1). When disabled, such requests return 404 (R2.2). Default: false.
    spa_fallback: bool,
    /// The handler callback that the Ran runtime provides.
    /// Maps handler_name -> actual function.
    handler_dispatch: Option<Arc<DispatchFn>>,
}

impl FastServer {
    pub fn new() -> Self {
        Self {
            routes: Vec::new(),
            middlewares: Vec::new(),
            static_dir: None,
            cors: None,
            spa_fallback: false,
            handler_dispatch: None,
        }
    }

    /// Register a route.
    pub fn route(mut self, method: HttpMethod, path: &str, handler_name: &str) -> Self {
        self.routes.push(Route {
            method,
            path: path.to_string(),
            handler_name: handler_name.to_string(),
        });
        self
    }

    /// Add middleware to the pipeline.
    pub fn middleware(mut self, mw: MiddlewareFn) -> Self {
        self.middlewares.push(mw);
        self
    }

    /// Enable static file serving from a directory.
    pub fn static_files(mut self, dir: &str) -> Self {
        self.static_dir = Some(dir.to_string());
        self
    }

    /// Builder: enable/disable SPA fallback (serve `index.html` for unmatched
    /// GET requests). Returns `self` for chaining (R2.1/R2.2).
    pub fn spa(mut self, enabled: bool) -> Self {
        self.spa_fallback = enabled;
        self
    }

    /// Toggle SPA fallback in place. The runtime `web` module uses this to
    /// reflect `web.spa(bool)` from Ran programs.
    pub fn set_spa_fallback(&mut self, enabled: bool) {
        self.spa_fallback = enabled;
    }

    /// Whether SPA fallback is currently enabled.
    pub fn spa_fallback_enabled(&self) -> bool {
        self.spa_fallback
    }

    /// Enable CORS with given config.
    pub fn cors(mut self, config: CorsConfig) -> Self {
        self.cors = Some(config);
        self
    }

    /// Set the handler dispatch function (called by Ran runtime).
    pub fn set_dispatch(
        mut self,
        dispatch: Arc<DispatchFn>,
    ) -> Self {
        self.handler_dispatch = Some(dispatch);
        self
    }

    /// Start the server - spawns a bounded worker pool and blocks.
    pub fn listen(self, host: &str, port: u16) -> std::io::Result<()> {
        let addr = format!("{}:{}", host, port);
        let listener = TcpListener::bind(&addr)?;
        let workers = worker_count();
        println!("[ran] FastServer listening on http://{}", addr);
        println!("[ran]    workers: {}, keep-alive: {}s", workers, KEEP_ALIVE_TIMEOUT_SECS);
        {
            use crate::support::sysinfo as si;
            println!(
                "[ran]    host: {} {} · {} CPU · RAM {} free / {} total",
                si::os_name(), si::arch(), si::cpu_count(),
                si::human_bytes(si::mem_available()), si::human_bytes(si::mem_total()),
            );
            println!(
                "[ran]    memory budget: {} (reserving {} for the OS)",
                si::human_bytes(si::memory_budget_bytes()),
                si::human_bytes(si::os_reserve_bytes()),
            );
        }

        let server = Arc::new(self);

        // Bounded worker pool with a bounded queue. Connections are accepted on
        // the main thread and dispatched to a fixed set of workers via a shared
        // queue. The fixed pool caps thread count (no unbounded-spawn DoS); the
        // bounded queue applies backpressure (send blocks when full, so excess
        // connections wait in the OS accept backlog instead of growing memory).
        let queue_cap = {
            // Bound the backlog by both workers and available memory so a flood
            // of queued connections can't push the machine into OOM. Each queued
            // connection is estimated at one read buffer (64 KiB).
            let by_workers = workers.saturating_mul(64);
            let budget = crate::support::sysinfo::memory_budget_bytes();
            let by_memory = if budget == 0 {
                usize::MAX
            } else {
                (budget / READ_BUFFER_SIZE as u64) as usize
            };
            by_workers.min(by_memory).max(256)
        };
        let (tx, rx) = mpsc::sync_channel::<TcpStream>(queue_cap);
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..workers {
            let rx = Arc::clone(&rx);
            let server = Arc::clone(&server);
            let _ = thread::Builder::new()
                .name(format!("ran-worker-{}", i))
                .spawn(move || loop {
                    // Hold the lock only while dequeueing, not while serving.
                    let next = {
                        match rx.lock() {
                            Ok(guard) => guard.recv(),
                            Err(_) => break,
                        }
                    };
                    match next {
                        Ok(stream) => {
                            if let Err(e) = handle_connection(stream, &server) {
                                eprintln!("[ran] connection error: {}", e);
                            }
                        }
                        Err(_) => break, // listener closed, queue drained
                    }
                });
        }

        // Accept loop: enqueue connections for the workers.
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if tx.send(stream).is_err() {
                        break; // all workers gone
                    }
                }
                Err(e) => {
                    eprintln!("[ran] accept error: {}", e);
                }
            }
        }
        Ok(())
    }
}

// --- Public Entry Point ------------------------------------------------------

/// Blocking serve function - the simplest way to start the server.
/// Called by the Ran runtime with a route table and optional static dir.
pub fn serve(port: u16, routes: Vec<Route>, static_dir: Option<String>) {
    let mut server = FastServer::new();
    server.routes = routes;
    server.static_dir = static_dir;
    server.cors = Some(CorsConfig::default());

    if let Err(e) = server.listen("0.0.0.0", port) {
        eprintln!("[ran] fatal: failed to start server: {}", e);
    }
}

// --- Connection Handler ------------------------------------------------------

/// Handle a single TCP connection with keep-alive support.
/// Re-uses a pre-allocated buffer across requests on the same connection.
fn handle_connection(stream: TcpStream, server: &FastServer) -> std::io::Result<()> {
    // Set socket timeouts for keep-alive
    stream.set_read_timeout(Some(Duration::from_secs(KEEP_ALIVE_TIMEOUT_SECS)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.set_nodelay(true)?;

    let mut reader = BufReader::with_capacity(READ_BUFFER_SIZE, stream.try_clone()?);
    let mut writer = stream;

    let mut request_count: u32 = 0;

    // Keep-alive loop: process multiple requests per connection
    loop {
        request_count += 1;
        if request_count > MAX_KEEP_ALIVE_REQUESTS {
            break;
        }

        // Parse the request from the buffered reader
        let request = match parse_request(&mut reader) {
            Ok(Some(req)) => req,
            Ok(None) => break, // Client closed connection
            Err(_) => {
                // Malformed request - send 400 and close
                let resp = Response::status(400, "Bad Request");
                let _ = writer.write_all(&resp.to_bytes());
                break;
            }
        };

        // Check Connection header for keep-alive
        let keep_alive = request
            .header("connection")
            .map(|v| v.to_lowercase() != "close")
            .unwrap_or(true); // HTTP/1.1 defaults to keep-alive

        // Handle CORS preflight
        if request.method == HttpMethod::Options {
            if let Some(ref cors) = server.cors {
                let origin = request.header("origin").map(|s| s.to_string());
                let resp = build_cors_preflight(cors, origin.as_deref());
                writer.write_all(&resp.to_bytes())?;
                if !keep_alive { break; }
                continue;
            }
        }

        // Route the request
        let mut response = route_request(&request, server);

        // Apply CORS headers to all responses
        if let Some(ref cors) = server.cors {
            let origin = request.header("origin").map(|s| s.to_string());
            apply_cors_headers(&mut response, cors, origin.as_deref());
        }

        // Set keep-alive header
        if keep_alive {
            response.headers.insert(
                "Connection".to_string(),
                "keep-alive".to_string(),
            );
        } else {
            response.headers.insert(
                "Connection".to_string(),
                "close".to_string(),
            );
        }

        // Write response
        writer.write_all(&response.to_bytes())?;
        writer.flush()?;

        if !keep_alive {
            break;
        }
    }

    Ok(())
}

// --- Request Parser ----------------------------------------------------------

/// Parse an HTTP/1.1 request from a buffered reader.
/// Returns Ok(None) if the client closed the connection cleanly.
fn parse_request(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<Request>> {
    // Hard caps to prevent memory-exhaustion DoS from hostile clients.
    const MAX_REQUEST_LINE: u64 = 8 * 1024;
    const MAX_HEADER_LINE: u64 = 16 * 1024;
    const MAX_HEADERS: usize = 100;
    const MAX_HEADERS_TOTAL: usize = 64 * 1024;

    // Read the request line (bounded).
    let mut request_line = String::with_capacity(256);
    let bytes_read = reader
        .by_ref()
        .take(MAX_REQUEST_LINE)
        .read_line(&mut request_line)?;
    if bytes_read == 0 {
        return Ok(None); // Connection closed
    }
    if bytes_read as u64 >= MAX_REQUEST_LINE && !request_line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request line too long",
        ));
    }

    let request_line = request_line.trim_end();
    let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "malformed request line",
        ));
    }

    let method = HttpMethod::from_str(parts[0]).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "unknown HTTP method")
    })?;

    let raw_path = parts[1];

    // Split path and query string
    let (path, query_string) = match raw_path.find('?') {
        Some(idx) => (&raw_path[..idx], &raw_path[idx + 1..]),
        None => (raw_path, ""),
    };

    let query_params = parse_query_string(query_string);

    // Read headers (bounded by count and total bytes).
    let mut headers = HashMap::with_capacity(16);
    let mut header_line = String::with_capacity(128);
    let mut header_total: usize = 0;
    loop {
        header_line.clear();
        let n = reader
            .by_ref()
            .take(MAX_HEADER_LINE)
            .read_line(&mut header_line)?;
        if n == 0 {
            break; // connection closed mid-headers
        }
        if n as u64 >= MAX_HEADER_LINE && !header_line.ends_with('\n') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "header line too long",
            ));
        }
        let trimmed = header_line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        header_total += n;
        if headers.len() >= MAX_HEADERS || header_total > MAX_HEADERS_TOTAL {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "too many headers",
            ));
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            headers.insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    // Parse cookies from Cookie header
    let cookies = headers
        .get("Cookie")
        .or_else(|| headers.get("cookie"))
        .map(|c| parse_cookies(c))
        .unwrap_or_default();

    // Read body based on Content-Length
    let content_length: usize = headers
        .get("Content-Length")
        .or_else(|| headers.get("content-length"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let body = if content_length > 0 {
        if content_length > MAX_BODY_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request body too large",
            ));
        }
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body)?;
        body
    } else {
        Vec::new()
    };

    Ok(Some(Request {
        method,
        path: path.to_string(),
        headers,
        body,
        query_params,
        path_params: HashMap::new(), // Filled during routing
        cookies,
    }))
}

// --- Router ------------------------------------------------------------------

/// Route a request to the appropriate handler.
/// Supports path parameters like /users/:id and /posts/:id/comments/:cid.
fn route_request(request: &Request, server: &FastServer) -> Response {
    // Try static files first (if configured). A served file, a 304, or a 403
    // traversal rejection short-circuits; a missing file falls through so that
    // dynamic routes (and SPA fallback) get a chance to handle the request.
    if request.method == HttpMethod::Get {
        if let Some(ref dir) = server.static_dir {
            if let Some(resp) = serve_static(dir, &request.path, Some(&request.headers)) {
                return resp;
            }
        }
    }

    // Match against registered routes
    for route in &server.routes {
        if route.method != request.method {
            continue;
        }
        if let Some(params) = match_path(&route.path, &request.path) {
            // Clone request and inject path params
            let mut req = request.clone();
            req.path_params = params;

            // If we have a dispatch function, use it
            if let Some(ref dispatch) = server.handler_dispatch {
                return dispatch(&route.handler_name, &req);
            }

            // Default: return the handler name as a debug response
            return Response::ok(&format!(
                "handler: {} (no dispatch configured)",
                route.handler_name
            ));
        }
    }

    // SPA fallback (R2.1): a GET that matched neither a static file nor a route
    // serves `index.html` when enabled. Reaching this point means no route
    // matched (matches return above) and no static file existed.
    if request.method == HttpMethod::Get {
        if let Some(ref dir) = server.static_dir {
            if decide_spa_fallback(false, false, server.spa_fallback) {
                if let Some(resp) = serve_spa_index(dir, Some(&request.headers)) {
                    return resp;
                }
            }
        }
    }

    Response::not_found()
}

/// Match a route pattern against a request path.
/// Pattern: /users/:id/posts/:pid
/// Returns extracted path parameters or None if no match.
fn match_path(pattern: &str, path: &str) -> Option<HashMap<String, String>> {
    let pattern_parts: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let path_parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if pattern_parts.len() != path_parts.len() {
        return None;
    }

    let mut params = HashMap::new();

    for (pat, val) in pattern_parts.iter().zip(path_parts.iter()) {
        if let Some(param_name) = pat.strip_prefix(':') {
            // Path parameter - extract name without ':'
            params.insert(param_name.to_string(), val.to_string());
        } else if pat != val {
            return None;
        }
    }

    Some(params)
}

// --- Static File Server ------------------------------------------------------

/// Backward-compatible entry point used by the legacy server. Serves a static
/// file without conditional-request (304) handling.
fn try_serve_static(base_dir: &str, request_path: &str) -> Option<Response> {
    serve_static(base_dir, request_path, None)
}

/// Attempt to serve a static file. Returns `None` when the file does not exist
/// inside the web root (so the caller can fall through to routing / SPA / 404).
/// A served file, a `304 Not Modified`, and a `403 Forbidden` traversal
/// rejection are all returned as `Some(..)` and should short-circuit.
///
/// Hardened against directory traversal: the resolved path is canonicalized
/// and verified to remain inside the canonicalized base directory, which
/// defeats `..`, symlink, and percent-encoded escape attempts (R3.1/R3.2).
fn serve_static(
    base_dir: &str,
    request_path: &str,
    req_headers: Option<&HashMap<String, String>>,
) -> Option<Response> {
    // Decode percent-escapes so encoded traversal (e.g. %2e%2e) is also caught.
    let decoded = url_decode(request_path);

    // Quick reject of obvious traversal tokens before touching the filesystem.
    if decoded.contains("..") || decoded.contains('\0') {
        emit_web_diag(
            "E0403",
            request_path,
            format!("blocked path traversal attempt: {}", request_path),
        );
        return Some(Response::status(403, "Forbidden"));
    }

    // Normalize path: /foo -> base_dir/foo, / -> base_dir/index.html
    let relative = if decoded == "/" {
        "index.html"
    } else {
        decoded.trim_start_matches('/')
    };

    let base = match fs::canonicalize(base_dir) {
        Ok(b) => b,
        Err(_) => return None, // base dir does not exist
    };
    let candidate = base.join(relative);
    let resolved = match fs::canonicalize(&candidate) {
        Ok(r) => r,
        Err(_) => return None, // file does not exist
    };

    // Containment check: resolved path must be within the base directory.
    if !resolved.starts_with(&base) {
        emit_web_diag(
            "E0403",
            request_path,
            format!("path resolves outside the web root: {}", request_path),
        );
        return Some(Response::status(403, "Forbidden"));
    }

    if !resolved.is_file() {
        return None;
    }

    Some(build_static_response(&resolved, req_headers))
}

/// Serve `index.html` from the web root for SPA fallback (R2.1). Returns `None`
/// when there is no servable `index.html`, in which case the caller returns 404.
fn serve_spa_index(
    base_dir: &str,
    req_headers: Option<&HashMap<String, String>>,
) -> Option<Response> {
    let base = fs::canonicalize(base_dir).ok()?;
    let resolved = fs::canonicalize(base.join("index.html")).ok()?;
    if !resolved.starts_with(&base) || !resolved.is_file() {
        return None;
    }
    Some(build_static_response(&resolved, req_headers))
}

/// Build the HTTP response for a resolved static file: attaches cache
/// validators (ETag + Last-Modified, R4.1) and answers conditional requests
/// with `304 Not Modified` when a validator matches (R4.2).
fn build_static_response(
    path: &std::path::Path,
    req_headers: Option<&HashMap<String, String>>,
) -> Response {
    let (len, mtime_secs) = match fs::metadata(path) {
        Ok(m) => (m.len(), mtime_unix_secs(&m)),
        Err(_) => (0, 0),
    };
    let etag = compute_etag(len, mtime_secs);
    let last_modified = format_http_date(mtime_secs);

    // Conditional request → 304 Not Modified (no body).
    if let Some(h) = req_headers {
        let inm = header_value(h, "if-none-match");
        let ims = header_value(h, "if-modified-since");
        if decide_not_modified(inm, ims, &etag, mtime_secs) {
            let mut headers = HashMap::with_capacity(4);
            headers.insert("ETag".to_string(), etag);
            headers.insert("Last-Modified".to_string(), last_modified);
            headers.insert("Cache-Control".to_string(), "public, max-age=3600".to_string());
            headers.insert("Connection".to_string(), "keep-alive".to_string());
            return Response { status: 304, headers, body: Vec::new() };
        }
    }

    let path_str = path.to_string_lossy();
    let (mime, recognized) = detect_mime_ext(&path_str);
    if !recognized {
        emit_web_diag(
            "E0405",
            &path_str,
            format!("unknown asset extension, served as {}", mime),
        );
    }

    match fs::read(path) {
        Ok(contents) => {
            let mut headers = HashMap::with_capacity(5);
            headers.insert("Content-Type".to_string(), mime.to_string());
            headers.insert("Cache-Control".to_string(), "public, max-age=3600".to_string());
            headers.insert("Connection".to_string(), "keep-alive".to_string());
            headers.insert("ETag".to_string(), etag);
            headers.insert("Last-Modified".to_string(), last_modified);

            Response { status: 200, headers, body: contents }
        }
        Err(_) => {
            emit_web_diag(
                "E0402",
                &path_str,
                format!("failed to read asset file: {}", path_str),
            );
            Response::internal_error()
        }
    }
}

/// Decide whether a SPA `index.html` fallback should be served (R2.1/R2.2).
/// Pure decision: fallback only when enabled and the request matched neither a
/// route nor an existing static file.
fn decide_spa_fallback(route_match: bool, file_exists: bool, spa_enabled: bool) -> bool {
    spa_enabled && !route_match && !file_exists
}

/// Pure decision for conditional requests (R4.2). Returns `true` when the
/// response should be `304 Not Modified`. `If-None-Match` takes precedence over
/// `If-Modified-Since` (RFC 7232); a matching ETag (or `*`) yields 304, else an
/// `If-Modified-Since` at or after the file mtime yields 304.
fn decide_not_modified(
    if_none_match: Option<&str>,
    if_modified_since: Option<&str>,
    etag: &str,
    mtime_secs: u64,
) -> bool {
    if let Some(inm) = if_none_match {
        return etag_matches(inm, etag);
    }
    if let Some(ims) = if_modified_since {
        if let Some(since) = parse_http_date(ims) {
            return mtime_secs <= since;
        }
    }
    false
}

/// Compute a strong ETag from a file's length and mtime (seconds).
fn compute_etag(len: u64, mtime_secs: u64) -> String {
    format!("\"{:x}-{:x}\"", len, mtime_secs)
}

/// Match an `If-None-Match` header value against an ETag. Supports `*`, a
/// comma-separated list, and weak/strong (`W/`) prefixes (compared on the tag).
fn etag_matches(header: &str, etag: &str) -> bool {
    let h = header.trim();
    if h == "*" {
        return true;
    }
    let want = etag.trim().strip_prefix("W/").unwrap_or(etag.trim());
    h.split(',').any(|tok| {
        let tok = tok.trim();
        let tok = tok.strip_prefix("W/").unwrap_or(tok);
        tok == want
    })
}

/// Case-insensitive header lookup returning the value.
fn header_value<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    let lower = name.to_lowercase();
    headers
        .iter()
        .find(|(k, _)| k.to_lowercase() == lower)
        .map(|(_, v)| v.as_str())
}

/// Extract a file's modification time as whole seconds since the Unix epoch.
fn mtime_unix_secs(m: &fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Emit a project-standard Diagnostic (code `E####`, `file:line:col`, fix hint)
/// for a web-serving event. Severity/hint come from the diagnostic catalog.
fn emit_web_diag(code: &str, path: &str, message: String) {
    use crate::support::diagnostics::{Diagnostic, SourceLoc};
    Diagnostic::from_code(code, message)
        .with_loc(SourceLoc::new(path, 0, 0, 0))
        .emit("");
}

/// Detect MIME type from a file extension, returning `(content_type, recognized)`.
/// Unknown extensions map to `application/octet-stream` with `recognized=false`
/// so callers can emit the `E0405` warning (R1.3). The mapping is total over the
/// supported extensions in R1.2; text types carry `charset=utf-8`.
fn detect_mime_ext(path: &str) -> (&'static str, bool) {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let ct = match ext.as_str() {
        // R1.2 supported asset types
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "map" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        // Additional well-known types retained from the original table (not in
        // R1.2 but harmless and avoids regressing existing behavior).
        "ttf" => "font/ttf",
        "pdf" => "application/pdf",
        "xml" => "application/xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "avif" => "image/avif",
        _ => return ("application/octet-stream", false),
    };
    (ct, true)
}

/// Detect MIME type from file extension (content-type only).
#[allow(dead_code)]
fn detect_mime(path: &str) -> &'static str {
    detect_mime_ext(path).0
}

// --- HTTP date (RFC 1123) ----------------------------------------------------
// Std-only conversions between Unix seconds and the IMF-fixdate format used by
// `Last-Modified` / `If-Modified-Since`, via Howard Hinnant's civil-date algos.

/// Format whole seconds since the Unix epoch as an RFC 1123 / IMF-fixdate
/// string, e.g. `Wed, 21 Oct 2015 07:28:00 GMT`.
fn format_http_date(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as i64;
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // 1970-01-01 was a Thursday; with 0=Sunday that is index 4.
    let dow = (((days % 7) + 4) % 7 + 7) % 7;
    let (year, month, day) = civil_from_days(days);
    const WDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        WDAYS[dow as usize],
        day,
        MONTHS[(month - 1) as usize],
        year,
        hour,
        min,
        sec
    )
}

/// Parse an RFC 1123 / IMF-fixdate string into whole seconds since the Unix
/// epoch. Returns `None` on malformed input or pre-epoch dates.
fn parse_http_date(s: &str) -> Option<u64> {
    let s = s.trim();
    // Drop the leading weekday up to the comma when present.
    let rest = match s.find(',') {
        Some(i) => s[i + 1..].trim(),
        None => s,
    };
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let day: i64 = parts[0].parse().ok()?;
    let month = month_num(parts[1])?;
    let year: i64 = parts[2].parse().ok()?;
    let time: Vec<&str> = parts[3].split(':').collect();
    if time.len() != 3 {
        return None;
    }
    let h: i64 = time[0].parse().ok()?;
    let mi: i64 = time[1].parse().ok()?;
    let se: i64 = time[2].parse().ok()?;
    if !(0..=23).contains(&h) || !(0..=59).contains(&mi) || !(0..=60).contains(&se) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + h * 3600 + mi * 60 + se;
    if secs < 0 {
        None
    } else {
        Some(secs as u64)
    }
}

fn month_num(m: &str) -> Option<i64> {
    Some(match m {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    })
}

/// Convert days since the Unix epoch into a `(year, month, day)` civil date.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

/// Convert a `(year, month, day)` civil date into days since the Unix epoch.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

// --- CORS --------------------------------------------------------------------

/// Resolve a single valid value for the `Access-Control-Allow-Origin` header.
///
/// That header must be exactly one origin or `*` — a comma-separated list is
/// invalid and rejected by browsers. When a concrete allow-list is configured
/// we reflect the request's `Origin` only if it is permitted.
fn resolve_allowed_origin(cors: &CorsConfig, request_origin: Option<&str>) -> Option<String> {
    if cors.allow_origins.iter().any(|o| o == "*") {
        return Some("*".to_string());
    }
    if let Some(origin) = request_origin {
        if cors.allow_origins.iter().any(|a| a == origin) {
            return Some(origin.to_string());
        }
    }
    cors.allow_origins.first().cloned()
}

/// Build a CORS preflight (OPTIONS) response.
fn build_cors_preflight(cors: &CorsConfig, request_origin: Option<&str>) -> Response {
    let mut headers = HashMap::with_capacity(6);
    if let Some(origin) = resolve_allowed_origin(cors, request_origin) {
        headers.insert("Access-Control-Allow-Origin".to_string(), origin);
    }
    headers.insert("Vary".to_string(), "Origin".to_string());
    headers.insert(
        "Access-Control-Allow-Methods".to_string(),
        cors.allow_methods.join(", "),
    );
    headers.insert(
        "Access-Control-Allow-Headers".to_string(),
        cors.allow_headers.join(", "),
    );
    headers.insert(
        "Access-Control-Max-Age".to_string(),
        cors.max_age.to_string(),
    );
    headers.insert("Content-Length".to_string(), "0".to_string());

    Response {
        status: 204,
        headers,
        body: Vec::new(),
    }
}

/// Apply CORS headers to a regular response.
fn apply_cors_headers(response: &mut Response, cors: &CorsConfig, request_origin: Option<&str>) {
    if let Some(origin) = resolve_allowed_origin(cors, request_origin) {
        response
            .headers
            .insert("Access-Control-Allow-Origin".to_string(), origin);
    }
    response
        .headers
        .insert("Vary".to_string(), "Origin".to_string());
    response.headers.insert(
        "Access-Control-Allow-Methods".to_string(),
        cors.allow_methods.join(", "),
    );
}

// --- Utility Functions -------------------------------------------------------

/// Parse a query string like "foo=bar&baz=qux" into a HashMap.
fn parse_query_string(qs: &str) -> HashMap<String, String> {
    if qs.is_empty() {
        return HashMap::new();
    }
    qs.split('&')
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?;
            let value = parts.next().unwrap_or("");
            Some((
                url_decode(key),
                url_decode(value),
            ))
        })
        .collect()
}

/// Parse a Cookie header value into name=value pairs.
fn parse_cookies(header: &str) -> HashMap<String, String> {
    header
        .split(';')
        .filter_map(|pair| {
            let mut parts = pair.trim().splitn(2, '=');
            let name = parts.next()?.trim();
            let value = parts.next().unwrap_or("").trim();
            if name.is_empty() {
                None
            } else {
                Some((name.to_string(), value.to_string()))
            }
        })
        .collect()
}

/// Percent-decode a URL component into bytes, then interpret as UTF-8.
///
/// Decoding into a byte buffer first (rather than pushing each `%XX` as a
/// `char`) is required for correctness: multi-byte UTF-8 sequences are encoded
/// as several percent-escapes and must be reassembled at the byte level.
fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }

    // Invalid UTF-8 is replaced rather than dropped, so callers always get a String.
    String::from_utf8_lossy(&out).into_owned()
}

/// Map HTTP status codes to reason phrases.
fn status_reason(code: u16) -> &'static str {
    match code {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

// --- Legacy Compatibility ----------------------------------------------------
// These types maintain backward compatibility with existing Ran programs
// that use the old `Server` API.

/// Legacy handler type for direct function handlers.
pub type Handler = fn(&Request) -> Response;

/// Legacy server that wraps FastServer for simple use cases.
pub struct Server {
    host: String,
    port: u16,
    routes: Vec<(HttpMethod, String, Handler)>,
    static_dir: Option<String>,
}

impl Server {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            routes: Vec::new(),
            static_dir: None,
        }
    }

    pub fn get(&mut self, path: &str, handler: Handler) {
        self.routes.push((HttpMethod::Get, path.to_string(), handler));
    }

    pub fn post(&mut self, path: &str, handler: Handler) {
        self.routes.push((HttpMethod::Post, path.to_string(), handler));
    }

    pub fn put(&mut self, path: &str, handler: Handler) {
        self.routes.push((HttpMethod::Put, path.to_string(), handler));
    }

    pub fn delete(&mut self, path: &str, handler: Handler) {
        self.routes.push((HttpMethod::Delete, path.to_string(), handler));
    }

    pub fn static_files(&mut self, dir: &str) {
        self.static_dir = Some(dir.to_string());
    }

    /// Start the legacy server using the new FastServer engine.
    pub fn listen(&self) -> std::io::Result<()> {
        let addr = format!("{}:{}", self.host, self.port);
        let listener = TcpListener::bind(&addr)?;
        println!("[ran] Server listening on http://{}", addr);

        // Build a dispatch table from handlers
        let handlers: Arc<Vec<(HttpMethod, String, Handler)>> =
            Arc::new(self.routes.clone());
        let static_dir = self.static_dir.clone();

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let handlers = Arc::clone(&handlers);
                    let static_dir = static_dir.clone();
                    thread::spawn(move || {
                        if let Err(e) = handle_legacy_connection(stream, &handlers, &static_dir) {
                            eprintln!("[ran] connection error: {}", e);
                        }
                    });
                }
                Err(e) => eprintln!("[ran] accept error: {}", e),
            }
        }
        Ok(())
    }
}

/// Handle a legacy connection with keep-alive.
fn handle_legacy_connection(
    stream: TcpStream,
    handlers: &[(HttpMethod, String, Handler)],
    static_dir: &Option<String>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(KEEP_ALIVE_TIMEOUT_SECS)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.set_nodelay(true)?;

    let mut reader = BufReader::with_capacity(READ_BUFFER_SIZE, stream.try_clone()?);
    let mut writer = stream;
    let mut request_count: u32 = 0;

    loop {
        request_count += 1;
        if request_count > MAX_KEEP_ALIVE_REQUESTS {
            break;
        }

        let mut request = match parse_request(&mut reader) {
            Ok(Some(req)) => req,
            Ok(None) => break,
            Err(_) => {
                let resp = Response::status(400, "Bad Request");
                let _ = writer.write_all(&resp.to_bytes());
                break;
            }
        };

        let keep_alive = request
            .header("connection")
            .map(|v| v.to_lowercase() != "close")
            .unwrap_or(true);

        // Try static files
        let response = if request.method == HttpMethod::Get {
            if let Some(ref dir) = static_dir {
                if let Some(resp) = try_serve_static(dir, &request.path) {
                    resp
                } else {
                    route_legacy(&mut request, handlers)
                }
            } else {
                route_legacy(&mut request, handlers)
            }
        } else {
            route_legacy(&mut request, handlers)
        };

        writer.write_all(&response.to_bytes())?;
        writer.flush()?;

        if !keep_alive {
            break;
        }
    }
    Ok(())
}

/// Route a request using legacy handlers with path parameter support.
fn route_legacy(
    request: &mut Request,
    handlers: &[(HttpMethod, String, Handler)],
) -> Response {
    for (method, pattern, handler) in handlers {
        if *method != request.method {
            continue;
        }
        if let Some(params) = match_path(pattern, &request.path) {
            request.path_params = params;
            return handler(request);
        }
    }
    Response::not_found()
}

// --- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_path_exact() {
        let params = match_path("/users", "/users");
        assert_eq!(params, Some(HashMap::new()));
    }

    #[test]
    fn test_match_path_with_param() {
        let params = match_path("/users/:id", "/users/42").unwrap();
        assert_eq!(params.get("id").unwrap(), "42");
    }

    #[test]
    fn test_match_path_multiple_params() {
        let params = match_path("/users/:uid/posts/:pid", "/users/7/posts/99").unwrap();
        assert_eq!(params.get("uid").unwrap(), "7");
        assert_eq!(params.get("pid").unwrap(), "99");
    }

    #[test]
    fn test_match_path_no_match() {
        assert_eq!(match_path("/users/:id", "/posts/42"), None);
        assert_eq!(match_path("/users/:id", "/users/42/extra"), None);
    }

    #[test]
    fn test_parse_query_string() {
        let qs = parse_query_string("name=ran&version=1.0&empty=");
        assert_eq!(qs.get("name").unwrap(), "ran");
        assert_eq!(qs.get("version").unwrap(), "1.0");
        assert_eq!(qs.get("empty").unwrap(), "");
    }

    #[test]
    fn test_parse_query_string_encoded() {
        let qs = parse_query_string("msg=hello+world&path=%2Ffoo%2Fbar");
        assert_eq!(qs.get("msg").unwrap(), "hello world");
        assert_eq!(qs.get("path").unwrap(), "/foo/bar");
    }

    #[test]
    fn test_parse_cookies() {
        let cookies = parse_cookies("session=abc123; theme=dark; lang=en");
        assert_eq!(cookies.get("session").unwrap(), "abc123");
        assert_eq!(cookies.get("theme").unwrap(), "dark");
        assert_eq!(cookies.get("lang").unwrap(), "en");
    }

    #[test]
    fn test_url_decode() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("foo+bar"), "foo bar");
        assert_eq!(url_decode("%2F"), "/");
    }

    #[test]
    fn test_detect_mime() {
        // R1.2 mapping. Note `.js` is `text/javascript` per the spec.
        assert_eq!(detect_mime("style.css"), "text/css; charset=utf-8");
        assert_eq!(detect_mime("app.js"), "text/javascript; charset=utf-8");
        assert_eq!(detect_mime("module.mjs"), "text/javascript; charset=utf-8");
        assert_eq!(detect_mime("index.html"), "text/html; charset=utf-8");
        assert_eq!(detect_mime("page.htm"), "text/html; charset=utf-8");
        assert_eq!(detect_mime("data.json"), "application/json; charset=utf-8");
        assert_eq!(detect_mime("photo.png"), "image/png");
        assert_eq!(detect_mime("pic.jpg"), "image/jpeg");
        assert_eq!(detect_mime("pic.jpeg"), "image/jpeg");
        assert_eq!(detect_mime("anim.gif"), "image/gif");
        assert_eq!(detect_mime("icon.svg"), "image/svg+xml");
        assert_eq!(detect_mime("photo.webp"), "image/webp");
        assert_eq!(detect_mime("favicon.ico"), "image/x-icon");
        assert_eq!(detect_mime("mod.wasm"), "application/wasm");
        assert_eq!(detect_mime("font.woff"), "font/woff");
        assert_eq!(detect_mime("font.woff2"), "font/woff2");
        assert_eq!(detect_mime("bundle.js.map"), "application/json");
        assert_eq!(detect_mime("readme.txt"), "text/plain; charset=utf-8");
        // Case-insensitive extension handling.
        assert_eq!(detect_mime("PHOTO.PNG"), "image/png");
        // Unknown extension falls back to octet-stream (recognized == false).
        assert_eq!(detect_mime("data.bin"), "application/octet-stream");
        assert_eq!(detect_mime_ext("data.bin"), ("application/octet-stream", false));
        assert_eq!(detect_mime_ext("noext"), ("application/octet-stream", false));
        assert!(detect_mime_ext("a.css").1);
    }

    #[test]
    fn test_etag_stable_and_matches() {
        let etag = compute_etag(1234, 1_600_000_000);
        // Deterministic for the same (len, mtime).
        assert_eq!(etag, compute_etag(1234, 1_600_000_000));
        // Differs when either input changes.
        assert_ne!(etag, compute_etag(1235, 1_600_000_000));
        assert_ne!(etag, compute_etag(1234, 1_600_000_001));

        assert!(etag_matches(&etag, &etag));
        assert!(etag_matches("*", &etag));
        assert!(etag_matches(&format!("\"other\", {}", etag), &etag));
        assert!(etag_matches(&format!("W/{}", etag), &etag));
        assert!(!etag_matches("\"nope\"", &etag));
    }

    #[test]
    fn test_decide_not_modified() {
        let etag = compute_etag(10, 1_600_000_000);
        // If-None-Match matches → 304.
        assert!(decide_not_modified(Some(&etag), None, &etag, 1_600_000_000));
        // If-None-Match present but mismatched → not 304 (precedence over IMS).
        assert!(!decide_not_modified(
            Some("\"x\""),
            Some("Sat, 01 Jan 2050 00:00:00 GMT"),
            &etag,
            1_600_000_000
        ));
        // If-Modified-Since at/after mtime → 304.
        assert!(decide_not_modified(
            None,
            Some("Sat, 01 Jan 2050 00:00:00 GMT"),
            &etag,
            1_600_000_000
        ));
        // If-Modified-Since before mtime → modified, not 304.
        assert!(!decide_not_modified(
            None,
            Some("Thu, 01 Jan 1970 00:00:00 GMT"),
            &etag,
            1_600_000_000
        ));
        // No validators → always serve fresh.
        assert!(!decide_not_modified(None, None, &etag, 1_600_000_000));
        // Unparseable date → not 304.
        assert!(!decide_not_modified(None, Some("not-a-date"), &etag, 1_600_000_000));
    }

    #[test]
    fn test_http_date_roundtrip() {
        // Known epoch landmark.
        assert_eq!(format_http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        // Well-known RFC 7232 example timestamp.
        assert_eq!(format_http_date(1_445_412_480), "Wed, 21 Oct 2015 07:28:00 GMT");
        assert_eq!(parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT"), Some(1_445_412_480));
        // Round-trip a spread of timestamps.
        for &t in &[0u64, 1, 86_400, 1_000_000_000, 1_600_000_000, 2_000_000_000] {
            let s = format_http_date(t);
            assert_eq!(parse_http_date(&s), Some(t), "roundtrip failed for {t} -> {s}");
        }
        // Tolerates missing weekday prefix.
        assert_eq!(parse_http_date("21 Oct 2015 07:28:00 GMT"), Some(1_445_412_480));
        // Rejects malformed input.
        assert_eq!(parse_http_date("garbage"), None);
        assert_eq!(parse_http_date("Wed, 21 Foo 2015 07:28:00 GMT"), None);
    }

    #[test]
    fn test_decide_spa_fallback() {
        // Serve index.html only when enabled and nothing else matched.
        assert!(decide_spa_fallback(false, false, true));
        // Disabled → never fall back (→ 404).
        assert!(!decide_spa_fallback(false, false, false));
        // A matching route wins over SPA fallback.
        assert!(!decide_spa_fallback(true, false, true));
        // An existing static file wins over SPA fallback.
        assert!(!decide_spa_fallback(false, true, true));
    }

    #[test]
    fn test_serve_static_traversal_rejected_403() {
        let dir = ".tmp_tests/net_traversal";
        let _ = fs::create_dir_all(dir);
        fs::write(format!("{}/index.html", dir), b"<h1>hi</h1>").unwrap();

        // `..` traversal is rejected with 403, never reading outside the root.
        let resp = serve_static(dir, "/../Cargo.toml", None).expect("403 expected");
        assert_eq!(resp.status, 403);
        // Percent-encoded traversal is also caught.
        let resp = serve_static(dir, "/%2e%2e/Cargo.toml", None).expect("403 expected");
        assert_eq!(resp.status, 403);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_serve_static_missing_is_none() {
        let dir = ".tmp_tests/net_missing";
        let _ = fs::create_dir_all(dir);
        fs::write(format!("{}/index.html", dir), b"x").unwrap();
        // A non-existent file inside the root yields None (caller → 404/SPA).
        assert!(serve_static(dir, "/nope.html", None).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_serve_static_serves_with_validators() {
        let dir = ".tmp_tests/net_serve";
        let _ = fs::create_dir_all(dir);
        fs::write(format!("{}/app.css", dir), b"body{}").unwrap();

        let resp = serve_static(dir, "/app.css", None).expect("file should serve");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.headers.get("Content-Type").unwrap(), "text/css; charset=utf-8");
        assert!(resp.headers.contains_key("ETag"));
        assert!(resp.headers.contains_key("Last-Modified"));
        assert_eq!(resp.body, b"body{}");

        // Conditional GET with the served ETag returns 304 with no body.
        let etag = resp.headers.get("ETag").unwrap().clone();
        let mut headers = HashMap::new();
        headers.insert("If-None-Match".to_string(), etag);
        let cond = serve_static(dir, "/app.css", Some(&headers)).expect("conditional");
        assert_eq!(cond.status, 304);
        assert!(cond.body.is_empty());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_serve_spa_index_fallback() {
        let dir = ".tmp_tests/net_spa";
        let _ = fs::create_dir_all(dir);
        fs::write(format!("{}/index.html", dir), b"<div id=app></div>").unwrap();

        let resp = serve_spa_index(dir, None).expect("index served");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.headers.get("Content-Type").unwrap(), "text/html; charset=utf-8");
        assert_eq!(resp.body, b"<div id=app></div>");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_response_builder() {
        let resp = Response::ok("hello")
            .header("X-Custom", "test")
            .cookie("session", "abc", Some(3600));

        assert_eq!(resp.status, 200);
        assert_eq!(resp.headers.get("X-Custom").unwrap(), "test");
        assert!(resp.headers.get("Set-Cookie").unwrap().contains("session=abc"));
        assert_eq!(resp.body, b"hello");
    }

    #[test]
    fn test_response_json() {
        let resp = Response::json(r#"{"ok":true}"#);
        assert_eq!(resp.status, 200);
        assert!(resp.headers.get("Content-Type").unwrap().contains("application/json"));
    }

    #[test]
    fn test_http_method_parse() {
        assert_eq!(HttpMethod::from_str("GET"), Some(HttpMethod::Get));
        assert_eq!(HttpMethod::from_str("POST"), Some(HttpMethod::Post));
        assert_eq!(HttpMethod::from_str("INVALID"), None);
    }

    // --- Task 2.5: web unit + integration coverage ---------------------------
    //
    // These exercise the static-serving decision path end-to-end at the helper
    // level (no socket needed) plus one live loopback integration test. Temp
    // Web_Root trees live under `.tmp_tests/` (gitignored) and are cleaned up.

    /// Build a bare GET request for `path` with no validator headers.
    fn get_req(path: &str) -> Request {
        Request {
            method: HttpMethod::Get,
            path: path.to_string(),
            headers: HashMap::new(),
            body: Vec::new(),
            query_params: HashMap::new(),
            path_params: HashMap::new(),
            cookies: HashMap::new(),
        }
    }

    /// An unknown extension is still served (status 200) with the
    /// `application/octet-stream` Content-Type — the request is never failed
    /// (R1.3 / E0405 is a warning, not an error).
    #[test]
    fn test_serve_unknown_extension_is_octet_stream() {
        let dir = ".tmp_tests/net_unknown_ext";
        let _ = fs::create_dir_all(dir);
        fs::write(format!("{}/data.bin", dir), b"\x00\x01\x02ran").unwrap();

        let resp = serve_static(dir, "/data.bin", None).expect("file should serve");
        assert_eq!(resp.status, 200);
        assert_eq!(
            resp.headers.get("Content-Type").unwrap(),
            "application/octet-stream"
        );
        assert_eq!(resp.body, b"\x00\x01\x02ran");

        let _ = fs::remove_dir_all(dir);
    }

    /// `route_request`: a GET for a missing file with SPA disabled returns 404.
    #[test]
    fn test_route_request_404_when_missing_no_spa() {
        let dir = ".tmp_tests/net_route_404";
        let _ = fs::create_dir_all(dir);
        fs::write(format!("{}/index.html", dir), b"<h1>home</h1>").unwrap();

        let server = FastServer::new().static_files(dir).spa(false);
        let resp = route_request(&get_req("/does-not-exist"), &server);
        assert_eq!(resp.status, 404);

        let _ = fs::remove_dir_all(dir);
    }

    /// `route_request`: a GET for an unmatched path with SPA enabled serves
    /// `index.html` with status 200 (client-side routing, R2.1).
    #[test]
    fn test_route_request_spa_fallback_serves_index() {
        let dir = ".tmp_tests/net_route_spa";
        let _ = fs::create_dir_all(dir);
        fs::write(format!("{}/index.html", dir), b"<div id=app></div>").unwrap();

        let server = FastServer::new().static_files(dir).spa(true);
        let resp = route_request(&get_req("/app/route/deep"), &server);
        assert_eq!(resp.status, 200);
        assert_eq!(
            resp.headers.get("Content-Type").unwrap(),
            "text/html; charset=utf-8"
        );
        assert_eq!(resp.body, b"<div id=app></div>");

        let _ = fs::remove_dir_all(dir);
    }

    /// `route_request`: a traversal attempt is rejected with 403 even when SPA
    /// is enabled (the traversal guard short-circuits before SPA fallback).
    #[test]
    fn test_route_request_traversal_403() {
        let dir = ".tmp_tests/net_route_traversal";
        let _ = fs::create_dir_all(dir);
        fs::write(format!("{}/index.html", dir), b"<h1>home</h1>").unwrap();

        let server = FastServer::new().static_files(dir).spa(true);
        let resp = route_request(&get_req("/../Cargo.toml"), &server);
        assert_eq!(resp.status, 403);

        let _ = fs::remove_dir_all(dir);
    }

    /// Live integration: bind an ephemeral loopback port, serve one connection
    /// through the real connection handler, and assert the wire response. This
    /// covers the full parse → route → static-serve → write path. Robust: uses
    /// `127.0.0.1:0`, client read timeout, and `Connection: close` so the
    /// handler returns promptly.
    #[test]
    fn test_live_loopback_serves_static_file() {
        let dir = ".tmp_tests/net_live_serve";
        let _ = fs::create_dir_all(dir);
        fs::write(format!("{}/index.html", dir), b"<h1>live</h1>").unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let addr = listener.local_addr().expect("local addr");

        let server = Arc::new(FastServer::new().static_files(dir));
        let server_thread = thread::spawn(move || {
            // Accept exactly one connection, serve it, then return.
            if let Ok((stream, _)) = listener.accept() {
                let _ = handle_connection(stream, &server);
            }
        });

        let mut client = TcpStream::connect(addr).expect("connect");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client
            .write_all(b"GET /index.html HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("write request");

        let mut raw = Vec::new();
        client.read_to_end(&mut raw).expect("read response");
        let _ = server_thread.join();

        let text = String::from_utf8_lossy(&raw);
        assert!(text.starts_with("HTTP/1.1 200"), "response: {}", text);
        assert!(
            text.contains("Content-Type: text/html; charset=utf-8"),
            "response: {}",
            text
        );
        assert!(text.contains("ETag:"), "missing ETag: {}", text);
        assert!(text.contains("<h1>live</h1>"), "body missing: {}", text);

        let _ = fs::remove_dir_all(dir);
    }

    /// Live integration: a missing file with SPA disabled returns a 404 over
    /// the wire (the default `FastServer` has SPA off).
    #[test]
    fn test_live_loopback_missing_returns_404() {
        let dir = ".tmp_tests/net_live_404";
        let _ = fs::create_dir_all(dir);
        fs::write(format!("{}/index.html", dir), b"<h1>home</h1>").unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let addr = listener.local_addr().expect("local addr");

        let server = Arc::new(FastServer::new().static_files(dir));
        let server_thread = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let _ = handle_connection(stream, &server);
            }
        });

        let mut client = TcpStream::connect(addr).expect("connect");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client
            .write_all(b"GET /missing.html HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("write request");

        let mut raw = Vec::new();
        client.read_to_end(&mut raw).expect("read response");
        let _ = server_thread.join();

        let text = String::from_utf8_lossy(&raw);
        assert!(text.starts_with("HTTP/1.1 404"), "response: {}", text);

        let _ = fs::remove_dir_all(dir);
    }
}

// --- HTTP Client -------------------------------------------------------------
//
// HTTP/1.1 client over std TcpStream, with TLS (`https://`) provided by the
// system OpenSSL (see support/tls.rs). Certificate + hostname verification are
// enforced for https.

/// Result of an HTTP client request.
pub struct ClientResponse {
    pub status: u16,
    pub body: String,
    pub error: Option<String>,
}

fn client_err(msg: String) -> ClientResponse {
    ClientResponse { status: 0, body: String::new(), error: Some(msg) }
}

/// Perform a blocking HTTP/1.1 request. Supports `http://` and `https://`.
///
/// For https, the server certificate and hostname are verified against the
/// system trust store; an invalid certificate fails the request.
pub fn http_request(method: &str, url: &str, body: &str) -> ClientResponse {
    let timeout = Duration::from_secs(10);

    let (is_tls, rest, default_port) = if let Some(r) = url.strip_prefix("https://") {
        (true, r, 443u16)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r, 80u16)
    } else {
        return client_err(format!("invalid URL (expected http:// or https://): {}", url));
    };

    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match host_port.rfind(':') {
        Some(i) => (&host_port[..i], host_port[i + 1..].parse::<u16>().unwrap_or(default_port)),
        None => (host_port, default_port),
    };

    // Build the request bytes once; reuse for either transport.
    let mut req = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: ran/0.1\r\nAccept: */*\r\nConnection: close\r\n",
        method.to_uppercase(),
        path,
        host
    );
    if !body.is_empty() {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
        req.push_str("Content-Type: application/x-www-form-urlencoded\r\n");
    }
    req.push_str("\r\n");
    req.push_str(body);

    // Connect with the right transport and run the exchange.
    let raw_result: std::io::Result<Vec<u8>> = if is_tls {
        match crate::support::tls::TlsStream::connect(host, port, timeout) {
            Ok(mut s) => http_exchange(&mut s, req.as_bytes()),
            Err(e) => return client_err(format!("tls connect error: {}", e)),
        }
    } else {
        use std::net::ToSocketAddrs;
        let addr = match (host, port).to_socket_addrs().ok().and_then(|mut it| it.next()) {
            Some(a) => a,
            None => return client_err(format!("could not resolve host: {}", host)),
        };
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(mut s) => {
                let _ = s.set_read_timeout(Some(timeout));
                let _ = s.set_write_timeout(Some(timeout));
                http_exchange(&mut s, req.as_bytes())
            }
            Err(e) => return client_err(format!("connect error: {}", e)),
        }
    };

    let raw = match raw_result {
        Ok(r) => r,
        Err(e) => return client_err(format!("io error: {}", e)),
    };

    // Split headers/body at the first CRLF CRLF.
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n");
    let (head, body_bytes) = match split {
        Some(i) => (&raw[..i], &raw[i + 4..]),
        None => (&raw[..], &[][..]),
    };
    let head_str = String::from_utf8_lossy(head);
    let status = head_str
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);

    ClientResponse {
        status,
        body: String::from_utf8_lossy(body_bytes).to_string(),
        error: None,
    }
}

/// Write the request and read the full response from any Read+Write transport.
/// The response is capped to prevent a hostile server exhausting client memory.
fn http_exchange<S: Read + Write>(stream: &mut S, request: &[u8]) -> std::io::Result<Vec<u8>> {
    const MAX_RESPONSE: u64 = 64 * 1024 * 1024; // 64 MB
    stream.write_all(request)?;
    stream.flush()?;
    let mut raw = Vec::new();
    stream.take(MAX_RESPONSE).read_to_end(&mut raw)?;
    Ok(raw)
}

// --- Property test P1 --------------------------------------------------------

#[cfg(test)]
mod web_serving_property {
    // Feature: enterprise-runtime-capabilities, Property 1: Integritas & keamanan penyajian aset web
    //
    // Validates: Requirements 1, 2, 3
    //
    // Oracle (three clauses checked for every generated case):
    //   (a) Security/containment: for ANY request path, the static-serving
    //       resolver either serves bytes that belong to a file INSIDE the web
    //       root, or rejects (403) / reports absent (None). It NEVER serves the
    //       bytes of a file located outside the root (no traversal escape).
    //   (b) Integrity: when a known in-root path is requested, the served bytes
    //       equal the bytes on disk exactly.
    //   (c) Asset_Type totality: the MIME mapping is total — every supported
    //       extension yields its documented Content-Type, and every unsupported
    //       extension yields `application/octet-stream`.
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};
    use std::collections::HashSet;

    /// Documented `extension -> Content-Type` mapping (must mirror
    /// `detect_mime_ext`). Used to assert MIME totality over supported types.
    const SUPPORTED: &[(&str, &str)] = &[
        ("html", "text/html; charset=utf-8"),
        ("htm", "text/html; charset=utf-8"),
        ("css", "text/css; charset=utf-8"),
        ("js", "text/javascript; charset=utf-8"),
        ("mjs", "text/javascript; charset=utf-8"),
        ("json", "application/json; charset=utf-8"),
        ("svg", "image/svg+xml"),
        ("png", "image/png"),
        ("jpg", "image/jpeg"),
        ("jpeg", "image/jpeg"),
        ("webp", "image/webp"),
        ("gif", "image/gif"),
        ("ico", "image/x-icon"),
        ("wasm", "application/wasm"),
        ("woff", "font/woff"),
        ("woff2", "font/woff2"),
        ("map", "application/json"),
        ("txt", "text/plain; charset=utf-8"),
        ("ttf", "font/ttf"),
        ("pdf", "application/pdf"),
        ("xml", "application/xml"),
        ("mp4", "video/mp4"),
        ("webm", "video/webm"),
        ("avif", "image/avif"),
    ];

    fn is_supported_ext(s: &str) -> bool {
        SUPPORTED.iter().any(|(e, _)| e.eq_ignore_ascii_case(s))
    }

    /// One generated scenario: a request path (possibly adversarial) plus a file
    /// extension to exercise the Asset_Type mapping.
    #[derive(Clone, Debug)]
    struct WebCase {
        /// Raw request path string handed to the resolver.
        req_path: String,
        /// `Some(bytes)` when `req_path` is a safe path to a known in-root file
        /// (then it MUST be served with exactly these bytes); `None` otherwise.
        safe_target: Option<Vec<u8>>,
        /// Generated extension (without the dot) for the MIME totality check.
        ext: String,
        /// `Some(content_type)` if `ext` is a supported type, else `None`.
        ext_supported: Option<&'static str>,
    }

    /// Build the case generator. Captures the set of in-root relative paths (with
    /// their on-disk bytes) and the name of a secret file placed OUTSIDE the root,
    /// so the generator can synthesize both legitimate and adversarial requests.
    fn case_gen(rel_targets: Vec<(String, Vec<u8>)>, secret_name: String) -> Gen<WebCase> {
        Gen::new(
            move |rng: &mut Rng, _size: usize| {
                // ---- MIME part: pick a supported ext or an unknown one. ----
                let (ext, ext_supported) = if rng.boolean() {
                    let (e, ct) = SUPPORTED[rng.below(SUPPORTED.len() as u64) as usize];
                    (e.to_string(), Some(ct))
                } else {
                    // Random lowercase token that is NOT a supported extension.
                    loop {
                        let len = 1 + rng.upto(5);
                        let mut s = String::new();
                        for _ in 0..len {
                            s.push((b'a' + rng.below(26) as u8) as char);
                        }
                        if !is_supported_ext(&s) {
                            break (s, None);
                        }
                    }
                };

                // ---- Path part: safe / adversarial / junk. ----
                let (req_path, safe_target) = match rng.below(3) {
                    0 => {
                        // Safe request to an existing in-root file.
                        let (rel, bytes) = rng
                            .choose(&rel_targets)
                            .clone();
                        let p = if rng.boolean() {
                            format!("/{}", rel)
                        } else {
                            rel
                        };
                        (p, Some(bytes))
                    }
                    1 => {
                        // Adversarial traversal / absolute / encoded escapes that
                        // must NEVER reach the secret file outside the root.
                        let s = &secret_name;
                        let variants: Vec<String> = vec![
                            format!("../{}", s),
                            format!("/../{}", s),
                            format!("../../{}", s),
                            format!("%2e%2e/{}", s),
                            format!("..%2f{}", s),
                            format!("..\\{}", s),
                            format!("/%2e%2e/{}", s),
                            format!("....//{}", s),
                            format!("/{}{}", "../".repeat(1 + rng.upto(3)), s),
                            "/etc/passwd".to_string(),
                            "/../../../../etc/passwd".to_string(),
                            "\\..\\..\\windows\\system32".to_string(),
                        ];
                        let i = rng.below(variants.len() as u64) as usize;
                        (variants[i].clone(), None)
                    }
                    _ => {
                        // Random junk path (printable ascii + slashes), no `..`.
                        let len = rng.upto(12);
                        let mut p = String::from("/");
                        for _ in 0..len {
                            let c = match rng.below(4) {
                                0 => '/',
                                1 => (b'a' + rng.below(26) as u8) as char,
                                2 => (b'A' + rng.below(26) as u8) as char,
                                _ => (b'0' + rng.below(10) as u8) as char,
                            };
                            p.push(c);
                        }
                        (p, None)
                    }
                };

                WebCase {
                    req_path,
                    safe_target,
                    ext,
                    ext_supported,
                }
            },
            // Minimal shrink: drop to the bare request path with an empty ext.
            |c: &WebCase| {
                if c.ext.is_empty() && c.safe_target.is_none() {
                    Vec::new()
                } else {
                    vec![WebCase {
                        req_path: c.req_path.clone(),
                        safe_target: None,
                        ext: String::new(),
                        ext_supported: None,
                    }]
                }
            },
        )
    }

    #[test]
    fn prop_web_asset_serving_is_safe_and_total() {
        // ---- Build a temporary Web_Root tree under .tmp_tests/ (gitignored). ----
        let root = pbt::unique_tmp_path("web_root", "");
        fs::create_dir_all(&root).expect("create web root");

        let files: &[&str] = &[
            "index.html",
            "style.css",
            "app.js",
            "data.json",
            "img/logo.png",
            "img/icon.svg",
            "assets/font.woff2",
            "readme.txt",
            "bin/data.bin", // unknown ext: still in-root, served as octet-stream
        ];

        let mut rel_targets: Vec<(String, Vec<u8>)> = Vec::new();
        let mut in_root_contents: HashSet<Vec<u8>> = HashSet::new();
        for rel in files {
            let full = root.join(rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).expect("create nested dir");
            }
            // Unique body per file so the safe-path integrity check is meaningful.
            let body = format!("RAN-ASSET::{}::unique-body-0123456789\n", rel).into_bytes();
            fs::write(&full, &body).expect("write asset");
            rel_targets.push(((*rel).to_string(), body.clone()));
            in_root_contents.insert(body);
        }

        // ---- A secret file OUTSIDE the root (sibling under .tmp_tests/). ----
        let secret_path = pbt::unique_tmp_path("web_secret", "txt");
        let secret_bytes = b"TOP-SECRET-OUTSIDE-WEB-ROOT-DO-NOT-SERVE\n".to_vec();
        fs::write(&secret_path, &secret_bytes).expect("write secret");
        let secret_name = secret_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();

        let root_str = root.to_string_lossy().to_string();
        let gen = case_gen(rel_targets, secret_name);

        let result = pbt::run(&gen, move |case: &WebCase| {
            // ---- Clause (c): Asset_Type mapping is total. ----
            let probe = format!("file.{}", case.ext);
            let (ct, recognized) = detect_mime_ext(&probe);
            match case.ext_supported {
                Some(expected) => {
                    if ct != expected || !recognized {
                        return false;
                    }
                }
                None => {
                    if ct != "application/octet-stream" || recognized {
                        return false;
                    }
                }
            }
            // `detect_mime` must agree with `detect_mime_ext`.
            if detect_mime(&probe) != ct {
                return false;
            }

            // ---- Clauses (a)+(b): resolution security & integrity. ----
            // No conditional headers, so a 304 is never produced here.
            match serve_static(&root_str, &case.req_path, None) {
                None => {
                    // Absent / not resolvable. A safe in-root path must NOT be
                    // reported absent — it has to be served.
                    case.safe_target.is_none()
                }
                Some(resp) => match resp.status {
                    403 => {
                        // Rejected. A safe in-root path must never be rejected.
                        case.safe_target.is_none()
                    }
                    200 => {
                        // Served bytes must belong to an in-root file and must
                        // never equal the out-of-root secret (no escape).
                        if resp.body == secret_bytes {
                            return false;
                        }
                        if !in_root_contents.contains(&resp.body) {
                            return false;
                        }
                        // Integrity: a safe path serves exactly its disk bytes.
                        match &case.safe_target {
                            Some(expected) => &resp.body == expected,
                            None => true,
                        }
                    }
                    // Any other status from the static resolver is unexpected.
                    _ => false,
                },
            }
        });

        // ---- Cleanup temp artifacts (best effort). ----
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_file(&secret_path);

        if let Err(failure) = result {
            panic!("[prop_web_asset_serving_is_safe_and_total] {}", failure);
        }
    }
}
