//! Stdlib module-method dispatch (http/web/db/concurrency/fs/json/...).
//! Extracted from mod.rs; part of the `Environment` inherent impl.

use super::*;

impl Environment {
    // ========================================================================
    // Module methods (http, time, fs, json, os, html)
    // ========================================================================

    pub(super) fn call_module_method(&mut self, module: &str, method: &str, args: &[Expression]) -> Value {
        match (module, method) {
            // --- HTTP ---
            ("http", "server") => {
                let port = self.eval_arg_int(args, 0, 8080) as u16;
                self.builtin_http_server(port);
                Value::Void
            }
            ("http", "listen") => {
                let port = self.eval_arg_int(args, 0, 8080) as u16;
                self.builtin_http_server(port);
                Value::Void
            }
            ("http", "get") => {
                let path = self.eval_arg_str(args, 0, "/");
                let handler = self.eval_arg_str(args, 1, "");
                self.routes.push(HttpRoute {
                    method: "GET".to_string(),
                    path,
                    handler_name: handler,
                });
                Value::Void
            }
            ("http", "post") => {
                let path = self.eval_arg_str(args, 0, "/");
                let handler = self.eval_arg_str(args, 1, "");
                self.routes.push(HttpRoute {
                    method: "POST".to_string(),
                    path,
                    handler_name: handler,
                });
                Value::Void
            }
            ("http", "put") => {
                let path = self.eval_arg_str(args, 0, "/");
                let handler = self.eval_arg_str(args, 1, "");
                self.routes.push(HttpRoute {
                    method: "PUT".to_string(),
                    path,
                    handler_name: handler,
                });
                Value::Void
            }
            ("http", "delete") => {
                let path = self.eval_arg_str(args, 0, "/");
                let handler = self.eval_arg_str(args, 1, "");
                self.routes.push(HttpRoute {
                    method: "DELETE".to_string(),
                    path,
                    handler_name: handler,
                });
                Value::Void
            }
            ("http", "patch") => {
                let path = self.eval_arg_str(args, 0, "/");
                let handler = self.eval_arg_str(args, 1, "");
                self.routes.push(HttpRoute {
                    method: "PATCH".to_string(),
                    path,
                    handler_name: handler,
                });
                Value::Void
            }
            // Response control (call inside a handler before returning):
            ("http", "set_header") => {
                let k = self.eval_arg_str(args, 0, "");
                let v = self.eval_arg_str(args, 1, "");
                resp_add_header(&k, &v);
                Value::Void
            }
            ("http", "set_status") => {
                let code = self.eval_arg_int(args, 0, 200) as u16;
                resp_set_status(code);
                Value::Void
            }
            // http.set_cookie(name, value [, max_age_seconds])
            ("http", "set_cookie") => {
                let name = self.eval_arg_str(args, 0, "");
                let value = self.eval_arg_str(args, 1, "");
                let mut cookie = format!("{}={}; Path=/; HttpOnly; SameSite=Lax", name, value);
                if args.len() >= 3 {
                    let max_age = self.eval_arg_int(args, 2, 0);
                    cookie.push_str(&format!("; Max-Age={}", max_age));
                }
                resp_add_header("Set-Cookie", &cookie);
                Value::Void
            }
            // http.clear_cookie(name) — expire it immediately.
            ("http", "clear_cookie") => {
                let name = self.eval_arg_str(args, 0, "");
                resp_add_header(
                    "Set-Cookie",
                    &format!("{}=; Path=/; HttpOnly; Max-Age=0", name),
                );
                Value::Void
            }
            // http.redirect(location) — 302 + Location header.
            ("http", "redirect") => {
                let loc = self.eval_arg_str(args, 0, "/");
                resp_set_status(302);
                resp_add_header("Location", &loc);
                Value::Void
            }

            // --- TIME ---
            ("time", "sleep") => {
                let ms = self.eval_arg_int(args, 0, 0) as u64;
                std::thread::sleep(std::time::Duration::from_millis(ms));
                Value::Void
            }
            ("time", "now") => {
                use std::time::{SystemTime, UNIX_EPOCH};
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                Value::Int(ts as i64)
            }

            // --- FS ---
            ("fs", "read") => {
                let path = self.eval_arg_str(args, 0, "");
                match std::fs::read_to_string(&path) {
                    Ok(content) => Value::Str(content),
                    Err(e) => {
                        eprintln!("ran: fs.read error: {}", e);
                        Value::Void
                    }
                }
            }
            ("fs", "write") => {
                let path = self.eval_arg_str(args, 0, "");
                let content = self.eval_arg_str(args, 1, "");
                match std::fs::write(&path, &content) {
                    Ok(_) => Value::Bool(true),
                    Err(e) => {
                        eprintln!("ran: fs.write error: {}", e);
                        Value::Bool(false)
                    }
                }
            }
            ("fs", "exists") => {
                let path = self.eval_arg_str(args, 0, "");
                Value::Bool(std::path::Path::new(&path).exists())
            }
            ("fs", "readdir") => {
                let path = self.eval_arg_str(args, 0, ".");
                match std::fs::read_dir(&path) {
                    Ok(entries) => {
                        let items: Vec<Value> = entries
                            .filter_map(|e| e.ok())
                            .map(|e| Value::Str(e.file_name().to_string_lossy().to_string()))
                            .collect();
                        Value::Array(items)
                    }
                    Err(_) => Value::Array(vec![]),
                }
            }

            // --- JSON ---
            ("json", "encode") | ("json", "stringify") => {
                if let Some(arg) = args.first() {
                    let val = self.eval_expression(arg);
                    Value::Str(val.to_json())
                } else {
                    Value::Str("null".to_string())
                }
            }
            ("json", "decode") | ("json", "parse") => {
                let s = self.eval_arg_str(args, 0, "");
                self.parse_json(&s)
            }
            ("json", "valid") => {
                let s = self.eval_arg_str(args, 0, "");
                Value::Bool(Self::json_is_valid(&s))
            }
            // json.get(value_or_json_string, "a.b.0.c") — dotted path lookup.
            ("json", "get") => {
                let base = match self.eval_arg_val(args, 0) {
                    Value::Str(s) => self.parse_json(&s),
                    other => other,
                };
                let path = self.eval_arg_str(args, 1, "");
                Self::json_path_get(&base, &path)
            }

            // --- OS ---
            ("os", "args") => {
                let args: Vec<Value> = std::env::args().map(|a| Value::Str(a)).collect();
                Value::Array(args)
            }
            ("os", "env") => {
                let key = self.eval_arg_str(args, 0, "");
                match std::env::var(&key) {
                    Ok(val) => Value::Str(val),
                    Err(_) => Value::Void,
                }
            }
            ("os", "exit") => {
                let code = self.eval_arg_int(args, 0, 0) as i32;
                std::process::exit(code);
            }

            // --- MATH ---
            ("math", "abs") => {
                match self.eval_arg_val(args, 0) {
                    Value::Float(f) => Value::Float(f.abs()),
                    Value::Int(n) => Value::Int(n.abs()),
                    _ => Value::Int(0),
                }
            }
            ("math", "max") => {
                let a = self.eval_arg_val(args, 0);
                let b = self.eval_arg_val(args, 1);
                if matches!(a, Value::Float(_)) || matches!(b, Value::Float(_)) {
                    Value::Float(a.as_f64().max(b.as_f64()))
                } else {
                    Value::Int(a.as_i64().max(b.as_i64()))
                }
            }
            ("math", "min") => {
                let a = self.eval_arg_val(args, 0);
                let b = self.eval_arg_val(args, 1);
                if matches!(a, Value::Float(_)) || matches!(b, Value::Float(_)) {
                    Value::Float(a.as_f64().min(b.as_f64()))
                } else {
                    Value::Int(a.as_i64().min(b.as_i64()))
                }
            }
            ("math", "sqrt") => Value::Float(self.eval_arg_f64(args, 0).sqrt()),
            ("math", "pow") => {
                let base = self.eval_arg_f64(args, 0);
                let exp = self.eval_arg_f64(args, 1);
                Value::Float(base.powf(exp))
            }
            ("math", "floor") => Value::Int(self.eval_arg_f64(args, 0).floor() as i64),
            ("math", "ceil") => Value::Int(self.eval_arg_f64(args, 0).ceil() as i64),
            ("math", "round") => Value::Int(self.eval_arg_f64(args, 0).round() as i64),
            ("math", "sin") => Value::Float(self.eval_arg_f64(args, 0).sin()),
            ("math", "cos") => Value::Float(self.eval_arg_f64(args, 0).cos()),
            ("math", "tan") => Value::Float(self.eval_arg_f64(args, 0).tan()),
            ("math", "log") => Value::Float(self.eval_arg_f64(args, 0).ln()),
            ("math", "log10") => Value::Float(self.eval_arg_f64(args, 0).log10()),
            ("math", "pi") => Value::Float(std::f64::consts::PI),
            ("math", "e") => Value::Float(std::f64::consts::E),

            // --- HTML template ---
            ("html", "render") => {
                let template = self.eval_arg_str(args, 0, "");
                Value::Str(self.render_template(&template))
            }

            // --- FS (extended) ---
            ("fs", "append") => {
                use std::io::Write;
                let path = self.eval_arg_str(args, 0, "");
                let content = self.eval_arg_str(args, 1, "");
                match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                    Ok(mut f) => Value::Bool(f.write_all(content.as_bytes()).is_ok()),
                    Err(_) => Value::Bool(false),
                }
            }
            ("fs", "remove") => {
                let path = self.eval_arg_str(args, 0, "");
                Value::Bool(std::fs::remove_file(&path).is_ok())
            }
            ("fs", "mkdir") => {
                let path = self.eval_arg_str(args, 0, "");
                Value::Bool(std::fs::create_dir_all(&path).is_ok())
            }
            ("fs", "is_file") => {
                let path = self.eval_arg_str(args, 0, "");
                Value::Bool(std::path::Path::new(&path).is_file())
            }
            ("fs", "is_dir") => {
                let path = self.eval_arg_str(args, 0, "");
                Value::Bool(std::path::Path::new(&path).is_dir())
            }

            // --- TIME (extended) ---
            ("time", "now_ms") => {
                use std::time::{SystemTime, UNIX_EPOCH};
                let ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                Value::Int(ms as i64)
            }

            // --- OS (extended) ---
            ("os", "cwd") => {
                match std::env::current_dir() {
                    Ok(p) => Value::Str(p.to_string_lossy().to_string()),
                    Err(_) => Value::Str(String::new()),
                }
            }
            ("os", "platform") => Value::Str(std::env::consts::OS.to_string()),
            ("os", "arch") => Value::Str(std::env::consts::ARCH.to_string()),
            ("os", "cpu_count") => Value::Int(crate::support::sysinfo::cpu_count() as i64),
            ("os", "meminfo") => {
                use crate::support::sysinfo as si;
                let total = si::mem_total();
                let avail = si::mem_available();
                let mut m = HashMap::new();
                m.insert("total".to_string(), Value::Int(total as i64));
                m.insert("available".to_string(), Value::Int(avail as i64));
                m.insert("used".to_string(), Value::Int(total.saturating_sub(avail) as i64));
                m.insert("reserve".to_string(), Value::Int(si::os_reserve_bytes() as i64));
                m.insert("budget".to_string(), Value::Int(si::memory_budget_bytes() as i64));
                Value::Map(m)
            }
            ("os", "memory_budget") => {
                Value::Int(crate::support::sysinfo::memory_budget_bytes() as i64)
            }
            ("os", "setenv") => {
                let key = self.eval_arg_str(args, 0, "");
                let val = self.eval_arg_str(args, 1, "");
                std::env::set_var(key, val);
                Value::Bool(true)
            }

            // --- STR (string utilities as a module) ---
            ("str", "from") => {
                let v = self.eval_arg_val(args, 0);
                Value::Str(format!("{}", v))
            }
            ("str", "upper") => Value::Str(self.eval_arg_str(args, 0, "").to_uppercase()),
            ("str", "lower") => Value::Str(self.eval_arg_str(args, 0, "").to_lowercase()),
            ("str", "trim") => Value::Str(self.eval_arg_str(args, 0, "").trim().to_string()),
            ("str", "len") => Value::Int(self.eval_arg_str(args, 0, "").chars().count() as i64),
            ("str", "contains") => {
                let hay = self.eval_arg_str(args, 0, "");
                let needle = self.eval_arg_str(args, 1, "");
                Value::Bool(hay.contains(&needle))
            }
            ("str", "replace") => {
                let s = self.eval_arg_str(args, 0, "");
                let from = self.eval_arg_str(args, 1, "");
                let to = self.eval_arg_str(args, 2, "");
                Value::Str(s.replace(&from, &to))
            }
            ("str", "split") => {
                let s = self.eval_arg_str(args, 0, "");
                let delim = self.eval_arg_str(args, 1, " ");
                Value::Array(s.split(&delim).map(|p| Value::Str(p.to_string())).collect())
            }
            ("str", "join") => {
                let arr = self.eval_arg_val(args, 0);
                let sep = self.eval_arg_str(args, 1, "");
                if let Value::Array(items) = arr {
                    let parts: Vec<String> = items.iter().map(|v| format!("{}", v)).collect();
                    Value::Str(parts.join(&sep))
                } else {
                    Value::Str(String::new())
                }
            }

            // --- RAND (random numbers, xorshift PRNG) ---
            ("rand", "int") => {
                let lo = self.eval_arg_int(args, 0, 0);
                let hi = self.eval_arg_int(args, 1, i64::MAX);
                let range = (hi - lo).max(1);
                Value::Int(lo + (Self::rand_u64() % range as u64) as i64)
            }
            ("rand", "float") => {
                Value::Float((Self::rand_u64() as f64) / (u64::MAX as f64))
            }
            ("rand", "bool") => Value::Bool(Self::rand_u64() % 2 == 0),

            // --- CRYPTO (hashing & encoding; built on in-tree SHA-256) ---
            ("crypto", "sha256") | ("crypto", "sha256_hex") => {
                let s = self.eval_arg_str(args, 0, "");
                Value::Str(crate::support::crypto::sha256_hex(s.as_bytes()))
            }
            ("crypto", "hmac_sha256") => {
                let key = self.eval_arg_str(args, 0, "");
                let msg = self.eval_arg_str(args, 1, "");
                let tag = crate::support::crypto::hmac_sha256(key.as_bytes(), msg.as_bytes());
                Value::Str(crate::support::crypto::hex_encode(&tag))
            }
            ("crypto", "hex") | ("crypto", "hex_encode") => {
                let s = self.eval_arg_str(args, 0, "");
                Value::Str(crate::support::crypto::hex_encode(s.as_bytes()))
            }
            ("crypto", "base64") | ("crypto", "base64_encode") => {
                let s = self.eval_arg_str(args, 0, "");
                Value::Str(crate::support::crypto::base64_encode(s.as_bytes()))
            }
            ("crypto", "base64_decode") => {
                let s = self.eval_arg_str(args, 0, "");
                match crate::support::crypto::base64_decode(&s) {
                    Some(bytes) => Value::Str(String::from_utf8_lossy(&bytes).to_string()),
                    None => Value::Void,
                }
            }

            // --- HTTP CLIENT (plaintext http:// only; see docs/stdlib/http.md) ---
            // Returns a map: { "status": int, "body": str, "ok": bool, "error": str }
            ("http", "fetch") => {
                let url = self.eval_arg_str(args, 0, "");
                self.http_client_call("GET", &url, "")
            }
            ("http", "post_to") => {
                let url = self.eval_arg_str(args, 0, "");
                let body = self.eval_arg_str(args, 1, "");
                self.http_client_call("POST", &url, &body)
            }
            ("http", "request") => {
                let method = self.eval_arg_str(args, 0, "GET");
                let url = self.eval_arg_str(args, 1, "");
                let body = self.eval_arg_str(args, 2, "");
                self.http_client_call(&method, &url, &body)
            }

            // --- LOG (leveled structured logging to stderr) ---
            ("log", "debug") => { self.log_at("DEBUG", "\x1b[36m", args); Value::Void }
            ("log", "info")  => { self.log_at("INFO",  "\x1b[32m", args); Value::Void }
            ("log", "warn")  => { self.log_at("WARN",  "\x1b[33m", args); Value::Void }
            ("log", "error") => { self.log_at("ERROR", "\x1b[31m", args); Value::Void }
            ("log", "fatal") => {
                self.log_at("FATAL", "\x1b[31;1m", args);
                std::process::exit(1);
            }

            // --- STR (extended) ---
            ("str", "starts_with") => {
                let s = self.eval_arg_str(args, 0, "");
                let p = self.eval_arg_str(args, 1, "");
                Value::Bool(s.starts_with(&p))
            }
            ("str", "ends_with") => {
                let s = self.eval_arg_str(args, 0, "");
                let p = self.eval_arg_str(args, 1, "");
                Value::Bool(s.ends_with(&p))
            }
            ("str", "index_of") => {
                let s = self.eval_arg_str(args, 0, "");
                let needle = self.eval_arg_str(args, 1, "");
                match s.find(&needle) {
                    Some(i) => Value::Int(s[..i].chars().count() as i64),
                    None => Value::Int(-1),
                }
            }
            ("str", "repeat") => {
                let s = self.eval_arg_str(args, 0, "");
                let n = self.eval_arg_int(args, 1, 0).max(0) as usize;
                Value::Str(s.repeat(n))
            }
            ("str", "reverse") => {
                let s = self.eval_arg_str(args, 0, "");
                Value::Str(s.chars().rev().collect())
            }
            ("str", "trim_start") => Value::Str(self.eval_arg_str(args, 0, "").trim_start().to_string()),
            ("str", "trim_end") => Value::Str(self.eval_arg_str(args, 0, "").trim_end().to_string()),
            ("str", "pad_left") => {
                let s = self.eval_arg_str(args, 0, "");
                let width = self.eval_arg_int(args, 1, 0).max(0) as usize;
                let pad = self.eval_arg_str(args, 2, " ");
                let pad_ch = pad.chars().next().unwrap_or(' ');
                let len = s.chars().count();
                if len >= width { Value::Str(s) }
                else { Value::Str(format!("{}{}", pad_ch.to_string().repeat(width - len), s)) }
            }
            ("str", "pad_right") => {
                let s = self.eval_arg_str(args, 0, "");
                let width = self.eval_arg_int(args, 1, 0).max(0) as usize;
                let pad = self.eval_arg_str(args, 2, " ");
                let pad_ch = pad.chars().next().unwrap_or(' ');
                let len = s.chars().count();
                if len >= width { Value::Str(s) }
                else { Value::Str(format!("{}{}", s, pad_ch.to_string().repeat(width - len))) }
            }
            ("str", "to_int") => {
                let s = self.eval_arg_str(args, 0, "");
                Value::Int(s.trim().parse::<i64>().unwrap_or(0))
            }
            ("str", "to_float") => {
                let s = self.eval_arg_str(args, 0, "");
                Value::Float(s.trim().parse::<f64>().unwrap_or(0.0))
            }

            // --- JSON (pretty) ---
            ("json", "pretty") => {
                if let Some(arg) = args.first() {
                    let val = self.eval_expression(arg);
                    Value::Str(Self::to_json_pretty(&val, 0))
                } else {
                    Value::Str("null".to_string())
                }
            }

            // --- OS (extended) ---
            ("os", "getpid") => Value::Int(std::process::id() as i64),
            ("os", "hostname") => {
                let h = std::fs::read_to_string("/etc/hostname")
                    .map(|s| s.trim().to_string())
                    .or_else(|_| std::env::var("HOSTNAME"))
                    .unwrap_or_else(|_| "localhost".to_string());
                Value::Str(h)
            }
            ("os", "env_or") => {
                let key = self.eval_arg_str(args, 0, "");
                let default = self.eval_arg_str(args, 1, "");
                Value::Str(std::env::var(&key).unwrap_or(default))
            }

            // --- TIME (extended) ---
            ("time", "now_iso") => {
                use std::time::{SystemTime, UNIX_EPOCH};
                let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                Value::Str(Self::unix_to_iso(secs as i64))
            }

            // --- FS (extended) ---
            ("fs", "size") => {
                let path = self.eval_arg_str(args, 0, "");
                match std::fs::metadata(&path) {
                    Ok(m) => Value::Int(m.len() as i64),
                    Err(_) => Value::Int(-1),
                }
            }
            ("fs", "copy") => {
                let from = self.eval_arg_str(args, 0, "");
                let to = self.eval_arg_str(args, 1, "");
                Value::Bool(std::fs::copy(&from, &to).is_ok())
            }
            ("fs", "rename") => {
                let from = self.eval_arg_str(args, 0, "");
                let to = self.eval_arg_str(args, 1, "");
                Value::Bool(std::fs::rename(&from, &to).is_ok())
            }

            // --- ENV (dotenv + typed config; enterprise-grade) ---
            ("env", "get") => {
                let key = self.eval_arg_str(args, 0, "");
                match std::env::var(&key) {
                    Ok(v) => Value::Str(v),
                    Err(_) => Value::Void,
                }
            }
            ("env", "get_or") => {
                let key = self.eval_arg_str(args, 0, "");
                let def = self.eval_arg_str(args, 1, "");
                Value::Str(std::env::var(&key).unwrap_or(def))
            }
            ("env", "require") => {
                let key = self.eval_arg_str(args, 0, "");
                match std::env::var(&key) {
                    Ok(v) => Value::Str(v),
                    Err(_) => runtime_error(
                        "E1005",
                        &format!("required environment variable `{}` is not set", key),
                        "set it in the environment or in a .env file (env.load)",
                    ),
                }
            }
            ("env", "has") => {
                let key = self.eval_arg_str(args, 0, "");
                Value::Bool(std::env::var(&key).is_ok())
            }
            ("env", "set") => {
                let key = self.eval_arg_str(args, 0, "");
                let val = self.eval_arg_str(args, 1, "");
                std::env::set_var(&key, &val);
                Value::Bool(true)
            }
            ("env", "unset") => {
                let key = self.eval_arg_str(args, 0, "");
                std::env::remove_var(&key);
                Value::Bool(true)
            }
            ("env", "int") => {
                let key = self.eval_arg_str(args, 0, "");
                let def = self.eval_arg_int(args, 1, 0);
                match std::env::var(&key) {
                    Ok(v) => Value::Int(v.trim().parse::<i64>().unwrap_or(def)),
                    Err(_) => Value::Int(def),
                }
            }
            ("env", "float") => {
                let key = self.eval_arg_str(args, 0, "");
                let def = self.eval_arg_f64(args, 1);
                match std::env::var(&key) {
                    Ok(v) => Value::Float(v.trim().parse::<f64>().unwrap_or(def)),
                    Err(_) => Value::Float(def),
                }
            }
            ("env", "bool") => {
                let key = self.eval_arg_str(args, 0, "");
                let def = self.eval_arg_val(args, 1).is_truthy_val();
                match std::env::var(&key) {
                    Ok(v) => Value::Bool(Self::parse_env_bool(&v).unwrap_or(def)),
                    Err(_) => Value::Bool(def),
                }
            }
            // env.decimal(key, default_str) — exact config for money/rates.
            ("env", "decimal") => {
                let key = self.eval_arg_str(args, 0, "");
                let def = self.eval_arg_str(args, 1, "0");
                let raw = std::env::var(&key).unwrap_or_else(|_| def.clone());
                match Decimal::parse(&raw) {
                    Ok(d) => Value::Decimal(d),
                    Err(_) => match Decimal::parse(&def) {
                        Ok(d) => Value::Decimal(d),
                        Err(_) => Value::Decimal(Decimal::zero()),
                    },
                }
            }
            ("env", "all") => {
                let mut m = HashMap::new();
                for (k, v) in std::env::vars() {
                    m.insert(k, Value::Str(v));
                }
                Value::Map(m)
            }
            // env.load(path) — load a .env file; returns count of vars set.
            // Existing process variables are NOT overridden (dotenv convention).
            ("env", "load") => {
                let path = self.eval_arg_str(args, 0, ".env");
                Value::Int(Self::load_dotenv(&path, false))
            }
            // env.load_override(path) — like load, but overrides existing vars.
            ("env", "load_override") => {
                let path = self.eval_arg_str(args, 0, ".env");
                Value::Int(Self::load_dotenv(&path, true))
            }
            // env.load_default() — load ./.env if present (no error if missing).
            ("env", "load_default") => Value::Int(Self::load_dotenv(".env", false)),

            // --- DECIMAL (exact money / business math) ---
            // decimal.new(x) / decimal.parse(str): build a Decimal from a string,
            // int, or float (float goes via its text form).
            ("decimal", "new") | ("decimal", "parse") | ("decimal", "from") => {
                let v = self.eval_arg_val(args, 0);
                self.make_decimal(&v)
            }
            // decimal.add/sub/mul(a, b)
            ("decimal", "add") => self.decimal_op2(args, BinaryOperator::Add),
            ("decimal", "sub") => self.decimal_op2(args, BinaryOperator::Sub),
            ("decimal", "mul") => self.decimal_op2(args, BinaryOperator::Mul),
            // decimal.div(a, b, scale, mode): exact, explicit rounding
            ("decimal", "div") => {
                let a = self.decimal_arg(args, 0);
                let b = self.decimal_arg(args, 1);
                let scale = self.eval_arg_int(args, 2, 2).max(0) as u32;
                let mode = Rounding::from_name(&self.eval_arg_str(args, 3, "half_up"));
                if b.is_zero() {
                    runtime_error("E1002", "decimal division by zero", "guard the divisor");
                }
                match a.div(&b, scale, mode) {
                    Ok(d) => Value::Decimal(d),
                    Err(e) => runtime_error("E1003", &e, "reduce the scale or operand size"),
                }
            }
            // decimal.round(a, scale, mode?)
            ("decimal", "round") => {
                let a = self.decimal_arg(args, 0);
                let scale = self.eval_arg_int(args, 1, 2).max(0) as u32;
                let mode = Rounding::from_name(&self.eval_arg_str(args, 2, "half_up"));
                match a.rescale(scale, mode) {
                    Ok(d) => Value::Decimal(d),
                    Err(e) => runtime_error("E1003", &e, "reduce the scale"),
                }
            }
            ("decimal", "cmp") => {
                let a = self.decimal_arg(args, 0);
                let b = self.decimal_arg(args, 1);
                Value::Int(match a.cmp(&b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                })
            }
            ("decimal", "abs") => Value::Decimal(self.decimal_arg(args, 0).abs()),
            ("decimal", "neg") => Value::Decimal(self.decimal_arg(args, 0).neg()),
            ("decimal", "is_zero") => Value::Bool(self.decimal_arg(args, 0).is_zero()),

            // --- COBOL-grade business helpers (R: exact money math) ----------
            // decimal.to_fixed(a, scale, mode?): rescale to a fixed number of
            // decimal places (COBOL fixed PIC 9..V9..), rounding half-up by
            // default. The canonical way to pin a money value to 2 places.
            ("decimal", "to_fixed") => {
                let a = self.decimal_arg(args, 0);
                let scale = self.eval_arg_int(args, 1, 2).max(0) as u32;
                let mode = Rounding::from_name(&self.eval_arg_str(args, 2, "half_up"));
                match a.rescale(scale, mode) {
                    Ok(d) => Value::Decimal(d),
                    Err(e) => runtime_error("E1003", &e, "reduce the scale"),
                }
            }
            // decimal.format(a, decimals?, thousands?, point?): PICTURE-style
            // fixed formatting with grouped thousands. Defaults: 2 places, ","
            // group, "." point (US). For EU pass ("." , ",").
            ("decimal", "format") => {
                let a = self.decimal_arg(args, 0);
                let decimals = self.eval_arg_int(args, 1, 2).max(0) as u32;
                let thousands = self.eval_arg_str(args, 2, ",");
                let point = self.eval_arg_str(args, 3, ".");
                Value::Str(a.format(decimals, &thousands, &point))
            }
            // decimal.sum(array): exact running total of a list of money values
            // (batch totals — a COBOL staple). Empty/non-array -> 0.
            ("decimal", "sum") => {
                let arr = self.eval_arg_val(args, 0);
                let mut acc = crate::support::decimal::Decimal::zero();
                if let Value::Array(items) = arr {
                    for it in items {
                        if let Some(d) = Self::to_decimal(&it) {
                            match acc.add(&d) {
                                Ok(s) => acc = s,
                                Err(e) => runtime_error("E1003", &e, "totals exceed decimal range; split the batch"),
                            }
                        }
                    }
                }
                Value::Decimal(acc)
            }
            // decimal.min(a, b) / decimal.max(a, b): exact ordered selection.
            ("decimal", "min") => {
                let a = self.decimal_arg(args, 0);
                let b = self.decimal_arg(args, 1);
                Value::Decimal(if a.cmp(&b) == std::cmp::Ordering::Greater { b } else { a })
            }
            ("decimal", "max") => {
                let a = self.decimal_arg(args, 0);
                let b = self.decimal_arg(args, 1);
                Value::Decimal(if a.cmp(&b) == std::cmp::Ordering::Less { b } else { a })
            }
            // decimal.percent(a, pct): a * pct / 100, kept exact at a generous
            // scale then caller can to_fixed(...) — handy for tax/interest.
            ("decimal", "percent") => {
                let a = self.decimal_arg(args, 0);
                let pct = self.decimal_arg(args, 1);
                let hundred = crate::support::decimal::Decimal::from_int(100);
                match a.mul(&pct).and_then(|p| {
                    let scale = p.scale().max(2);
                    p.div(&hundred, scale, Rounding::HalfUp)
                }) {
                    Ok(d) => Value::Decimal(d),
                    Err(e) => runtime_error("E1003", &e, "reduce operand size or scale"),
                }
            }

            // --- CONCURRENCY (channels: bounded + rendezvous) ----------------
            // chan(capacity) -> handle. capacity 0 => rendezvous (R11.1, R11.6).
            ("concurrency", "chan") => {
                let cap = self.eval_arg_int(args, 0, 0).max(0) as usize;
                let id = crate::stdlib::concurrency::chan_create(cap);
                Value::Int(id as i64)
            }
            // send(handle, value): blocks if a bounded buffer is full; a closed
            // channel / dropped receiver yields a handleable E0611 error value
            // (R11.2, R11.6, R11.7).
            ("concurrency", "send") => {
                let id = self.eval_arg_int(args, 0, 0) as u64;
                let value = self.eval_arg_val(args, 1);
                use crate::stdlib::concurrency::SendOutcome;
                match crate::stdlib::concurrency::chan_send(id, value) {
                    SendOutcome::Ok => Value::Bool(true),
                    SendOutcome::Closed => Self::concurrency_send_closed(id),
                    SendOutcome::InvalidHandle => Self::concurrency_send_closed(id),
                }
            }
            // recv(handle) -> value | Closed indicator. Blocks while empty/open;
            // returns the distinguishable closed indicator once all senders are
            // closed and the buffer is drained (R11.3, R11.4).
            ("concurrency", "recv") => {
                let id = self.eval_arg_int(args, 0, 0) as u64;
                use crate::stdlib::concurrency::RecvOutcome;
                match crate::stdlib::concurrency::chan_recv(id) {
                    RecvOutcome::Value(v) => v,
                    RecvOutcome::Closed => Self::channel_closed_indicator(),
                }
            }
            // close(handle): close the sending endpoint (R11.4, R11.7).
            ("concurrency", "close") => {
                let id = self.eval_arg_int(args, 0, 0) as u64;
                Value::Bool(crate::stdlib::concurrency::chan_close(id))
            }
            // is_closed(value): true when `value` is the recv closed indicator.
            ("concurrency", "is_closed") => {
                let v = self.eval_arg_val(args, 0);
                Value::Bool(Self::is_channel_closed_indicator(&v))
            }
            // last_thread() -> handle id of the most recently spawned thread on
            // this thread. `spawn` is a statement, so the id is captured here
            // right after a `spawn { }` to obtain a join handle (R12.1).
            ("concurrency", "last_thread") => {
                let id = LAST_THREAD_ID.with(|c| c.get());
                Value::Int(id as i64)
            }
            // join(handle): block until the thread finishes and return its
            // result value (R12.2). Re-joining or an invalid handle yields a
            // handleable E0612 error value without blocking (R12.3). A faulting
            // thread body is delivered here as an error value (R12.6).
            ("concurrency", "join") => {
                let id = self.eval_arg_int(args, 0, 0) as u64;
                use crate::stdlib::concurrency::JoinOutcome;
                match crate::stdlib::concurrency::join_thread(id) {
                    JoinOutcome::Value(v) => v,
                    JoinOutcome::Invalid => Self::concurrency_join_invalid(id),
                    JoinOutcome::Panicked => Self::concurrency_thread_panicked(id),
                }
            }

            // --- CONCURRENCY (wait groups: add / done / wait) ----------------
            // waitgroup() -> handle id of a fresh wait group (R12.4).
            ("concurrency", "waitgroup") => {
                let id = crate::stdlib::concurrency::waitgroup_create();
                Value::Int(id as i64)
            }
            // add(wg, n): add n to the counter (n clamped to 0..=65535, R12.4).
            ("concurrency", "add") => {
                let id = self.eval_arg_int(args, 0, 0) as u64;
                let n = self.eval_arg_int(args, 1, 0);
                use crate::stdlib::concurrency::WgOutcome;
                match crate::stdlib::concurrency::wg_add(id, n) {
                    WgOutcome::Ok => Value::Bool(true),
                    WgOutcome::Negative => Self::concurrency_waitgroup_negative(id),
                    WgOutcome::InvalidHandle => Self::concurrency_waitgroup_invalid(id),
                }
            }
            // done(wg): decrement the counter. Calling `done` more often than
            // `add` drives the counter negative and yields a handleable E0610
            // error value without underflowing (R12.5).
            ("concurrency", "done") => {
                let id = self.eval_arg_int(args, 0, 0) as u64;
                use crate::stdlib::concurrency::WgOutcome;
                match crate::stdlib::concurrency::wg_done(id) {
                    WgOutcome::Ok => Value::Bool(true),
                    WgOutcome::Negative => Self::concurrency_waitgroup_negative(id),
                    WgOutcome::InvalidHandle => Self::concurrency_waitgroup_invalid(id),
                }
            }
            // wait(wg): block until the counter reaches zero; returns
            // immediately when it is already zero (R12.4).
            ("concurrency", "wait") => {
                let id = self.eval_arg_int(args, 0, 0) as u64;
                use crate::stdlib::concurrency::WgOutcome;
                match crate::stdlib::concurrency::wg_wait(id) {
                    WgOutcome::Ok => Value::Bool(true),
                    WgOutcome::Negative => Self::concurrency_waitgroup_negative(id),
                    WgOutcome::InvalidHandle => Self::concurrency_waitgroup_invalid(id),
                }
            }

            // --- CONCURRENCY (shared state: shared / lock-scoped access) -----
            // shared(v) -> handle id of a fresh Arc<Mutex<Value>> initialized to
            // v (R13.1). Every thread holding the id refers to the same value,
            // so all access is serialized through the mutex.
            ("concurrency", "shared") => {
                let v = self.eval_arg_val(args, 0);
                let id = crate::stdlib::concurrency::shared_create(v);
                Value::Int(id as i64)
            }
            // shared_get(s): acquire the lock, return a clone of the current
            // value, release (R13.1/R13.5). Acquisition uses a try_lock loop with
            // a 30s deadline; a timeout yields a handleable E0614 error value
            // without crashing the process (R13.3/R13.4).
            ("concurrency", "shared_get") => {
                let id = self.eval_arg_int(args, 0, 0) as u64;
                use crate::stdlib::concurrency::LockOutcome;
                match crate::stdlib::concurrency::shared_get(id) {
                    LockOutcome::Value(v) => v,
                    LockOutcome::TimedOut => Self::concurrency_lock_timeout(id),
                    LockOutcome::InvalidHandle => Self::concurrency_shared_invalid(id),
                }
            }
            // shared_set(s, v): acquire the lock, store v, release (R13.1/R13.2/
            // R13.5). Returns true on success.
            ("concurrency", "shared_set") => {
                let id = self.eval_arg_int(args, 0, 0) as u64;
                let v = self.eval_arg_val(args, 1);
                use crate::stdlib::concurrency::LockOutcome;
                match crate::stdlib::concurrency::shared_set(id, v) {
                    LockOutcome::Value(_) => Value::Bool(true),
                    LockOutcome::TimedOut => Self::concurrency_lock_timeout(id),
                    LockOutcome::InvalidHandle => Self::concurrency_shared_invalid(id),
                }
            }
            // shared_add(s, n): atomic read-modify-write that adds n while
            // holding the lock and returns the new value (R13.1/R13.2/R13.5).
            // Concurrent increments never lose updates.
            ("concurrency", "shared_add") => {
                let id = self.eval_arg_int(args, 0, 0) as u64;
                let delta = self.eval_arg_int(args, 1, 0);
                use crate::stdlib::concurrency::LockOutcome;
                match crate::stdlib::concurrency::shared_add(id, delta) {
                    LockOutcome::Value(v) => v,
                    LockOutcome::TimedOut => Self::concurrency_lock_timeout(id),
                    LockOutcome::InvalidHandle => Self::concurrency_shared_invalid(id),
                }
            }

            // --- Kelompok A: WEB (penyajian web native) ----------------------
            // web.serve(dir [, port]) — set Web_Root to `dir` and start the
            // built-in web server serving static assets from it (R1.1/R5.1).
            // Blocks like http.server. Missing Web_Root → E0401, no serving.
            ("web", "serve") => {
                let dir = self.eval_arg_str(args, 0, "public");
                let default_port = std::env::var("RAN_PORT")
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(8080);
                let port = self.eval_arg_int(args, 1, default_port) as u16;
                self.builtin_web_serve(&dir, port);
                Value::Void
            }
            // web.spa(enabled) — enable/disable SPA fallback (R2.1/R2.2). The
            // flag is stored and applied when the server is started.
            ("web", "spa") => {
                let enabled = self.eval_arg_val(args, 0).is_truthy_val();
                WEB_SPA.with(|c| c.set(enabled));
                Value::Void
            }
            // web.build(cmd) — record the Frontend_Build command to run before
            // serving (R5.2). The actual run lands in task 2.4; here we store
            // the command string and return Void.
            ("web", "build") => {
                let cmd = self.eval_arg_str(args, 0, "");
                WEB_BUILD_CMD.with(|c| {
                    *c.borrow_mut() = if cmd.is_empty() { None } else { Some(cmd) };
                });
                Value::Void
            }

            // --- Kelompok B: DB (SQLite native) ------------------------------
            // db.connect(path) -> Int(handle). Open/create the SQLite file and
            // register the connection; any DbError (libsqlite3 absent → E0501,
            // permission → E0501, not-a-db → E0502) becomes a handleable error
            // value without crashing (R6.1/R6.2/R6.3/R6.4).
            ("db", "connect") => {
                let path = self.eval_arg_str(args, 0, "");
                match crate::stdlib::db::connect(&path) {
                    Ok(id) => Value::Int(id as i64),
                    Err(e) => Self::db_error_value(&e),
                }
            }
            // db.close(handle) — close + remove from the registry (R6.5).
            // Invalid handle → handleable E0503 (R6.6).
            ("db", "close") => {
                let handle = self.eval_arg_int(args, 0, 0) as u64;
                match crate::stdlib::db::close(handle) {
                    Ok(()) => Value::Bool(true),
                    Err(e) => Self::db_error_value(&e),
                }
            }
            // db.query(handle, sql, params) -> array<map>. Parameters are always
            // bound (never interpolated, R7.4); zero rows → empty array (R7.2).
            ("db", "query") => {
                let handle = self.eval_arg_int(args, 0, 0) as u64;
                let sql = self.eval_arg_str(args, 1, "");
                let params = match self.eval_db_params(args, 2) {
                    Ok(p) => p,
                    Err(v) => return v,
                };
                match crate::stdlib::db::query(handle, &sql, &params) {
                    Ok(rows) => Self::db_rows_to_value(rows),
                    Err(e) => Self::db_error_value(&e),
                }
            }
            // db.exec(handle, sql, params) -> Int(affected) (R7.3).
            ("db", "exec") => {
                let handle = self.eval_arg_int(args, 0, 0) as u64;
                let sql = self.eval_arg_str(args, 1, "");
                let params = match self.eval_db_params(args, 2) {
                    Ok(p) => p,
                    Err(v) => return v,
                };
                match crate::stdlib::db::exec(handle, &sql, &params) {
                    Ok(affected) => Value::Int(affected),
                    Err(e) => Self::db_error_value(&e),
                }
            }
            // db.begin/commit/rollback(handle) -> Bool(true) on success
            // (R9.1/R9.3/R9.4); else a handleable error value.
            ("db", "begin") => {
                let handle = self.eval_arg_int(args, 0, 0) as u64;
                match crate::stdlib::db::begin(handle) {
                    Ok(()) => Value::Bool(true),
                    Err(e) => Self::db_error_value(&e),
                }
            }
            ("db", "commit") => {
                let handle = self.eval_arg_int(args, 0, 0) as u64;
                match crate::stdlib::db::commit(handle) {
                    Ok(()) => Value::Bool(true),
                    Err(e) => Self::db_error_value(&e),
                }
            }
            ("db", "rollback") => {
                let handle = self.eval_arg_int(args, 0, 0) as u64;
                match crate::stdlib::db::rollback(handle) {
                    Ok(()) => Value::Bool(true),
                    Err(e) => Self::db_error_value(&e),
                }
            }

            _ => Value::Void,
        }
    }
}
