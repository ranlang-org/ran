//! Module system - Go-style, one file = one module.
//!
//! Resolution:
//!   import "math"        -> stdlib module
//!   import "./utils"     -> local file (./utils.ran)
//!   import "github.com/user/pkg"  -> remote package (future)
//!
//! Each module has its own namespace. Exported symbols (pub) are accessible
//! from the importing module via `module.symbol`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Module registry - tracks loaded modules
pub struct ModuleRegistry {
    /// module_path -> Module
    modules: HashMap<String, Module>,
    /// Search paths for module resolution
    search_paths: Vec<PathBuf>,
}

/// A loaded module
#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub path: String,
    pub source: String,
    pub exports: Vec<String>,
    pub loaded: bool,
}

/// Module resolution result
#[derive(Debug)]
pub enum ResolveResult {
    /// Found as a local file
    LocalFile(PathBuf),
    /// Found as stdlib module
    Stdlib(String),
    /// Found as package (future)
    Package(String),
    /// Not found
    NotFound(String),
}

impl ModuleRegistry {
    pub fn new() -> Self {
        Self {
            modules: HashMap::new(),
            search_paths: vec![
                PathBuf::from("."),
                PathBuf::from("./lib"),
                PathBuf::from("./modules"),
            ],
        }
    }

    /// Resolve an import path to a file
    pub fn resolve(&self, import_path: &str, from_file: &str) -> ResolveResult {
        // Standard library: canonical form is `std::<name>`. A bare known name
        // is also treated as stdlib here (the analyzer rejects it with E0006).
        if Self::is_stdlib(import_path) {
            return ResolveResult::Stdlib(import_path.to_string());
        }

        // Relative import: starts with ./ or ../ (parent traversal allowed).
        if import_path.starts_with('.') {
            let from_dir = Path::new(from_file).parent().unwrap_or(Path::new("."));
            // Honor an explicit `.ran` extension; otherwise append it.
            let target = if import_path.ends_with(".ran") {
                from_dir.join(import_path)
            } else {
                from_dir.join(format!("{}.ran", import_path))
            };
            if target.exists() {
                return ResolveResult::LocalFile(target);
            }
            // Try a directory module: `<path>/mod.ran`.
            let dir_mod = from_dir
                .join(import_path.trim_end_matches(".ran"))
                .join("mod.ran");
            if dir_mod.exists() {
                return ResolveResult::LocalFile(dir_mod);
            }
            return ResolveResult::NotFound(format!(
                "cannot find module '{}' (looked for {})",
                import_path,
                target.display()
            ));
        }

        // Search in search paths (bare local module names).
        for search_path in &self.search_paths {
            let target = if import_path.ends_with(".ran") {
                search_path.join(import_path)
            } else {
                search_path.join(format!("{}.ran", import_path))
            };
            if target.exists() {
                return ResolveResult::LocalFile(target);
            }
        }

        // Package (future): host/user/pkg
        if import_path.contains('/') {
            return ResolveResult::Package(import_path.to_string());
        }

        ResolveResult::NotFound(format!("module '{}' not found", import_path))
    }

    /// Check if a module name is part of stdlib (accepts `std::name` or bare).
    fn is_stdlib(name: &str) -> bool {
        let n = name.strip_prefix("std::").unwrap_or(name);
        matches!(
            n,
            "http" | "time" | "fs" | "json" | "os" | "math"
                | "html" | "str" | "rand" | "log" | "decimal" | "env" | "crypto"
                | "concurrency" | "web" | "db"
        )
    }

    /// Register a loaded module
    pub fn register(&mut self, module: Module) {
        self.modules.insert(module.path.clone(), module);
    }

    /// Get a module by path
    pub fn get(&self, path: &str) -> Option<&Module> {
        self.modules.get(path)
    }

    /// Check if module is already loaded
    pub fn is_loaded(&self, path: &str) -> bool {
        self.modules.contains_key(path)
    }
}

// ============================================================================
// Module loader - resolves imports and merges into a single Program
// ============================================================================

use crate::frontend::ast::{Program, Statement};
use std::collections::HashSet;
use std::fs;

/// Load a program from an entry file, resolving all `import` statements.
///
/// Local file imports (`import "./utils"`) are loaded, parsed, and their
/// declarations merged into the resulting program. Stdlib imports
/// (`import "http"`) are left as-is (handled natively by the runtime).
///
/// Returns the merged Program. Reports import errors to stderr.
pub fn load_program(entry_file: &str) -> Option<Program> {
    let source = fs::read_to_string(entry_file).ok()?;
    let mut visited = HashSet::new();
    visited.insert(entry_file.to_string());

    let mut merged = Vec::new();
    let registry = ModuleRegistry::new();

    load_recursive(&source, entry_file, &registry, &mut visited, &mut merged);

    Some(Program { statements: merged })
}

/// Build a single, self-contained Ran source string equivalent to the fully
/// merged program, for embedding into a standalone binary (`ran build`).
///
/// `load_program` produces a merged *AST*, but the compiled-binary runtime
/// re-parses an embedded *source string* (it does not re-run the module
/// resolver, since the imported files are not shipped alongside the binary).
/// So a build must embed the merged source, not just the entry file — otherwise
/// declarations from imported local files are missing at runtime (handlers
/// resolve to nothing and return `()`).
///
/// The result inlines every local-file import (their bodies are concatenated,
/// dependencies first) and keeps a single, de-duplicated set of stdlib imports
/// at the top. Returns `None` if the entry file cannot be read.
pub fn load_merged_source(entry_file: &str) -> Option<String> {
    let source = fs::read_to_string(entry_file).ok()?;
    let mut visited = HashSet::new();
    visited.insert(entry_file.to_string());

    let registry = ModuleRegistry::new();
    let mut body = String::new();
    // De-duplicated stdlib imports, in first-seen order: (path, alias).
    let mut stdlib_imports: Vec<(String, Option<String>)> = Vec::new();

    merge_source_recursive(
        &source,
        entry_file,
        &registry,
        &mut visited,
        &mut body,
        &mut stdlib_imports,
    );

    // Reconstruct a single header of stdlib imports.
    let mut header = String::new();
    for (path, alias) in &stdlib_imports {
        match alias {
            Some(a) => header.push_str(&format!("import \"{}\" as {}\n", path, a)),
            None => header.push_str(&format!("import \"{}\"\n", path)),
        }
    }
    Some(format!("{}{}", header, body))
}

/// Recurse like `load_recursive`, but accumulate *source text*: inline local
/// imports (dependencies first), collect de-duplicated stdlib imports, and drop
/// every `import` line from each file's emitted body.
fn merge_source_recursive(
    source: &str,
    from_file: &str,
    registry: &ModuleRegistry,
    visited: &mut HashSet<String>,
    body: &mut String,
    stdlib_imports: &mut Vec<(String, Option<String>)>,
) {
    let tokens = crate::frontend::lexer::tokenize(source);
    let (program, syntax_diags) = crate::frontend::parser::parse_checked(tokens);
    crate::support::diagnostics::abort_on_syntax_errors(syntax_diags, from_file, source);

    // 1-based line numbers that hold an `import` statement (stripped from the
    // emitted body; imports are single-line by convention).
    let mut import_lines: HashSet<usize> = HashSet::new();

    for stmt in &program.statements {
        if let Statement::Import { path, alias } = &stmt.kind {
            import_lines.insert(stmt.span.line);
            match registry.resolve(path, from_file) {
                ResolveResult::LocalFile(target) => {
                    let target_str = target.to_string_lossy().to_string();
                    if visited.contains(&target_str) {
                        continue;
                    }
                    visited.insert(target_str.clone());
                    if let Ok(imported_src) = fs::read_to_string(&target) {
                        // Dependencies first: inline the imported file's body
                        // before this file's own body.
                        merge_source_recursive(
                            &imported_src,
                            &target_str,
                            registry,
                            visited,
                            body,
                            stdlib_imports,
                        );
                    } else {
                        eprintln!("ran: cannot read imported module '{}'", target_str);
                    }
                }
                ResolveResult::Stdlib(_) => {
                    let entry = (path.clone(), alias.clone());
                    if !stdlib_imports.contains(&entry) {
                        stdlib_imports.push(entry);
                    }
                }
                ResolveResult::Package(pkg) => {
                    eprintln!("ran: remote packages not yet supported: '{}'", pkg);
                }
                ResolveResult::NotFound(msg) => {
                    eprintln!("\x1b[31;1merror\x1b[0m: {}", msg);
                }
            }
        }
    }

    // Emit this file's body with every import line removed.
    for (i, line) in source.lines().enumerate() {
        if import_lines.contains(&(i + 1)) {
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
}

/// Recursively load a source's imports, appending declarations to `merged`.
fn load_recursive(
    source: &str,
    from_file: &str,
    registry: &ModuleRegistry,
    visited: &mut HashSet<String>,
    merged: &mut Vec<crate::frontend::ast::Stmt>,
) {
    let tokens = crate::frontend::lexer::tokenize(source);
    let (program, syntax_diags) = crate::frontend::parser::parse_checked(tokens);
    // Abort immediately if this file has syntax errors, with correct source context.
    crate::support::diagnostics::abort_on_syntax_errors(syntax_diags, from_file, source);

    for stmt in program.statements {
        if let Statement::Import { ref path, .. } = stmt.kind {
            // Resolve and load local-file imports
            match registry.resolve(path, from_file) {
                ResolveResult::LocalFile(target) => {
                    let target_str = target.to_string_lossy().to_string();
                    if visited.contains(&target_str) {
                        continue; // already loaded - avoid cycles
                    }
                    visited.insert(target_str.clone());

                    if let Ok(imported_src) = fs::read_to_string(&target) {
                        load_recursive(&imported_src, &target_str, registry, visited, merged);
                    } else {
                        eprintln!("ran: cannot read imported module '{}'", target_str);
                    }
                }
                ResolveResult::Stdlib(_) => {
                    // Stdlib modules are handled natively - keep the import
                    merged.push(stmt);
                }
                ResolveResult::Package(pkg) => {
                    eprintln!("ran: remote packages not yet supported: '{}'", pkg);
                }
                ResolveResult::NotFound(msg) => {
                    eprintln!("\x1b[31;1merror\x1b[0m: {}", msg);
                }
            }
        } else {
            merged.push(stmt);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a built binary embeds the *merged* source, so declarations
    /// from local imports must be inlined and local `import "./..."` lines must
    /// be gone, while stdlib imports survive (de-duplicated) at the top.
    #[test]
    fn merged_source_inlines_local_imports() {
        let dir = std::env::temp_dir().join(format!("ran_merge_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let entry = dir.join("app.ran");
        let helper = dir.join("helper.ran");

        fs::write(
            &helper,
            "import \"std::log\" as log\nfn greeting() {\n    return \"hi from helper\"\n}\n",
        )
        .unwrap();
        fs::write(
            &entry,
            "import \"std::http\" as http\nimport \"./helper\"\nfn main() {\n    echo greeting()\n}\n",
        )
        .unwrap();

        let merged = load_merged_source(entry.to_str().unwrap()).expect("merged source");

        // The imported function is inlined.
        assert!(merged.contains("fn greeting()"), "merged:\n{}", merged);
        assert!(merged.contains("fn main()"), "merged:\n{}", merged);
        // The local import line is stripped (no `./helper` import remains).
        assert!(!merged.contains("\"./helper\""), "merged:\n{}", merged);
        // Stdlib imports survive, de-duplicated.
        assert!(merged.contains("import \"std::http\" as http"));
        assert!(merged.contains("import \"std::log\" as log"));

        let _ = fs::remove_dir_all(&dir);
    }
}
