//! Built-in HTTP/web server wiring (extracted from mod.rs).
//! Part of the `Environment` inherent impl.

use super::*;

impl Environment {
    pub(super) fn builtin_http_server(&mut self, port: u16) {
        // Launch the shared built-in server with the conventional `public`
        // web root. The SPA fallback flag honors any prior `web.spa(...)` call
        // (default disabled), since `web` and `http` share one FastServer.
        self.launch_server("public", port);
    }

    /// Convert the runtime's registered routes into `net::Route`s for the
    /// FastServer. Shared by the `http` and `web` server-launch paths.
    fn build_net_routes(&self) -> Vec<crate::stdlib::net::Route> {
        use crate::stdlib::net::{HttpMethod, Route};
        self.routes
            .iter()
            .map(|r| {
                let method = match r.method.as_str() {
                    "POST" => HttpMethod::Post,
                    "PUT" => HttpMethod::Put,
                    "PATCH" => HttpMethod::Patch,
                    "DELETE" => HttpMethod::Delete,
                    _ => HttpMethod::Get,
                };
                Route {
                    method,
                    path: r.path.clone(),
                    handler_name: r.handler_name.clone(),
                }
            })
            .collect()
    }

    /// Build the per-request dispatch closure that runs a Ran handler in a
    /// fresh environment snapshot. Shared by the `http` and `web` server-launch
    /// paths so static assets and `http.get/post/...` routes work together.
    fn build_handler_dispatch(&self) -> std::sync::Arc<crate::stdlib::net::DispatchFn> {
        use crate::stdlib::net::{Request, Response};
        use std::sync::Arc;

        // Clone what handlers need.
        let functions = self.functions.clone();
        let fn_params = self.fn_params.clone();
        let fn_mut = self.fn_mut.clone();
        let variables = self.flatten_scopes();
        let module_aliases = self.module_aliases.clone();
        let methods = self.methods.clone();
        let traits = self.traits.clone();
        let enums = self.enums.clone();

        Arc::new(move |handler_name: &str, req: &Request| -> Response {
            let mut handler_env = Environment {
                globals: variables.clone(),
                frames: Vec::new(),
                frame_base: 0,
                base_stack: Vec::new(),
                functions: functions.clone(),
                fn_params: fn_params.clone(),
                fn_mut: fn_mut.clone(),
                routes: Vec::new(),
                module_aliases: module_aliases.clone(),
                methods: methods.clone(),
                traits: traits.clone(),
                enums: enums.clone(),
                spawned: Vec::new(),
            };

            // Inject request data into the handler's global scope.
            handler_env.var_set_local("req_method", Value::Str(req.method.as_str().to_string()));
            handler_env.var_set_local("req_path", Value::Str(req.path.clone()));
            handler_env.var_set_local("req_body", Value::Str(
                req.body_as_str().unwrap_or("").to_string()
            ));

            // Inject query params
            for (k, v) in &req.query_params {
                handler_env.var_set_local(&format!("query_{}", k), Value::Str(v.clone()));
            }
            // Inject path params
            for (k, v) in &req.path_params {
                handler_env.var_set_local(&format!("param_{}", k), Value::Str(v.clone()));
            }
            // Inject cookies (cookie_<name>)
            for (k, v) in &req.cookies {
                handler_env.var_set_local(&format!("cookie_{}", k), Value::Str(v.clone()));
            }

            // Reset per-request response controls (set_header/set_cookie/status).
            resp_reset();

            // Run the handler, catching any runtime fault so a single bad
            // request returns 500 instead of taking down the whole server.
            let hname = handler_name.to_string();
            let outcome = catch_fault(move || {
                let result = handler_env.call_function(&hname, &[]);
                match result {
                    Value::Str(s) => {
                        // Interpolate request vars ($param_*, $query_*, $req_*).
                        let s = handler_env.interpolate_string(&s);
                        let trimmed = s.trim();
                        if trimmed.starts_with('<') {
                            Response::html(&s)
                        } else if trimmed.starts_with('{') || trimmed.starts_with('[') {
                            Response::json(&s)
                        } else {
                            Response::ok(&s)
                        }
                    }
                    Value::Map(_) | Value::Object(_, _) => Response::json(&result.to_json()),
                    _ => Response::ok(&format!("{}", result)),
                }
            });
            // R3.3: a faulting handler must yield a generic 500 and the server
            // must keep serving later requests. `catch_fault` already contained
            // the unwind, so the worker survives; here we shape the response.
            match outcome {
                Ok(mut resp) => {
                    // Success path: apply handler-requested status + headers/cookies.
                    let (status, headers) = resp_take();
                    if let Some(code) = status {
                        resp.status = code;
                    }
                    for (k, v) in headers {
                        resp.headers.insert(k, v);
                    }
                    resp
                }
                Err(fault) => {
                    // Log the full diagnostic SERVER-SIDE ONLY. The client must
                    // never see the fault code, message, help text, or any stack
                    // detail (R3 security).
                    eprintln!(
                        "[ran] handler `{}` faulted: error[{}]: {}",
                        handler_name, fault.code, fault.message
                    );
                    // Drain and DISCARD any per-request response controls the
                    // handler may have set before faulting, so partially-built
                    // internal headers/status cannot leak into the 500 response.
                    let _ = resp_take();
                    // Generic body only — no internals are exposed to the client.
                    Response::status(500, "Internal Server Error")
                }
            }
        })
    }

    /// Start the built-in FastServer serving static assets from `static_dir`
    /// plus any registered routes, and block (R1.1/R5.1). The SPA fallback flag
    /// is read from the runtime (`web.spa(...)`, default disabled) so `web` and
    /// `http` share one underlying server configuration.
    fn launch_server(&mut self, static_dir: &str, port: u16) {
        use crate::stdlib::net::{self as network, FastServer};

        let net_routes = self.build_net_routes();
        let dispatch = self.build_handler_dispatch();
        let spa = WEB_SPA.with(|c| c.get());

        let mut server = FastServer::new()
            .static_files(static_dir)
            .spa(spa)
            .cors(network::CorsConfig::default())
            .set_dispatch(dispatch);

        // Inject routes.
        server.routes = net_routes;

        // Bind host is configurable via RAN_HOST (default: all interfaces).
        // Set RAN_HOST=127.0.0.1 to restrict the server to localhost.
        let host = std::env::var("RAN_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        if let Err(e) = server.listen(&host, port) {
            eprintln!("ran: server error: {}", e);
        }
    }

    /// `web.serve(dir [, port])` — configure the Web_Root to `dir` and start
    /// the built-in web server serving static assets from it (R1.1/R5.1).
    ///
    /// If the configured Web_Root does not exist, emit `E0401` and return
    /// without serving. Otherwise this blocks like `http.server`, serving
    /// static files alongside any routes registered via `http.get/post/...`.
    /// The desired SPA fallback (from `web.spa(...)`) is applied when the
    /// server is built. The configured frontend build command (`web.build(...)`)
    /// is run to completion before serving (R5.2).
    pub(super) fn builtin_web_serve(&mut self, dir: &str, port: u16) {
        // E0401: refuse to serve when the Web_Root is missing/unreadable.
        let root = std::path::Path::new(dir);
        if !root.is_dir() {
            let msg = format!("Web_Root `{}` tidak ditemukan atau bukan direktori", dir);
            let hint = crate::support::diagnostics::code_severity_hint("E0401")
                .map(|(_, h)| h)
                .unwrap_or("Pastikan direktori Web_Root ada dan dapat dibaca.");
            eprintln!("\x1b[31;1merror\x1b[0m[E0401]: {}", msg);
            eprintln!("  \x1b[36m= help\x1b[0m: {}", hint);
            return;
        }

        // R5.2: if a frontend build command is configured, run it to completion
        // BEFORE binding/serving. On failure we emit E0404 and refuse to serve,
        // so stale or unbuilt assets are never delivered.
        let build_cmd = WEB_BUILD_CMD.with(|c| c.borrow().clone());
        if let Some(cmd) = build_cmd {
            if run_frontend_build(&cmd).is_err() {
                let msg = format!(
                    "perintah build frontend gagal dijalankan: `{}`",
                    cmd
                );
                let hint = crate::support::diagnostics::code_severity_hint("E0404")
                    .map(|(_, h)| h)
                    .unwrap_or("Perbaiki perintah build sebelum menyajikan aset.");
                eprintln!("\x1b[31;1merror\x1b[0m[E0404]: {}", msg);
                eprintln!("  \x1b[36m= help\x1b[0m: {}", hint);
                // Do not bind the server: never serve stale assets (R5.2).
                return;
            }
        }

        self.launch_server(dir, port);
    }
}
