#![allow(unused)]
//! Bytecode compiler - transforms Ran AST into bytecode Chunks for the VM.

use std::collections::HashMap;
use std::sync::Arc;

use super::{Chunk, OpCode, VMValue};
use crate::frontend::ast::*;

/// Result of compilation
#[derive(Debug)]
pub struct CompileResult {
    /// All compiled chunks. Index 0 is always the main/top-level chunk.
    pub chunks: Vec<Chunk>,
    /// Global variable names (index corresponds to GLOAD/GSTORE operand)
    pub global_names: Vec<String>,
}

/// Tracks a local variable during compilation
#[derive(Debug, Clone)]
struct Local {
    name: String,
    depth: usize,
}

/// Scope for tracking local variables within a function/block
#[derive(Debug)]
struct Scope {
    locals: Vec<Local>,
    depth: usize,
    /// Name recorded for each slot index ever allocated (most-recent wins on
    /// reuse). Captured into `Chunk::local_names` so the VM can resolve `$name`
    /// interpolation against a frame's locals.
    slot_names: Vec<String>,
}

impl Scope {
    fn new() -> Self {
        Self {
            locals: Vec::new(),
            depth: 0,
            slot_names: Vec::new(),
        }
    }

    fn begin(&mut self) {
        self.depth += 1;
    }

    fn end(&mut self) -> usize {
        let mut popped = 0;
        while let Some(local) = self.locals.last() {
            if local.depth < self.depth {
                break;
            }
            self.locals.pop();
            popped += 1;
        }
        self.depth -= 1;
        popped
    }

    fn add_local(&mut self, name: &str) -> u16 {
        let slot = self.locals.len() as u16;
        self.locals.push(Local {
            name: name.to_string(),
            depth: self.depth,
        });
        // Record the name for this slot index (grow or overwrite on reuse) so
        // the VM has a slot -> name map for interpolation.
        let s = slot as usize;
        if s >= self.slot_names.len() {
            self.slot_names.resize(s + 1, String::new());
        }
        self.slot_names[s] = name.to_string();
        slot
    }

    fn resolve_local(&self, name: &str) -> Option<u16> {
        for (i, local) in self.locals.iter().enumerate().rev() {
            if local.name == name {
                return Some(i as u16);
            }
        }
        None
    }
}

/// Known module names for ModCall resolution
const KNOWN_MODULES: &[&str] = &[
    "io", "fs", "http", "net", "json", "math", "os", "time",
    "crypto", "fmt", "log", "chan", "sync", "env",
];

/// The bytecode compiler
pub struct BytecodeCompiler {
    /// All compiled chunks
    chunks: Vec<Chunk>,
    /// Index of the chunk currently being compiled
    current_chunk: usize,
    /// Scope stack for the current function
    scope: Scope,
    /// Global variable names
    global_names: Vec<String>,
    /// Module name -> index mapping
    module_indices: HashMap<String, u16>,
    /// Method name -> index mapping per module
    method_indices: HashMap<String, HashMap<String, u16>>,
    /// Function name -> chunk index mapping
    fn_chunks: HashMap<String, usize>,
    /// Function name -> declared parameter count (arity), for correct CALL.
    fn_arity: HashMap<String, u8>,
    /// Names declared as enums (first pass). Used so `EnumName.Variant` field
    /// access falls back to the interpreter, which builds the variant value.
    enum_names: std::collections::HashSet<String>,
    /// Conservative compiler-local int inference (task 12.2 / R9.3). Names of
    /// LOCAL variables in the current function scope that are statically known
    /// to hold an integer (seeded from `: int` params, int literals, and the
    /// results of integer arithmetic). Used to decide when to emit
    /// type-specialized opcodes (AddInt/LtInt/...). Saved/restored alongside
    /// `scope` at every function boundary. Soundness note: even if this set is
    /// imprecise, correctness is preserved — the specialized opcodes fall back
    /// to the identical generic operation for any non-int operand at run time.
    int_locals: std::collections::HashSet<String>,
    /// Same as `int_locals` but for top-level globals (GStore/GLoad).
    int_globals: std::collections::HashSet<String>,
    /// Current source line (for debug info)
    line: usize,
}

impl BytecodeCompiler {
    fn new() -> Self {
        let main_chunk = Chunk::new("<main>");
        let mut module_indices = HashMap::new();
        for (i, name) in KNOWN_MODULES.iter().enumerate() {
            module_indices.insert(name.to_string(), i as u16);
        }

        Self {
            chunks: vec![main_chunk],
            current_chunk: 0,
            scope: Scope::new(),
            global_names: Vec::new(),
            module_indices,
            method_indices: HashMap::new(),
            fn_chunks: HashMap::new(),
            fn_arity: HashMap::new(),
            enum_names: std::collections::HashSet::new(),
            int_locals: std::collections::HashSet::new(),
            int_globals: std::collections::HashSet::new(),
            line: 1,
        }
    }

    /// Main entry point: compile a full program
    pub fn compile(program: &Program) -> CompileResult {
        let mut compiler = Self::new();

        // First pass: register all function declarations so they can be referenced
        for stmt in &program.statements {
            if let Statement::FnDecl { name, params, .. } = &stmt.kind {
                let chunk_idx = compiler.chunks.len();
                compiler.chunks.push(Chunk::new(name));
                compiler.fn_chunks.insert(name.clone(), chunk_idx);
                compiler.fn_arity.insert(name.clone(), params.len() as u8);
            }
            // Record enum names so `EnumName.Variant` access can fall back.
            if let Statement::EnumDecl { name, .. } = &stmt.kind {
                compiler.enum_names.insert(name.clone());
            }
        }

        // Second pass: compile top-level items, mirroring the interpreter's
        // entry (`runtime::execute`) EXACTLY. The interpreter's first pass
        // registers declarations AND evaluates top-level `VarDecl`s (program
        // globals like `name = "World"`) and `Import`s — in source order — then
        // calls `fn main()`. It does NOT execute other free-standing top-level
        // statements (Echo/If/For/While/Expr/...). So here we compile
        // declarations, top-level `VarDecl`s (→ GStore globals), and imports,
        // and skip the rest — running the same code the interpreter does.
        //
        // Top-level `VarDecl` was previously skipped, which meant program-level
        // globals were never stored: `echo "Hello, $name!"` then found no
        // binding for `$name` and printed the literal `$name` instead of the
        // interpolated value — a correctness violation vs the interpreter.
        for stmt in &program.statements {
            match &stmt.kind {
                Statement::FnDecl { .. }
                | Statement::StructDecl { .. }
                | Statement::EnumDecl { .. }
                | Statement::ImplBlock { .. }
                | Statement::TraitDecl { .. }
                | Statement::VarDecl { .. }
                | Statement::Import { .. } => {
                    compiler.compile_statement(stmt);
                }
                // Free-standing executable top-level statement: ignored by the
                // interpreter, so ignored here too.
                _ => {}
            }
        }

        // Entry point: after any top-level statements in chunk 0, invoke `main`
        // so the VM actually runs the program (mirrors the interpreter). Without
        // this, chunk 0 was just `Halt` and the VM did nothing.
        if let Some(&main_idx) = compiler.fn_chunks.get("main") {
            compiler.current_chunk = 0;
            let fn_ref = VMValue::Function(super::value::FnRef {
                name: "main".to_string(),
                chunk_idx: main_idx,
                arity: 0,
            });
            let cidx = compiler.chunks[0].add_constant(fn_ref);
            let line = compiler.line;
            compiler.chunks[0].emit_u16(OpCode::Const, cidx, line);
            compiler.chunks[0].emit_u8(OpCode::Call, 0, line);
            compiler.chunks[0].emit(OpCode::Pop, line); // discard main's return value
        }

        // Emit Halt at the end of main
        let line = compiler.line;
        compiler.current_chunk = 0;
        compiler.chunk().emit(OpCode::Halt, line);

        // Set local_count on all chunks
        let local_count = compiler.scope.slot_names.len() as u16;
        compiler.chunks[0].local_count = local_count;
        compiler.chunks[0].local_names = compiler.scope.slot_names.clone();

        CompileResult {
            chunks: compiler.chunks,
            global_names: compiler.global_names,
        }
    }

    /// Emit an intentionally-unsupported opcode as a "fall back to the
    /// interpreter" marker. The `all_supported()` pre-flight sees the
    /// unsupported opcode and routes the whole program to the tree-walking
    /// interpreter, which executes the construct correctly (R6.3). `And`/`Or`
    /// are never emitted by real expressions (short-circuit uses jumps), so they
    /// are safe sentinels.
    fn emit_fallback_marker(&mut self) {
        let line = self.line;
        self.chunk().emit(OpCode::And, line);
    }

    // --- Helpers ---

    fn chunk(&mut self) -> &mut Chunk {
        &mut self.chunks[self.current_chunk]
    }

    fn resolve_global(&mut self, name: &str) -> u16 {
        if let Some(idx) = self.global_names.iter().position(|n| n == name) {
            return idx as u16;
        }
        let idx = self.global_names.len() as u16;
        self.global_names.push(name.to_string());
        idx
    }

    fn resolve_module(&mut self, name: &str) -> u16 {
        if let Some(&idx) = self.module_indices.get(name) {
            return idx;
        }
        let idx = self.module_indices.len() as u16;
        self.module_indices.insert(name.to_string(), idx);
        idx
    }

    fn resolve_method(&mut self, module: &str, method: &str) -> u16 {
        let methods = self
            .method_indices
            .entry(module.to_string())
            .or_insert_with(HashMap::new);
        if let Some(&idx) = methods.get(method) {
            return idx;
        }
        let idx = methods.len() as u16;
        methods.insert(method.to_string(), idx);
        idx
    }

    /// Emit a jump instruction with placeholder offset, return the position of the offset bytes
    fn emit_jump(&mut self, op: OpCode) -> usize {
        let line = self.line;
        self.chunk().emit_i16(op, 0, line);
        // The offset bytes start at code.len() - 2
        self.chunk().len() - 2
    }

    /// Patch a previously emitted jump to target the current position
    fn patch_jump_here(&mut self, offset_pos: usize) {
        let target = self.chunk().len();
        self.chunk().patch_jump(offset_pos, target);
    }

    // --- Statement Compilation ---

    fn compile_statement(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            Statement::VarDecl {
                name,
                mutable: _,
                type_annotation,
                value,
            } => self.compile_var_decl(name, type_annotation, value),

            Statement::FnDecl {
                name,
                params,
                return_type: _,
                body,
                is_pub: _,
                is_async: _,
            } => self.compile_fn_decl(name, params, body),

            Statement::StructDecl {
                name,
                fields,
                is_pub: _,
            } => self.compile_struct_decl(name, fields),

            Statement::EnumDecl { .. } => {
                // Enum declarations are type-level; no bytecode needed at runtime
            }

            Statement::ImplBlock {
                type_name,
                trait_name: _,
                methods,
            } => self.compile_impl_block(type_name, methods),

            // Trait declarations are a type-level/interpreter construct (default
            // method bodies + dispatch fallback). The VM has no opcode for them,
            // so emit an intentionally-unsupported opcode: the `all_supported()`
            // pre-flight then routes any program declaring a trait to the
            // interpreter, which executes it correctly (R6.3 / R8.6).
            Statement::TraitDecl { .. } => {
                let line = self.line;
                self.chunk().emit(OpCode::And, line);
            }

            Statement::Expr(expr) => {
                self.compile_expression(expr);
                // Pop the result unless it's already consumed
                let line = self.line;
                self.chunk().emit(OpCode::Pop, line);
            }

            Statement::Echo { expr, .. } => self.compile_echo(expr),

            Statement::Return(expr) => self.compile_return(expr),

            Statement::If {
                condition,
                then_body,
                else_body,
            } => self.compile_if(condition, then_body, else_body),

            Statement::For {
                variable,
                iterable,
                body,
            } => self.compile_for(variable, iterable, body),

            Statement::While { condition, body } => self.compile_while(condition, body),

            Statement::Spawn { body } => self.compile_spawn(body),

            Statement::Import { path, .. } => self.compile_import(path),

            // `break`/`continue` are handled by the tree-walking interpreter
            // (see runtime `Flow::Break`/`Flow::Continue`). The VM has no
            // loop-control opcodes yet, so emit an intentionally-unsupported
            // opcode: the `all_supported()` pre-flight then routes any program
            // using loop control to the interpreter, which executes it
            // correctly (R6.3) rather than the VM running it incorrectly.
            Statement::Break | Statement::Continue => {
                let line = self.line;
                self.chunk().emit(OpCode::And, line);
            }
        }
    }

    fn compile_var_decl(&mut self, name: &str, type_annotation: &Option<TypeExpr>, value: &Expression) {
        // Decide int-ness BEFORE storing: the value is evaluated in the current
        // binding environment, so infer against the pre-assignment state.
        let is_int = Self::type_is_int(type_annotation) || self.infer_int(value);

        self.compile_expression(value);
        let line = self.line;

        if self.scope.depth == 0 && self.current_chunk == 0 {
            // Global variable in the main chunk at top-level
            let idx = self.resolve_global(name);
            self.chunk().emit_u16(OpCode::GStore, idx, line);
            // Track int-ness for specialization (R9.3): a re-assignment with a
            // non-int value clears the flag, keeping inference conservative.
            if is_int {
                self.int_globals.insert(name.to_string());
            } else {
                self.int_globals.remove(name);
            }
        } else {
            // Local variable: reuse the existing slot when this is a
            // reassignment (e.g. `i = i + 1`); only allocate a new slot for a
            // genuinely new binding. (Allocating a fresh slot for every `=` was
            // a bug: `i = i + 1` stored to a new slot, leaving the loop variable
            // unchanged → infinite loop.)
            let slot = match self.scope.resolve_local(name) {
                Some(existing) => existing,
                None => self.scope.add_local(name),
            };
            self.chunk().emit_u16(OpCode::Store, slot, line);
            if is_int {
                self.int_locals.insert(name.to_string());
            } else {
                self.int_locals.remove(name);
            }
        }
    }

    fn compile_fn_decl(&mut self, name: &str, params: &[Param], body: &[Stmt]) {
        let chunk_idx = match self.fn_chunks.get(name) {
            Some(&idx) => idx,
            None => {
                // Shouldn't happen if first pass worked, but handle gracefully
                let idx = self.chunks.len();
                self.chunks.push(Chunk::new(name));
                self.fn_chunks.insert(name.to_string(), idx);
                idx
            }
        };

        // Save current compilation state
        let prev_chunk = self.current_chunk;
        let prev_scope = std::mem::replace(&mut self.scope, Scope::new());
        let prev_int_locals = std::mem::take(&mut self.int_locals);

        self.current_chunk = chunk_idx;

        // Register parameters as locals in the function scope
        for param in params {
            self.scope.add_local(&param.name);
            // Seed int inference from `: int` parameter annotations (R9.3).
            if Self::type_is_int(&param.type_annotation) {
                self.int_locals.insert(param.name.clone());
            }
        }

        // Compile function body
        for stmt in body {
            self.compile_statement(stmt);
        }

        // Ensure function ends with a return
        let line = self.line;
        let last_is_ret = self.chunk().code.last().copied()
            .and_then(OpCode::from_u8)
            .map(|op| op == OpCode::Ret || op == OpCode::RetVal)
            .unwrap_or(false);

        if !last_is_ret {
            self.chunk().emit(OpCode::Ret, line);
        }

        // Set local count for this function chunk
        let local_count = self.scope.slot_names.len() as u16;
        self.chunks[chunk_idx].local_count = local_count;
        self.chunks[chunk_idx].local_names = self.scope.slot_names.clone();

        // Restore previous state
        self.current_chunk = prev_chunk;
        self.scope = prev_scope;
        self.int_locals = prev_int_locals;
    }

    fn compile_struct_decl(&mut self, name: &str, fields: &[Field]) {
        // Register struct type as a global constant so it can be referenced
        let line = self.line;
        let name_idx = self.chunk().add_constant(VMValue::Str(Arc::new(name.to_string())));
        // Store struct field names as a constant for runtime validation
        for field in fields {
            self.chunk()
                .add_constant(VMValue::Str(Arc::new(field.name.clone())));
        }
        // No bytecode emitted; struct layout is used at StructInit time
        let _ = name_idx;
    }

    fn compile_impl_block(&mut self, type_name: &str, methods: &[Stmt]) {
        for method in methods {
            if let Statement::FnDecl {
                name,
                params,
                body,
                ..
            } = &method.kind
            {
                // Compile method as a function with a mangled name: TypeName::method
                let mangled = format!("{}::{}", type_name, name);

                let chunk_idx = if let Some(&idx) = self.fn_chunks.get(&mangled) {
                    idx
                } else {
                    let idx = self.chunks.len();
                    self.chunks.push(Chunk::new(&mangled));
                    self.fn_chunks.insert(mangled.clone(), idx);
                    idx
                };

                // Save state
                let prev_chunk = self.current_chunk;
                let prev_scope = std::mem::replace(&mut self.scope, Scope::new());
                let prev_int_locals = std::mem::take(&mut self.int_locals);
                self.current_chunk = chunk_idx;

                // 'self' is the first local (implicit parameter)
                self.scope.add_local("self");
                for param in params {
                    self.scope.add_local(&param.name);
                    if Self::type_is_int(&param.type_annotation) {
                        self.int_locals.insert(param.name.clone());
                    }
                }

                for stmt in body {
                    self.compile_statement(stmt);
                }

                let line = self.line;
                let last_is_ret = self.chunk().code.last().copied()
                    .and_then(OpCode::from_u8)
                    .map(|op| op == OpCode::Ret || op == OpCode::RetVal)
                    .unwrap_or(false);
                if !last_is_ret {
                    self.chunk().emit(OpCode::Ret, line);
                }

                let local_count = self.scope.slot_names.len() as u16;
                self.chunks[chunk_idx].local_count = local_count;
                self.chunks[chunk_idx].local_names = self.scope.slot_names.clone();

                self.current_chunk = prev_chunk;
                self.scope = prev_scope;
                self.int_locals = prev_int_locals;
            }
        }
    }

    fn compile_echo(&mut self, expr: &Expression) {
        // Interpolation is performed at run time by the VM's `Echo` (mirroring
        // the interpreter, which interpolates the *formatted* value against the
        // live scope). So just push the value and emit Echo — string literals
        // are pushed verbatim, exactly like the interpreter evaluates them.
        self.compile_expression(expr);
        let line = self.line;
        self.chunk().emit(OpCode::Echo, line);
    }

    fn compile_return(&mut self, expr: &Option<Expression>) {
        let line = self.line;
        match expr {
            Some(e) => {
                self.compile_expression(e);
                self.chunk().emit(OpCode::RetVal, line);
            }
            None => {
                self.chunk().emit(OpCode::Ret, line);
            }
        }
    }

    fn compile_if(
        &mut self,
        condition: &Expression,
        then_body: &[Stmt],
        else_body: &Option<Vec<Stmt>>,
    ) {
        self.compile_expression(condition);

        // Jump to else (or end) if condition is false
        let jump_to_else = self.emit_jump(OpCode::JmpFalse);

        // Compile then branch. Block-scoped locals are addressed by absolute
        // slot and cleaned up when the frame returns (Ret/RetVal truncates the
        // whole frame), so we do NOT pop them here — emitting a fixed number of
        // Pops is wrong when a branch/loop runs zero times. `scope.begin/end`
        // still governs name visibility.
        self.scope.begin();
        for stmt in then_body {
            self.compile_statement(stmt);
        }
        self.scope.end();

        if let Some(else_stmts) = else_body {
            // Jump over else branch at end of then
            let jump_over_else = self.emit_jump(OpCode::Jmp);

            // Patch the conditional jump to land here (start of else)
            self.patch_jump_here(jump_to_else);

            // Compile else branch
            self.scope.begin();
            for stmt in else_stmts {
                self.compile_statement(stmt);
            }
            self.scope.end();

            // Patch the unconditional jump to land here (after else)
            self.patch_jump_here(jump_over_else);
        } else {
            // No else branch: patch conditional jump to here
            self.patch_jump_here(jump_to_else);
        }
    }

    fn compile_while(&mut self, condition: &Expression, body: &[Stmt]) {
        let loop_start = self.chunk().len();

        // Compile condition
        self.compile_expression(condition);

        // Jump out if false
        let exit_jump = self.emit_jump(OpCode::JmpFalse);

        // Compile body (locals addressed absolutely; no per-iteration Pop).
        self.scope.begin();
        for stmt in body {
            self.compile_statement(stmt);
        }
        self.scope.end();
        let line = self.line;

        // Jump back to loop start
        let current_pos = self.chunk().len();
        let offset = loop_start as isize - (current_pos as isize + 3);
        self.chunk().emit_i16(OpCode::Jmp, offset as i16, line);

        // Patch exit jump
        self.patch_jump_here(exit_jump);
    }

    fn compile_for(&mut self, variable: &str, iterable: &Expression, body: &[Stmt]) {
        let line = self.line;

        // Fast path: `for x in range(...)` — iterate numerically without
        // materializing an array, matching the interpreter's `as_range_bounds`
        // (range(n) => 0..n, range(a, b) => a..b). Skipped if the user defined
        // their own `range` function.
        if let Expression::FnCall { callee, args } = iterable {
            if let Expression::Variable(fname) = callee.as_ref() {
                if fname == "range"
                    && !self.fn_chunks.contains_key("range")
                    && (args.len() == 1 || args.len() == 2)
                {
                    self.scope.begin();
                    let var_slot = self.scope.add_local(variable);
                    let end_slot = self.scope.add_local("__end__");

                    // The loop counter is an integer exactly when the range
                    // bounds are integers (R9.3): specialize the comparison and
                    // increment, and mark `variable` int so the body's uses of
                    // it specialize too.
                    let bounds_int = args.iter().all(|a| self.infer_int(a));
                    if bounds_int {
                        self.int_locals.insert(variable.to_string());
                    }

                    // Push start then end; store end, then start into var.
                    if args.len() == 2 {
                        self.compile_expression(&args[0]);
                        self.compile_expression(&args[1]);
                    } else {
                        let zero = self.chunk().add_constant(VMValue::Int(0));
                        self.chunk().emit_u16(OpCode::Const, zero, line);
                        self.compile_expression(&args[0]);
                    }
                    self.chunk().emit_u16(OpCode::Store, end_slot, line);
                    self.chunk().emit_u16(OpCode::Store, var_slot, line);

                    let loop_start = self.chunk().len();
                    // cond: var < end
                    self.chunk().emit_u16(OpCode::Load, var_slot, line);
                    self.chunk().emit_u16(OpCode::Load, end_slot, line);
                    self.chunk().emit(if bounds_int { OpCode::LtInt } else { OpCode::Lt }, line);
                    let exit_jump = self.emit_jump(OpCode::JmpFalse);

                    // body
                    for stmt in body {
                        self.compile_statement(stmt);
                    }

                    // var = var + 1
                    let one = self.chunk().add_constant(VMValue::Int(1));
                    self.chunk().emit_u16(OpCode::Load, var_slot, line);
                    self.chunk().emit_u16(OpCode::Const, one, line);
                    self.chunk().emit(if bounds_int { OpCode::AddInt } else { OpCode::Add }, line);
                    self.chunk().emit_u16(OpCode::Store, var_slot, line);

                    let cur = self.chunk().len();
                    let off = loop_start as isize - (cur as isize + 3);
                    self.chunk().emit_i16(OpCode::Jmp, off as i16, line);
                    self.patch_jump_here(exit_jump);
                    self.scope.end();
                    return;
                }
            }
        }

        // General path: iterate an evaluated array (or string/map) by index,
        // bounded by `Len` so the loop always terminates and never indexes out
        // of bounds.
        self.scope.begin();
        let iter_slot = self.scope.add_local("__iter__");
        let idx_slot = self.scope.add_local("__idx__");
        let var_slot = self.scope.add_local(variable);

        // iter = <iterable>; idx = 0
        self.compile_expression(iterable);
        self.chunk().emit_u16(OpCode::Store, iter_slot, line);
        let zero_idx = self.chunk().add_constant(VMValue::Int(0));
        self.chunk().emit_u16(OpCode::Const, zero_idx, line);
        self.chunk().emit_u16(OpCode::Store, idx_slot, line);

        let loop_start = self.chunk().len();
        // cond: idx < len(iter)  — idx and Len are always integers, so this
        // comparison is unconditionally int-specializable (R9.3).
        self.chunk().emit_u16(OpCode::Load, idx_slot, line);
        self.chunk().emit_u16(OpCode::Load, iter_slot, line);
        self.chunk().emit(OpCode::Len, line);
        self.chunk().emit(OpCode::LtInt, line);
        let exit_jump = self.emit_jump(OpCode::JmpFalse);

        // var = iter[idx]
        self.chunk().emit_u16(OpCode::Load, iter_slot, line);
        self.chunk().emit_u16(OpCode::Load, idx_slot, line);
        self.chunk().emit(OpCode::Index, line);
        self.chunk().emit_u16(OpCode::Store, var_slot, line);

        // body
        for stmt in body {
            self.compile_statement(stmt);
        }

        // idx = idx + 1  — idx is always an integer, so specialize (R9.3).
        let one_idx = self.chunk().add_constant(VMValue::Int(1));
        self.chunk().emit_u16(OpCode::Load, idx_slot, line);
        self.chunk().emit_u16(OpCode::Const, one_idx, line);
        self.chunk().emit(OpCode::AddInt, line);
        self.chunk().emit_u16(OpCode::Store, idx_slot, line);

        let current_pos = self.chunk().len();
        let offset = loop_start as isize - (current_pos as isize + 3);
        self.chunk().emit_i16(OpCode::Jmp, offset as i16, line);

        self.patch_jump_here(exit_jump);
        self.scope.end();
    }

    fn compile_spawn(&mut self, body: &[Stmt]) {
        // Compile spawn body as an anonymous function chunk
        let spawn_name = format!("<spawn_{}>", self.chunks.len());
        let chunk_idx = self.chunks.len();
        self.chunks.push(Chunk::new(&spawn_name));

        let prev_chunk = self.current_chunk;
        let prev_scope = std::mem::replace(&mut self.scope, Scope::new());
        let prev_int_locals = std::mem::take(&mut self.int_locals);
        self.current_chunk = chunk_idx;

        for stmt in body {
            self.compile_statement(stmt);
        }

        let line = self.line;
        self.chunk().emit(OpCode::Ret, line);
        let local_count = self.scope.slot_names.len() as u16;
        self.chunks[chunk_idx].local_count = local_count;
        self.chunks[chunk_idx].local_names = self.scope.slot_names.clone();

        self.current_chunk = prev_chunk;
        self.scope = prev_scope;
        self.int_locals = prev_int_locals;

        // Emit Spawn opcode with the chunk index
        self.chunk().emit_u16(OpCode::Spawn, chunk_idx as u16, line);
    }

    fn compile_import(&mut self, path: &str) {
        // Register module so it's available for ModCall
        self.resolve_module(path);
    }

    // --- Expression Compilation ---

    fn compile_expression(&mut self, expr: &Expression) {
        match expr {
            Expression::IntLiteral(n) => {
                let line = self.line;
                let idx = self.chunk().add_constant(VMValue::Int(*n));
                self.chunk().emit_u16(OpCode::Const, idx, line);
            }

            Expression::FloatLiteral(f) => {
                let line = self.line;
                let idx = self.chunk().add_constant(VMValue::Float(*f));
                self.chunk().emit_u16(OpCode::Const, idx, line);
            }

            Expression::StringLiteral(s) => {
                // The interpreter evaluates a string literal to its raw value
                // (no interpolation at eval time — interpolation happens only at
                // `echo`). Match that: push the verbatim string.
                let line = self.line;
                let idx = self
                    .chunk()
                    .add_constant(VMValue::Str(Arc::new(s.clone())));
                self.chunk().emit_u16(OpCode::Const, idx, line);
            }

            Expression::BoolLiteral(b) => {
                let line = self.line;
                let idx = self.chunk().add_constant(VMValue::Bool(*b));
                self.chunk().emit_u16(OpCode::Const, idx, line);
            }

            Expression::Variable(name) => {
                self.compile_variable_load(name);
            }

            Expression::BinaryOp { left, op, right } => {
                self.compile_binary_op(left, op, right);
            }

            Expression::UnaryOp { op, operand } => {
                self.compile_unary_op(op, operand);
            }

            Expression::FnCall { callee, args } => {
                self.compile_fn_call(callee, args);
            }

            Expression::MethodCall {
                object,
                method,
                args,
            } => {
                self.compile_method_call(object, method, args);
            }

            Expression::FieldAccess { object, field } => {
                // `EnumName.Variant` builds a variant object in the interpreter;
                // the VM has no model for that, so emit a fall-back marker so the
                // whole program runs on the interpreter (correct output).
                if let Expression::Variable(base) = object.as_ref() {
                    if self.enum_names.contains(base.as_str()) {
                        let line = self.line;
                        self.chunk().emit(OpCode::And, line); // unsupported marker
                        return;
                    }
                }
                self.compile_expression(object);
                let line = self.line;
                let field_idx = self
                    .chunk()
                    .add_constant(VMValue::Str(Arc::new(field.clone())));
                self.chunk().emit_u16(OpCode::Field, field_idx, line);
            }

            Expression::Index { object, index } => {
                self.compile_expression(object);
                self.compile_expression(index);
                let line = self.line;
                self.chunk().emit(OpCode::Index, line);
            }

            Expression::Pipe { left, right } => {
                // Bash-style pipe semantics aren't modeled by the VM; fall back.
                let _ = (left, right);
                self.emit_fallback_marker();
            }

            Expression::ChanSend { channel, value } => {
                self.compile_expression(channel);
                self.compile_expression(value);
                let line = self.line;
                self.chunk().emit(OpCode::ChanSend, line);
            }

            Expression::ChanRecv { channel } => {
                self.compile_expression(channel);
                let line = self.line;
                self.chunk().emit(OpCode::ChanRecv, line);
            }

            Expression::Lambda { params, body } => {
                // Closures capture their defining scope (interpreter
                // `Value::Closure`); the VM has no capture, so fall back.
                let _ = (params, body);
                self.emit_fallback_marker();
            }

            Expression::StructInit { name, fields } => {
                self.compile_struct_init(name, fields);
            }

            Expression::Array(elements) => {
                let line = self.line;
                for elem in elements {
                    self.compile_expression(elem);
                }
                self.chunk()
                    .emit_u16(OpCode::Array, elements.len() as u16, line);
            }

            Expression::Await(expr) => {
                self.compile_expression(expr);
                let line = self.line;
                self.chunk().emit(OpCode::Await, line);
            }

            Expression::Match { subject, arms } => {
                // Pattern matching (incl. binding patterns) is not modeled
                // faithfully by the VM yet; fall back to the interpreter.
                let _ = (subject, arms);
                self.emit_fallback_marker();
            }
        }
    }

    fn compile_variable_load(&mut self, name: &str) {
        let line = self.line;
        // Try local first
        if let Some(slot) = self.scope.resolve_local(name) {
            self.chunk().emit_u16(OpCode::Load, slot, line);
        } else if let Some(&chunk_idx) = self.fn_chunks.get(name) {
            // It's a function reference - push as a constant with its real arity
            // so the VM's CALL can validate argument count.
            let arity = self.fn_arity.get(name).copied().unwrap_or(0);
            let fn_ref = VMValue::Function(super::value::FnRef {
                name: name.to_string(),
                chunk_idx,
                arity,
            });
            let idx = self.chunk().add_constant(fn_ref);
            self.chunk().emit_u16(OpCode::Const, idx, line);
        } else {
            // Global
            let idx = self.resolve_global(name);
            self.chunk().emit_u16(OpCode::GLoad, idx, line);
        }
    }

    fn compile_binary_op(
        &mut self,
        left: &Expression,
        op: &BinaryOperator,
        right: &Expression,
    ) {
        // Short-circuit for And/Or
        match op {
            BinaryOperator::And => {
                self.compile_expression(left);
                let jump = self.emit_jump(OpCode::JmpFalse);
                // If left is true, evaluate right
                let line = self.line;
                self.chunk().emit(OpCode::Pop, line);
                self.compile_expression(right);
                self.patch_jump_here(jump);
                return;
            }
            BinaryOperator::Or => {
                self.compile_expression(left);
                let jump = self.emit_jump(OpCode::JmpTrue);
                // If left is false, evaluate right
                let line = self.line;
                self.chunk().emit(OpCode::Pop, line);
                self.compile_expression(right);
                self.patch_jump_here(jump);
                return;
            }
            _ => {}
        }

        self.compile_expression(left);
        self.compile_expression(right);

        let line = self.line;
        // Type specialization (task 12.2 / R9.3): when BOTH operands are
        // statically proven integers, emit the int-specialized opcode so the VM
        // takes an int-first fast path that skips the generic operand
        // type-match. Semantics are identical to the generic opcode (checked
        // overflow, div/mod-by-zero), so this never changes observable behavior.
        let both_int = self.infer_int(left) && self.infer_int(right);
        let opcode = match op {
            BinaryOperator::Add => if both_int { OpCode::AddInt } else { OpCode::Add },
            BinaryOperator::Sub => if both_int { OpCode::SubInt } else { OpCode::Sub },
            BinaryOperator::Mul => if both_int { OpCode::MulInt } else { OpCode::Mul },
            BinaryOperator::Div => if both_int { OpCode::DivInt } else { OpCode::Div },
            BinaryOperator::Mod => if both_int { OpCode::ModInt } else { OpCode::Mod },
            BinaryOperator::Eq => OpCode::Eq,
            BinaryOperator::Neq => OpCode::Neq,
            BinaryOperator::Lt => if both_int { OpCode::LtInt } else { OpCode::Lt },
            BinaryOperator::Lte => if both_int { OpCode::LteInt } else { OpCode::Lte },
            BinaryOperator::Gt => if both_int { OpCode::GtInt } else { OpCode::Gt },
            BinaryOperator::Gte => if both_int { OpCode::GteInt } else { OpCode::Gte },
            BinaryOperator::And | BinaryOperator::Or => unreachable!(),
        };
        self.chunk().emit(opcode, line);
    }

    /// Conservative compiler-local integer inference (task 12.2 / R9.3).
    ///
    /// Returns `true` only when `expr` is *statically provable* to evaluate to
    /// an integer using information reachable inside `backend/vm/` alone (no
    /// edits to `semantics/`): integer literals, `: int`-annotated params /
    /// known-int variables, integer arithmetic over int operands, and unary
    /// negation of an int. Anything uncertain returns `false` (stay generic).
    ///
    /// Soundness is not required for correctness: the specialized opcodes fall
    /// back to the identical generic operation for non-int operands at run time,
    /// so an over-eager `true` can only cost a (cheap) extra tag check, never a
    /// wrong result. We keep it conservative anyway to avoid pointless
    /// specialization.
    fn infer_int(&self, expr: &Expression) -> bool {
        match expr {
            Expression::IntLiteral(_) => true,
            Expression::UnaryOp { op: UnaryOperator::Neg, operand } => self.infer_int(operand),
            Expression::BinaryOp { left, op, right } => {
                matches!(
                    op,
                    BinaryOperator::Add
                        | BinaryOperator::Sub
                        | BinaryOperator::Mul
                        | BinaryOperator::Div
                        | BinaryOperator::Mod
                ) && self.infer_int(left)
                    && self.infer_int(right)
            }
            Expression::Variable(name) => {
                // A local binding shadows a global of the same name, mirroring
                // `compile_variable_load`'s resolution order.
                if self.scope.resolve_local(name).is_some() {
                    self.int_locals.contains(name)
                } else {
                    self.int_globals.contains(name)
                }
            }
            _ => false,
        }
    }

    /// Whether a type annotation names Ran's integer type. Conservative: only
    /// the canonical `int` (and common fixed-width spellings) count.
    fn type_is_int(ty: &Option<TypeExpr>) -> bool {
        match ty {
            Some(TypeExpr::Named(name)) => {
                matches!(name.as_str(), "int" | "i64" | "i32" | "i16" | "i8" | "isize")
            }
            _ => false,
        }
    }

    fn compile_unary_op(&mut self, op: &UnaryOperator, operand: &Expression) {
        self.compile_expression(operand);
        let line = self.line;
        match op {
            UnaryOperator::Neg => self.chunk().emit(OpCode::Neg, line),
            UnaryOperator::Not => self.chunk().emit(OpCode::Not, line),
            UnaryOperator::Ref | UnaryOperator::MutRef => {
                // References are a compile-time concept in Ran;
                // at the VM level they're a no-op (value stays on stack)
            }
            UnaryOperator::Deref => {
                // Dereference is also a no-op at VM level for now
            }
        }
    }

    fn compile_fn_call(&mut self, callee: &Expression, args: &[Expression]) {
        // Check if this is a module call: module.method(args)
        if let Expression::FieldAccess { object, field } = callee {
            if let Expression::Variable(module_name) = object.as_ref() {
                if self.module_indices.contains_key(module_name.as_str())
                    || KNOWN_MODULES.contains(&module_name.as_str())
                {
                    // This is a module call
                    let mod_idx = self.resolve_module(module_name);
                    let method_idx = self.resolve_method(module_name, field);
                    for arg in args {
                        self.compile_expression(arg);
                    }
                    let line = self.line;
                    // MODCALL encoding: opcode, module_idx(u16), method_idx(u16), arg_count(u8)
                    self.chunk().code.push(OpCode::ModCall as u8);
                    self.chunk().code.push((mod_idx >> 8) as u8);
                    self.chunk().code.push((mod_idx & 0xFF) as u8);
                    self.chunk().code.push((method_idx >> 8) as u8);
                    self.chunk().code.push((method_idx & 0xFF) as u8);
                    self.chunk().code.push(args.len() as u8);
                    for _ in 0..6 {
                        self.chunk().lines.push(line);
                    }
                    return;
                }
            }
        }

        // Regular function call: push callee, push args, emit CALL
        self.compile_expression(callee);
        for arg in args {
            self.compile_expression(arg);
        }
        let line = self.line;
        self.chunk()
            .emit_u8(OpCode::Call, args.len() as u8, line);
    }

    fn compile_method_call(
        &mut self,
        object: &Expression,
        method: &str,
        args: &[Expression],
    ) {
        // Check if object is a known module name (e.g., http.listen(...))
        if let Expression::Variable(name) = object {
            if self.module_indices.contains_key(name.as_str())
                || KNOWN_MODULES.contains(&name.as_str())
            {
                let mod_idx = self.resolve_module(name);
                let method_idx = self.resolve_method(name, method);
                for arg in args {
                    self.compile_expression(arg);
                }
                let line = self.line;
                self.chunk().code.push(OpCode::ModCall as u8);
                self.chunk().code.push((mod_idx >> 8) as u8);
                self.chunk().code.push((mod_idx & 0xFF) as u8);
                self.chunk().code.push((method_idx >> 8) as u8);
                self.chunk().code.push((method_idx & 0xFF) as u8);
                self.chunk().code.push(args.len() as u8);
                for _ in 0..6 {
                    self.chunk().lines.push(line);
                }
                return;
            }
        }

        // Regular method call: push object, push args, emit METHOD
        self.compile_expression(object);
        for arg in args {
            self.compile_expression(arg);
        }
        let line = self.line;
        let name_idx = self
            .chunk()
            .add_constant(VMValue::Str(Arc::new(method.to_string())));
        // METHOD encoding: opcode, name_idx(u16), arg_count(u8)
        self.chunk().code.push(OpCode::Method as u8);
        self.chunk().code.push((name_idx >> 8) as u8);
        self.chunk().code.push((name_idx & 0xFF) as u8);
        self.chunk().code.push(args.len() as u8);
        for _ in 0..4 {
            self.chunk().lines.push(line);
        }
    }

    fn compile_lambda(&mut self, params: &[Param], body: &[Stmt]) {
        let lambda_name = format!("<lambda_{}>", self.chunks.len());
        let chunk_idx = self.chunks.len();
        self.chunks.push(Chunk::new(&lambda_name));

        let prev_chunk = self.current_chunk;
        let prev_scope = std::mem::replace(&mut self.scope, Scope::new());
        let prev_int_locals = std::mem::take(&mut self.int_locals);
        self.current_chunk = chunk_idx;

        for param in params {
            self.scope.add_local(&param.name);
            if Self::type_is_int(&param.type_annotation) {
                self.int_locals.insert(param.name.clone());
            }
        }

        for stmt in body {
            self.compile_statement(stmt);
        }

        let line = self.line;
        let last_is_ret = self.chunk().code.last().copied()
            .and_then(OpCode::from_u8)
            .map(|op| op == OpCode::Ret || op == OpCode::RetVal)
            .unwrap_or(false);
        if !last_is_ret {
            self.chunk().emit(OpCode::Ret, line);
        }

        let local_count = self.scope.slot_names.len() as u16;
        self.chunks[chunk_idx].local_count = local_count;
        self.chunks[chunk_idx].local_names = self.scope.slot_names.clone();

        self.current_chunk = prev_chunk;
        self.scope = prev_scope;
        self.int_locals = prev_int_locals;

        // Push the lambda as a function constant
        let fn_ref = VMValue::Function(super::value::FnRef {
            name: lambda_name,
            chunk_idx,
            arity: params.len() as u8,
        });
        let idx = self.chunk().add_constant(fn_ref);
        self.chunk().emit_u16(OpCode::Const, idx, line);
    }

    fn compile_struct_init(&mut self, name: &str, fields: &[(String, Expression)]) {
        let line = self.line;
        // Push each field as a (name, value) pair: name constant first, then the
        // evaluated value. The VM's STRUCT opcode pops `field_count` such pairs
        // (value on top, name below) to assemble the instance.
        for (field_name, value) in fields {
            let name_idx = self
                .chunk()
                .add_constant(VMValue::Str(Arc::new(field_name.clone())));
            self.chunk().emit_u16(OpCode::Const, name_idx, line);
            self.compile_expression(value);
        }
        // Push the struct type name as a constant
        let type_idx = self
            .chunk()
            .add_constant(VMValue::Str(Arc::new(name.to_string())));
        // STRUCT encoding: opcode, type_idx(u16), field_count(u8)
        self.chunk().code.push(OpCode::Struct as u8);
        self.chunk().code.push((type_idx >> 8) as u8);
        self.chunk().code.push((type_idx & 0xFF) as u8);
        self.chunk().code.push(fields.len() as u8);
        for _ in 0..4 {
            self.chunk().lines.push(line);
        }
    }

    fn compile_match(&mut self, subject: &Expression, arms: &[MatchArm]) {
        self.compile_expression(subject);
        let line = self.line;

        let mut end_jumps = Vec::new();

        for arm in arms {
            // Duplicate subject for comparison
            self.chunk().emit(OpCode::Dup, line);

            match &arm.pattern {
                Pattern::Literal(lit) => {
                    self.compile_expression(lit);
                    self.chunk().emit(OpCode::Eq, line);
                }
                Pattern::Variable(var_name) => {
                    // Bind the value to a local - always matches
                    let slot = self.scope.add_local(var_name);
                    self.chunk().emit_u16(OpCode::Store, slot, line);
                    // Push true (always matches)
                    let true_idx = self.chunk().add_constant(VMValue::Bool(true));
                    self.chunk().emit_u16(OpCode::Const, true_idx, line);
                }
                Pattern::Wildcard => {
                    // Pop the dup'd value, push true
                    self.chunk().emit(OpCode::Pop, line);
                    let true_idx = self.chunk().add_constant(VMValue::Bool(true));
                    self.chunk().emit_u16(OpCode::Const, true_idx, line);
                }
            }

            // If pattern doesn't match, jump to next arm
            let next_arm = self.emit_jump(OpCode::JmpFalse);

            // Pop the subject duplicate before executing body
            self.chunk().emit(OpCode::Pop, line);

            // Compile arm body
            self.scope.begin();
            for stmt in &arm.body {
                self.compile_statement(stmt);
            }
            let popped = self.scope.end();
            for _ in 0..popped {
                self.chunk().emit(OpCode::Pop, line);
            }

            // Jump to end of match
            end_jumps.push(self.emit_jump(OpCode::Jmp));

            // Patch next_arm jump
            self.patch_jump_here(next_arm);
        }

        // Pop the original subject value
        self.chunk().emit(OpCode::Pop, line);

        // Patch all end jumps
        for jump in end_jumps {
            self.patch_jump_here(jump);
        }
    }

    // --- String Interpolation ---

    /// Parse a string like "Hello $name, you are $age years old"
    /// and emit INTERP opcode with segment count.
    fn compile_string_interpolation(&mut self, s: &str) {
        let line = self.line;
        let segments = self.parse_interpolation_segments(s);
        let segment_count = segments.len();

        for segment in segments {
            match segment {
                InterpSegment::Literal(text) => {
                    let idx = self
                        .chunk()
                        .add_constant(VMValue::Str(Arc::new(text)));
                    self.chunk().emit_u16(OpCode::Const, idx, line);
                }
                InterpSegment::Variable(var_name) => {
                    self.compile_variable_load(&var_name);
                }
            }
        }

        self.chunk()
            .emit_u8(OpCode::Interp, segment_count as u8, line);
    }

    fn parse_interpolation_segments(&self, s: &str) -> Vec<InterpSegment> {
        let mut segments = Vec::new();
        let mut chars = s.chars().peekable();
        let mut current_literal = String::new();

        while let Some(ch) = chars.next() {
            if ch == '$' {
                // Start of variable interpolation
                if !current_literal.is_empty() {
                    segments.push(InterpSegment::Literal(
                        std::mem::take(&mut current_literal),
                    ));
                }
                // Read variable name (alphanumeric + underscore)
                let mut var_name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' {
                        var_name.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if !var_name.is_empty() {
                    segments.push(InterpSegment::Variable(var_name));
                } else {
                    // Lone $ - treat as literal
                    current_literal.push('$');
                }
            } else {
                current_literal.push(ch);
            }
        }

        if !current_literal.is_empty() {
            segments.push(InterpSegment::Literal(current_literal));
        }

        segments
    }
}

/// Segment in string interpolation
enum InterpSegment {
    Literal(String),
    Variable(String),
}
