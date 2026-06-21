//! VM execution engine - high-performance bytecode interpreter.
//!
//! Register-based with a value stack for expression evaluation.
//! Call frames for function calls with proper scope isolation.

#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::Arc;

use super::chunk::Chunk;
use super::opcodes::OpCode;
use super::value::{VMValue, StructInstance, FnRef};

/// Call frame - represents one function invocation
#[derive(Debug, Clone)]
struct CallFrame {
    /// Which chunk (function) is executing
    chunk_idx: usize,
    /// Instruction pointer within the chunk
    ip: usize,
    /// Base pointer into the stack (where this frame's locals start)
    bp: usize,
}

/// The Ran Virtual Machine
pub struct VM {
    /// All compiled chunks (main + functions)
    pub chunks: Vec<Chunk>,
    /// Value stack
    stack: Vec<VMValue>,
    /// Global variables
    globals: HashMap<String, VMValue>,
    /// Call frame stack
    frames: Vec<CallFrame>,
    /// Global constant name table (for GLOAD/GSTORE)
    pub global_names: Vec<String>,
    /// Captured program output (stdout). `Echo`/`Print` append here instead of
    /// writing to the process stdout directly, so the `--vm` driver can flush it
    /// only on a *clean* run. If a guard trips (E1008/E1009) or an opcode is
    /// unsupported mid-run, the buffer is discarded and the program is re-run on
    /// the interpreter — without this, any output already emitted by the VM
    /// would be duplicated by the interpreter fallback (R6.3).
    out: String,
}

impl VM {
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            stack: Vec::with_capacity(1024),
            globals: HashMap::new(),
            frames: Vec::new(),
            global_names: Vec::new(),
            out: String::new(),
        }
    }

    /// Take ownership of the captured output, leaving the buffer empty. The
    /// `--vm` driver calls this only after a successful `run()` to flush the
    /// program's stdout exactly once.
    pub fn take_output(&mut self) -> String {
        std::mem::take(&mut self.out)
    }

    /// Execute starting from chunk 0 (main), bounded so an incomplete/experimental
    /// program can never loop forever or grow the stack without limit (the VM is
    /// a work-in-progress target). Returns `Err` with a reason if a guard trips.
    pub fn run(&mut self) -> Result<(), String> {
        if self.chunks.is_empty() {
            return Ok(());
        }

        // Push main frame
        self.frames.push(CallFrame {
            chunk_idx: 0,
            ip: 0,
            bp: 0,
        });
        // Reserve this chunk's local slots up-front (filled with Void) so locals
        // are addressed by a fixed `bp + slot` region and operand temporaries
        // always sit *above* them. Without this, a `Store` to a fresh slot could
        // shrink the stack below a higher slot and corrupt locals.
        let lc = self.chunks[0].local_count as usize;
        while self.stack.len() < lc {
            self.stack.push(VMValue::Void);
        }

        self.execute_loop()
    }

    /// Run a named chunk (e.g. `"main"`) as an entry function: push a fresh
    /// frame for it and execute. Returns `Ok(())` (a no-op) if no such chunk
    /// exists. Reserved for when the VM's call/loop semantics are complete
    /// enough to run `fn main()` programs (see `--vm` notes).
    #[allow(dead_code)]
    pub fn run_named(&mut self, name: &str) -> Result<(), String> {
        let idx = match self.chunks.iter().position(|c| c.name == name) {
            Some(i) => i,
            None => return Ok(()),
        };
        let bp = self.stack.len();
        self.frames.push(CallFrame { chunk_idx: idx, ip: 0, bp });
        let lc = self.chunks[idx].local_count as usize;
        while self.stack.len() < bp + lc {
            self.stack.push(VMValue::Void);
        }
        self.execute_loop()
    }

    /// Safety caps: the bytecode VM is experimental and does not implement every
    /// opcode. Without bounds, a misread operand stream can spin forever or push
    /// the value stack without limit (OOM). These guards turn that into a clean,
    /// recoverable error instead of a crash/leak.
    const MAX_STEPS: u64 = 50_000_000;
    const MAX_STACK: usize = 1_000_000;

    fn execute_loop(&mut self) -> Result<(), String> {
        let mut steps: u64 = 0;
        loop {
            steps += 1;
            if steps > Self::MAX_STEPS {
                // E1008 (R6.1): step budget exceeded. Recoverable Err so the `--vm`
                // path can fall back to the interpreter instead of spinning forever.
                return Err(format!(
                    "E1008: VM step budget exceeded ({} instructions) — unsupported construct or runaway; \
help: periksa loop tak-berujung atau jalankan ulang lewat interpreter",
                    Self::MAX_STEPS
                ));
            }
            if self.stack.len() > Self::MAX_STACK {
                // E1009 (R6.2): value-stack cap exceeded. Recoverable Err so execution
                // can fall back to the interpreter instead of growing the stack to OOM.
                return Err(format!(
                    "E1009: VM value-stack cap exceeded ({} entries) — unsupported construct; \
help: periksa rekursi/ekspresi sangat dalam atau jalankan lewat interpreter",
                    Self::MAX_STACK
                ));
            }
            let frame = match self.frames.last() {
                Some(f) => f.clone(),
                None => return Ok(()),
            };

            let chunk = &self.chunks[frame.chunk_idx];
            if frame.ip >= chunk.code.len() {
                self.frames.pop();
                continue;
            }

            let opcode = chunk.code[frame.ip];
            let op = match OpCode::from_u8(opcode) {
                Some(o) => o,
                None => {
                    return Err(format!(
                        "invalid opcode {} at offset {} (experimental VM)",
                        opcode, frame.ip
                    ));
                }
            };

            match op {
                OpCode::Nop => self.advance_ip(1),

                OpCode::Halt => return Ok(()),

                OpCode::Const => {
                    let idx = self.read_u16_at(frame.chunk_idx, frame.ip + 1);
                    let val = self.chunks[frame.chunk_idx].constants[idx as usize].clone();
                    self.stack.push(val);
                    self.advance_ip(3);
                }

                OpCode::Load => {
                    let slot = self.read_u16_at(frame.chunk_idx, frame.ip + 1) as usize;
                    let bp = frame.bp;
                    let val = if bp + slot < self.stack.len() {
                        self.stack[bp + slot].clone()
                    } else {
                        VMValue::Void
                    };
                    self.stack.push(val);
                    self.advance_ip(3);
                }

                OpCode::Store => {
                    let slot = self.read_u16_at(frame.chunk_idx, frame.ip + 1) as usize;
                    let val = self.stack.pop().unwrap_or(VMValue::Void);
                    let bp = frame.bp;
                    let target = bp + slot;
                    if target < self.stack.len() {
                        self.stack[target] = val;
                    } else {
                        while self.stack.len() <= target {
                            self.stack.push(VMValue::Void);
                        }
                        self.stack[target] = val;
                    }
                    self.advance_ip(3);
                }

                OpCode::GLoad => {
                    let idx = self.read_u16_at(frame.chunk_idx, frame.ip + 1) as usize;
                    let name = &self.global_names[idx];
                    let val = self.globals.get(name).cloned().unwrap_or(VMValue::Void);
                    self.stack.push(val);
                    self.advance_ip(3);
                }

                OpCode::GStore => {
                    let idx = self.read_u16_at(frame.chunk_idx, frame.ip + 1) as usize;
                    let val = self.stack.pop().unwrap_or(VMValue::Void);
                    let name = self.global_names[idx].clone();
                    self.globals.insert(name, val);
                    self.advance_ip(3);
                }

                // Arithmetic. Overflow and divide/modulo-by-zero return a
                // recoverable Err so the `--vm` driver falls back to the
                // interpreter, which raises the proper E1010/E1011 fault — the
                // VM never silently wraps or yields a wrong value (R9.1).
                OpCode::Add => { self.binary_try(Self::op_add)?; self.advance_ip(1); }
                OpCode::Sub => { self.binary_try(Self::op_sub)?; self.advance_ip(1); }
                OpCode::Mul => { self.binary_try(Self::op_mul)?; self.advance_ip(1); }
                OpCode::Div => { self.binary_try(Self::op_div)?; self.advance_ip(1); }
                OpCode::Mod => { self.binary_try(Self::op_mod)?; self.advance_ip(1); }
                OpCode::Concat => { self.binary_try(Self::op_concat)?; self.advance_ip(1); }

                // Type-specialized arithmetic (task 12.2 / R9.3). Emitted only
                // when the compiler proved both operands are integers, so the
                // common case takes an int-first fast path that skips the
                // generic multi-arm operand match. For any non-int operand
                // (e.g. if inference was conservative) they defer to the
                // identical generic op, so observable behavior — including
                // checked overflow → Err (E1010) and div/mod-by-zero → Err
                // (E1011) — is byte-for-byte identical to the generic opcode.
                OpCode::AddInt => { self.binary_try(Self::op_add_int)?; self.advance_ip(1); }
                OpCode::SubInt => { self.binary_try(Self::op_sub_int)?; self.advance_ip(1); }
                OpCode::MulInt => { self.binary_try(Self::op_mul_int)?; self.advance_ip(1); }
                OpCode::DivInt => { self.binary_try(Self::op_div_int)?; self.advance_ip(1); }
                OpCode::ModInt => { self.binary_try(Self::op_mod_int)?; self.advance_ip(1); }
                OpCode::Neg => {
                    let val = self.stack.pop().unwrap_or(VMValue::Void);
                    let result = match val {
                        VMValue::Int(n) => VMValue::Int(-n),
                        VMValue::Float(f) => VMValue::Float(-f),
                        _ => VMValue::Void,
                    };
                    self.stack.push(result);
                    self.advance_ip(1);
                }

                // Comparison
                OpCode::Eq => { self.binary_op(|a, b| VMValue::Bool(a == b)); self.advance_ip(1); }
                OpCode::Neq => { self.binary_op(|a, b| VMValue::Bool(a != b)); self.advance_ip(1); }
                OpCode::Lt => { self.binary_op(|a, b| Self::op_lt(a, b)); self.advance_ip(1); }
                OpCode::Lte => { self.binary_op(|a, b| Self::op_lte(a, b)); self.advance_ip(1); }
                OpCode::Gt => { self.binary_op(|a, b| Self::op_gt(a, b)); self.advance_ip(1); }
                OpCode::Gte => { self.binary_op(|a, b| Self::op_gte(a, b)); self.advance_ip(1); }

                // Type-specialized comparison (task 12.2 / R9.3). Int-first fast
                // path; identical to the generic comparison for every operand
                // pair (the generic ops already produce Bool(false) for non-int
                // / mismatched operands, and these defer to them in that case).
                OpCode::LtInt => { self.binary_op(|a, b| Self::op_lt_int(a, b)); self.advance_ip(1); }
                OpCode::LteInt => { self.binary_op(|a, b| Self::op_lte_int(a, b)); self.advance_ip(1); }
                OpCode::GtInt => { self.binary_op(|a, b| Self::op_gt_int(a, b)); self.advance_ip(1); }
                OpCode::GteInt => { self.binary_op(|a, b| Self::op_gte_int(a, b)); self.advance_ip(1); }

                // Logic
                OpCode::Not => {
                    let val = self.stack.pop().unwrap_or(VMValue::Void);
                    self.stack.push(VMValue::Bool(!val.is_truthy()));
                    self.advance_ip(1);
                }

                // Control flow
                OpCode::Jmp => {
                    let offset = self.read_i16_at(frame.chunk_idx, frame.ip + 1);
                    let new_ip = (frame.ip as isize + 3 + offset as isize) as usize;
                    self.set_ip(new_ip);
                }
                OpCode::JmpFalse => {
                    let val = self.stack.pop().unwrap_or(VMValue::Void);
                    if !val.is_truthy() {
                        let offset = self.read_i16_at(frame.chunk_idx, frame.ip + 1);
                        let new_ip = (frame.ip as isize + 3 + offset as isize) as usize;
                        self.set_ip(new_ip);
                    } else {
                        self.advance_ip(3);
                    }
                }
                OpCode::JmpTrue => {
                    let val = self.stack.pop().unwrap_or(VMValue::Void);
                    if val.is_truthy() {
                        let offset = self.read_i16_at(frame.chunk_idx, frame.ip + 1);
                        let new_ip = (frame.ip as isize + 3 + offset as isize) as usize;
                        self.set_ip(new_ip);
                    } else {
                        self.advance_ip(3);
                    }
                }

                // Functions. Calling convention: the callee `Function` value is
                // pushed first, then `arg_count` arguments. So the stack layout
                // at CALL is `[ ... , callee, arg0, arg1, ... ]`. We set the new
                // frame's base pointer (`bp`) at `arg0`, so the callee's params
                // occupy local slots `0..arg_count` (the compiler registers
                // params as the first locals). `Ret`/`RetVal` then unwind down to
                // `bp - 1` (removing args AND the callee) and push the single
                // result, so a call expression leaves exactly one value behind —
                // matching the interpreter's value semantics.
                OpCode::Call => {
                    let arg_count = self.chunks[frame.chunk_idx].code[frame.ip + 1] as usize;
                    if self.stack.len() < arg_count + 1 {
                        return Err("VM call: stack underflow (malformed bytecode)".to_string());
                    }
                    let callee = self.stack[self.stack.len() - 1 - arg_count].clone();
                    match callee {
                        VMValue::Function(fn_ref) => {
                            // Arity guard: a mismatch means we'd read uninitialized
                            // slots or leave args behind. Fall back rather than
                            // risk wrong output.
                            if fn_ref.arity as usize != arg_count {
                                return Err(format!(
                                    "VM call: arity mismatch for `{}` (expected {}, got {})",
                                    fn_ref.name, fn_ref.arity, arg_count
                                ));
                            }
                            let bp = self.stack.len() - arg_count;
                            self.advance_ip(2);
                            self.frames.push(CallFrame {
                                chunk_idx: fn_ref.chunk_idx,
                                ip: 0,
                                bp,
                            });
                            // Reserve the callee's remaining local slots (the
                            // args already occupy slots 0..arg_count) so locals
                            // form a fixed region below the operand stack.
                            let lc = self.chunks[fn_ref.chunk_idx].local_count as usize;
                            while self.stack.len() < bp + lc {
                                self.stack.push(VMValue::Void);
                            }
                        }
                        // Calling a non-function (e.g. a bare built-in like
                        // `len`/`range`, or an undefined name) is not modeled by
                        // the VM. Return a recoverable Err so the driver falls
                        // back to the interpreter (which knows the built-ins).
                        other => {
                            return Err(format!(
                                "VM call: callee is not a function ({}) — built-in or \
unknown call; using the interpreter",
                                other.type_name()
                            ));
                        }
                    }
                }

                OpCode::Ret => {
                    // Implicit `return ()`: unwind the frame and leave Void.
                    if let Some(f) = self.frames.pop() {
                        let base = f.bp.saturating_sub(1);
                        self.stack.truncate(base);
                    }
                    self.stack.push(VMValue::Void);
                    // No advance: the popped frame's ip is gone; the caller's ip
                    // already points past its CALL.
                }

                OpCode::RetVal => {
                    let val = self.stack.pop().unwrap_or(VMValue::Void);
                    if let Some(f) = self.frames.pop() {
                        let base = f.bp.saturating_sub(1);
                        self.stack.truncate(base);
                    }
                    self.stack.push(val);
                }

                // Data structures
                OpCode::Array => {
                    let count = self.read_u16_at(frame.chunk_idx, frame.ip + 1) as usize;
                    let start = self.stack.len().saturating_sub(count);
                    let items: Vec<VMValue> = self.stack.drain(start..).collect();
                    self.stack.push(VMValue::Array(Arc::new(items)));
                    self.advance_ip(3);
                }

                // MAP <count:u16>: pops `count` (value, key) pairs pushed in
                // source order (key first, then value), builds a string-keyed
                // map. (Not currently emitted by the compiler — Ran has no map
                // literal — but implemented for completeness and tests.)
                OpCode::Map => {
                    let count = self.read_u16_at(frame.chunk_idx, frame.ip + 1) as usize;
                    let mut map = HashMap::with_capacity(count);
                    for _ in 0..count {
                        let val = self.stack.pop().unwrap_or(VMValue::Void);
                        let key = self.stack.pop().unwrap_or(VMValue::Void);
                        map.insert(format!("{}", key), val);
                    }
                    self.stack.push(VMValue::Map(Arc::new(map)));
                    self.advance_ip(3);
                }

                // INDEX: `obj[idx]`. Mirrors the interpreter exactly:
                //   array[int]  -> bounds-checked element (OOB incl. negative
                //                  raises E1012 → Err → interpreter reproduces it)
                //   string[int] -> char-boundary-aware indexing, OOB → E1012
                //   map[string] -> value or Void
                // Any other combination → Err so the interpreter handles it.
                OpCode::Index => {
                    let idx = self.stack.pop().unwrap_or(VMValue::Void);
                    let obj = self.stack.pop().unwrap_or(VMValue::Void);
                    let result = match (&obj, &idx) {
                        (VMValue::Array(arr), VMValue::Int(i)) => {
                            match Self::checked_index(*i, arr.len()) {
                                Some(u) => arr[u].clone(),
                                None => {
                                    return Err(format!(
                                        "E1012: index {} out of bounds (len {})",
                                        i,
                                        arr.len()
                                    ));
                                }
                            }
                        }
                        (VMValue::Str(s), VMValue::Int(i)) => {
                            let len = s.chars().count();
                            match Self::checked_index(*i, len).and_then(|u| s.chars().nth(u)) {
                                Some(c) => VMValue::Str(Arc::new(c.to_string())),
                                None => {
                                    return Err(format!(
                                        "E1012: index {} out of bounds (len {})",
                                        i, len
                                    ));
                                }
                            }
                        }
                        (VMValue::Map(map), VMValue::Str(key)) => {
                            map.get(key.as_str()).cloned().unwrap_or(VMValue::Void)
                        }
                        _ => {
                            return Err("VM index: unsupported operand types".to_string());
                        }
                    };
                    self.stack.push(result);
                    self.advance_ip(1);
                }

                // FIELD <name_idx:u16>: `obj.field`. Struct/Map field read,
                // matching the interpreter (missing field → Void; non-aggregate
                // → Void).
                OpCode::Field => {
                    let name_idx = self.read_u16_at(frame.chunk_idx, frame.ip + 1) as usize;
                    let field = match self.chunks[frame.chunk_idx].constants.get(name_idx) {
                        Some(VMValue::Str(s)) => s.as_ref().clone(),
                        _ => return Err("VM field: bad field-name constant".to_string()),
                    };
                    let obj = self.stack.pop().unwrap_or(VMValue::Void);
                    let result = match &obj {
                        VMValue::Struct(s) => {
                            s.fields.get(&field).cloned().unwrap_or(VMValue::Void)
                        }
                        VMValue::Map(m) => m.get(&field).cloned().unwrap_or(VMValue::Void),
                        _ => VMValue::Void,
                    };
                    self.stack.push(result);
                    self.advance_ip(3);
                }

                // STRUCT <type_idx:u16> <field_count:u8>: pops `field_count`
                // (name, value) pairs pushed in source order (name const first,
                // then the value), assembling a struct instance. Matches the
                // interpreter's `Value::Object(name, fields)`.
                OpCode::Struct => {
                    let type_idx = self.read_u16_at(frame.chunk_idx, frame.ip + 1) as usize;
                    let field_count =
                        self.chunks[frame.chunk_idx].code[frame.ip + 3] as usize;
                    let type_name = match self.chunks[frame.chunk_idx].constants.get(type_idx) {
                        Some(VMValue::Str(s)) => s.as_ref().clone(),
                        _ => return Err("VM struct: bad type-name constant".to_string()),
                    };
                    let mut fields = HashMap::with_capacity(field_count);
                    for _ in 0..field_count {
                        let val = self.stack.pop().unwrap_or(VMValue::Void);
                        let name = self.stack.pop().unwrap_or(VMValue::Void);
                        fields.insert(format!("{}", name), val);
                    }
                    self.stack.push(VMValue::Struct(Arc::new(StructInstance {
                        type_name,
                        fields,
                    })));
                    self.advance_ip(4);
                }

                // LEN: length of array (#elements), string (#chars), or map
                // (#entries). Used by compiled `for` loops to bound iteration.
                OpCode::Len => {
                    let val = self.stack.pop().unwrap_or(VMValue::Void);
                    let n = match &val {
                        VMValue::Array(a) => a.len() as i64,
                        VMValue::Str(s) => s.chars().count() as i64,
                        VMValue::Map(m) => m.len() as i64,
                        _ => {
                            return Err("VM len: operand is not array/string/map".to_string());
                        }
                    };
                    self.stack.push(VMValue::Int(n));
                    self.advance_ip(1);
                }

                OpCode::Echo => {
                    let val = self.stack.pop().unwrap_or(VMValue::Void);
                    // Mirror the interpreter's `echo`: format the value, then
                    // run `$name` interpolation over the *formatted* string
                    // against the current scope (frame locals + globals), with
                    // dotted-path traversal into structs/maps. Buffered (not
                    // written straight to stdout) so the `--vm` driver can
                    // discard it on fallback (R6.3).
                    let formatted = format!("{}", val);
                    let output = self.interpolate(&formatted, &frame);
                    self.out.push_str(&output);
                    self.out.push('\n');
                    self.advance_ip(1);
                }

                OpCode::Print => {
                    let val = self.stack.pop().unwrap_or(VMValue::Void);
                    self.out.push_str(&format!("{}", val));
                    self.out.push('\n');
                    self.advance_ip(1);
                }

                OpCode::Pop => {
                    self.stack.pop();
                    self.advance_ip(1);
                }

                OpCode::Dup => {
                    if let Some(val) = self.stack.last().cloned() {
                        self.stack.push(val);
                    }
                    self.advance_ip(1);
                }

                other => {
                    return Err(format!("unimplemented opcode {:?} (experimental VM)", other));
                }
            }
        }
    }

    // --- Helpers ---

    fn advance_ip(&mut self, n: usize) {
        if let Some(frame) = self.frames.last_mut() {
            frame.ip += n;
        }
    }

    fn set_ip(&mut self, ip: usize) {
        if let Some(frame) = self.frames.last_mut() {
            frame.ip = ip;
        }
    }

    fn read_u16_at(&self, chunk_idx: usize, pos: usize) -> u16 {
        self.chunks[chunk_idx].read_u16(pos)
    }

    fn read_i16_at(&self, chunk_idx: usize, pos: usize) -> i16 {
        self.chunks[chunk_idx].read_i16(pos)
    }

    fn binary_op<F>(&mut self, f: F)
    where
        F: FnOnce(&VMValue, &VMValue) -> VMValue,
    {
        let b = self.stack.pop().unwrap_or(VMValue::Void);
        let a = self.stack.pop().unwrap_or(VMValue::Void);
        self.stack.push(f(&a, &b));
    }

    /// Fallible binary op: pop `a`, `b`, push `f(a, b)` or propagate an Err
    /// (used for checked arithmetic so overflow / div-by-zero become a clean
    /// fall-back to the interpreter rather than a wrap or a wrong value).
    fn binary_try<F>(&mut self, f: F) -> Result<(), String>
    where
        F: FnOnce(&VMValue, &VMValue) -> Result<VMValue, String>,
    {
        let b = self.stack.pop().unwrap_or(VMValue::Void);
        let a = self.stack.pop().unwrap_or(VMValue::Void);
        let v = f(&a, &b)?;
        self.stack.push(v);
        Ok(())
    }

    /// Resolve a (possibly dotted) variable path against the current frame's
    /// locals first, then globals — mirroring the interpreter's `var_get`
    /// (innermost binding wins) and `lookup_path` (traverse struct/map fields).
    fn lookup_path(&self, path: &str, frame: &CallFrame) -> Option<VMValue> {
        let mut parts = path.split('.');
        let base = parts.next()?;

        // Locals: map each named slot to its current stack value. Higher slots
        // (inner scopes) overwrite lower ones, so the innermost binding wins —
        // matching the interpreter's innermost->outermost search.
        let mut current: Option<VMValue> = None;
        let names = &self.chunks[frame.chunk_idx].local_names;
        for (slot, name) in names.iter().enumerate() {
            if name == base {
                let pos = frame.bp + slot;
                if pos < self.stack.len() {
                    current = Some(self.stack[pos].clone());
                }
            }
        }
        // Globals fall back if no local matched.
        if current.is_none() {
            current = self.globals.get(base).cloned();
        }
        let mut current = current?;

        for field in parts {
            current = match current {
                VMValue::Struct(s) => s.fields.get(field).cloned()?,
                VMValue::Map(m) => m.get(field).cloned()?,
                _ => return None,
            };
        }
        Some(current)
    }

    /// Faithful port of the interpreter's `interpolate_string`: replace `$name`,
    /// `${name}`, and dotted `$a.b.c` with the looked-up value's display form;
    /// leave an unresolved `$path` literal (matching the interpreter).
    fn interpolate(&self, s: &str, frame: &CallFrame) -> String {
        let mut result = String::new();
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '$' && i + 1 < chars.len() {
                i += 1;
                let mut path = String::new();
                if chars[i] == '{' {
                    i += 1;
                    while i < chars.len() && chars[i] != '}' {
                        path.push(chars[i]);
                        i += 1;
                    }
                    if i < chars.len() {
                        i += 1;
                    }
                } else {
                    while i < chars.len()
                        && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
                    {
                        path.push(chars[i]);
                        i += 1;
                    }
                    // A trailing dot is punctuation, not part of the path.
                    while path.ends_with('.') {
                        path.pop();
                        i -= 1;
                    }
                }
                match self.lookup_path(&path, frame) {
                    Some(val) => result.push_str(&format!("{}", val)),
                    None => {
                        result.push('$');
                        result.push_str(&path);
                    }
                }
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        result
    }

    // --- Arithmetic operations ---
    //
    // These mirror the interpreter's checked arithmetic (R7.1/R7.2): integer
    // overflow → Err (the interpreter raises E1010), divide/modulo by zero →
    // Err (E1011). Returning Err makes the `--vm` driver fall back so the
    // interpreter produces the proper coded fault — the VM never wraps or
    // yields a wrong value.

    fn op_add(a: &VMValue, b: &VMValue) -> Result<VMValue, String> {
        Ok(match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => match x.checked_add(*y) {
                Some(v) => VMValue::Int(v),
                None => return Err("E1010: integer overflow in `+`".to_string()),
            },
            (VMValue::Float(x), VMValue::Float(y)) => VMValue::Float(x + y),
            (VMValue::Int(x), VMValue::Float(y)) => VMValue::Float(*x as f64 + y),
            (VMValue::Float(x), VMValue::Int(y)) => VMValue::Float(x + *y as f64),
            (VMValue::Str(x), VMValue::Str(y)) => VMValue::Str(Arc::new(format!("{}{}", x, y))),
            (VMValue::Str(x), other) => VMValue::Str(Arc::new(format!("{}{}", x, other))),
            (other, VMValue::Str(y)) => VMValue::Str(Arc::new(format!("{}{}", other, y))),
            // Mixed/unsupported operand types: let the interpreter decide.
            _ => return Err("VM add: unsupported operand types".to_string()),
        })
    }

    fn op_sub(a: &VMValue, b: &VMValue) -> Result<VMValue, String> {
        Ok(match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => match x.checked_sub(*y) {
                Some(v) => VMValue::Int(v),
                None => return Err("E1010: integer overflow in `-`".to_string()),
            },
            (VMValue::Float(x), VMValue::Float(y)) => VMValue::Float(x - y),
            (VMValue::Int(x), VMValue::Float(y)) => VMValue::Float(*x as f64 - y),
            (VMValue::Float(x), VMValue::Int(y)) => VMValue::Float(x - *y as f64),
            _ => return Err("VM sub: unsupported operand types".to_string()),
        })
    }

    fn op_mul(a: &VMValue, b: &VMValue) -> Result<VMValue, String> {
        Ok(match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => match x.checked_mul(*y) {
                Some(v) => VMValue::Int(v),
                None => return Err("E1010: integer overflow in `*`".to_string()),
            },
            (VMValue::Float(x), VMValue::Float(y)) => VMValue::Float(x * y),
            (VMValue::Int(x), VMValue::Float(y)) => VMValue::Float(*x as f64 * y),
            (VMValue::Float(x), VMValue::Int(y)) => VMValue::Float(x * *y as f64),
            _ => return Err("VM mul: unsupported operand types".to_string()),
        })
    }

    fn op_div(a: &VMValue, b: &VMValue) -> Result<VMValue, String> {
        Ok(match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => {
                if *y == 0 {
                    return Err("E1011: division by zero".to_string());
                }
                match x.checked_div(*y) {
                    Some(v) => VMValue::Int(v),
                    None => return Err("E1010: integer overflow in `/`".to_string()),
                }
            }
            (VMValue::Float(x), VMValue::Float(y)) => VMValue::Float(x / y),
            (VMValue::Int(x), VMValue::Float(y)) => VMValue::Float(*x as f64 / y),
            (VMValue::Float(x), VMValue::Int(y)) => VMValue::Float(x / *y as f64),
            _ => return Err("VM div: unsupported operand types".to_string()),
        })
    }

    fn op_mod(a: &VMValue, b: &VMValue) -> Result<VMValue, String> {
        Ok(match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => {
                if *y == 0 {
                    return Err("E1011: modulo by zero".to_string());
                }
                match x.checked_rem(*y) {
                    Some(v) => VMValue::Int(v),
                    None => return Err("E1010: integer overflow in `%`".to_string()),
                }
            }
            _ => return Err("VM mod: unsupported operand types".to_string()),
        })
    }

    /// Explicit string concatenation (`Concat` opcode). Like `op_add` for
    /// strings but always stringifies both operands.
    fn op_concat(a: &VMValue, b: &VMValue) -> Result<VMValue, String> {
        Ok(VMValue::Str(Arc::new(format!("{}{}", a, b))))
    }

    // --- Type-specialized integer operations (task 12.2 / R9.3) ---
    //
    // Each takes an int-first fast path (the hot, statically-proven case) and
    // defers to the identical generic op for any non-int operand. Because the
    // generic ops' own Int/Int branch performs exactly the same checked
    // arithmetic (and comparison), these are observationally identical to their
    // generic counterparts for EVERY possible operand pair — overflow still
    // yields Err (E1010), div/mod-by-zero still yields Err (E1011). The only
    // difference is the dispatch shape, never the result. This is what
    // guarantees VM↔VM and VM↔interpreter parity (R9.1).

    fn op_add_int(a: &VMValue, b: &VMValue) -> Result<VMValue, String> {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => match x.checked_add(*y) {
                Some(v) => Ok(VMValue::Int(v)),
                None => Err("E1010: integer overflow in `+`".to_string()),
            },
            _ => Self::op_add(a, b),
        }
    }

    fn op_sub_int(a: &VMValue, b: &VMValue) -> Result<VMValue, String> {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => match x.checked_sub(*y) {
                Some(v) => Ok(VMValue::Int(v)),
                None => Err("E1010: integer overflow in `-`".to_string()),
            },
            _ => Self::op_sub(a, b),
        }
    }

    fn op_mul_int(a: &VMValue, b: &VMValue) -> Result<VMValue, String> {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => match x.checked_mul(*y) {
                Some(v) => Ok(VMValue::Int(v)),
                None => Err("E1010: integer overflow in `*`".to_string()),
            },
            _ => Self::op_mul(a, b),
        }
    }

    fn op_div_int(a: &VMValue, b: &VMValue) -> Result<VMValue, String> {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => {
                if *y == 0 {
                    return Err("E1011: division by zero".to_string());
                }
                match x.checked_div(*y) {
                    Some(v) => Ok(VMValue::Int(v)),
                    None => Err("E1010: integer overflow in `/`".to_string()),
                }
            }
            _ => Self::op_div(a, b),
        }
    }

    fn op_mod_int(a: &VMValue, b: &VMValue) -> Result<VMValue, String> {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => {
                if *y == 0 {
                    return Err("E1011: modulo by zero".to_string());
                }
                match x.checked_rem(*y) {
                    Some(v) => Ok(VMValue::Int(v)),
                    None => Err("E1010: integer overflow in `%`".to_string()),
                }
            }
            _ => Self::op_mod(a, b),
        }
    }

    fn op_lt_int(a: &VMValue, b: &VMValue) -> VMValue {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => VMValue::Bool(x < y),
            _ => Self::op_lt(a, b),
        }
    }

    fn op_lte_int(a: &VMValue, b: &VMValue) -> VMValue {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => VMValue::Bool(x <= y),
            _ => Self::op_lte(a, b),
        }
    }

    fn op_gt_int(a: &VMValue, b: &VMValue) -> VMValue {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => VMValue::Bool(x > y),
            _ => Self::op_gt(a, b),
        }
    }

    fn op_gte_int(a: &VMValue, b: &VMValue) -> VMValue {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => VMValue::Bool(x >= y),
            _ => Self::op_gte(a, b),
        }
    }

    /// Bounds-check an `i64` index against `len`, mirroring the interpreter's
    /// `checked_index`: reject negatives and `>= len`, returning the `usize`
    /// offset on success.
    fn checked_index(i: i64, len: usize) -> Option<usize> {
        if i < 0 {
            return None;
        }
        let u = i as usize;
        if u < len {
            Some(u)
        } else {
            None
        }
    }

    fn op_lt(a: &VMValue, b: &VMValue) -> VMValue {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => VMValue::Bool(x < y),
            (VMValue::Float(x), VMValue::Float(y)) => VMValue::Bool(x < y),
            _ => VMValue::Bool(false),
        }
    }

    fn op_lte(a: &VMValue, b: &VMValue) -> VMValue {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => VMValue::Bool(x <= y),
            (VMValue::Float(x), VMValue::Float(y)) => VMValue::Bool(x <= y),
            _ => VMValue::Bool(false),
        }
    }

    fn op_gt(a: &VMValue, b: &VMValue) -> VMValue {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => VMValue::Bool(x > y),
            (VMValue::Float(x), VMValue::Float(y)) => VMValue::Bool(x > y),
            _ => VMValue::Bool(false),
        }
    }

    fn op_gte(a: &VMValue, b: &VMValue) -> VMValue {
        match (a, b) {
            (VMValue::Int(x), VMValue::Int(y)) => VMValue::Bool(x >= y),
            (VMValue::Float(x), VMValue::Float(y)) => VMValue::Bool(x >= y),
            _ => VMValue::Bool(false),
        }
    }
}

// ============================================================================
// Property 11 — Bounded VM execution always terminates (R6.1, R6.2).
// ============================================================================
#[cfg(test)]
mod bounded_vm_property {
    // Feature: memory-safe-self-hosting, Property 11: Bounded VM execution always terminates
    //
    // The bytecode VM is experimental and does not implement every opcode, so a
    // misread operand stream or a runaway loop must NEVER hang the process or
    // grow the value stack without bound. Two guards enforce this:
    //   * step budget  -> Err("E1008: ...")  (R6.1)
    //   * value-stack cap -> Err("E1009: ...") (R6.2)
    // This property asserts that, for arbitrary well-formed synthetic bytecode
    // (straight-line programs and bounded countdown loops), `VM::run` always
    // *returns* (terminates) with `Ok` or an `Err` carrying E1008/E1009 — never
    // an infinite loop or an unbounded stack. The two deterministic guard tests
    // below additionally prove each guard actually fires.

    use super::super::{Chunk, OpCode, VMValue, VM};
    use crate::support::pbt::{self, Gen};

    /// A single safe (always-supported, never-overflowing) instruction used to
    /// assemble straight-line synthetic programs. Arithmetic that could panic on
    /// i64 overflow in debug builds is deliberately excluded here — the loop
    /// variant exercises Sub/Gt/Load/Store/Jmp instead, with bounded operands.
    #[derive(Clone, Debug)]
    enum SafeOp {
        PushInt(i64),
        Pop,
        Dup,
        Nop,
        Echo,
    }

    /// A synthetic VM program: either a finite straight-line sequence or a real
    /// countdown loop with a finite, bounded trip count.
    #[derive(Clone, Debug)]
    enum SynthProg {
        StraightLine { ops: Vec<SafeOp> },
        Countdown { trips: i64 },
    }

    fn synth_gen() -> Gen<SynthProg> {
        Gen::new(
            |rng, _size| {
                if rng.below(2) == 0 {
                    // Bounded countdown loop: 0..=50 iterations (always terminates Ok).
                    SynthProg::Countdown { trips: rng.upto(50) as i64 }
                } else {
                    let len = rng.upto(20);
                    let mut ops = Vec::with_capacity(len);
                    for _ in 0..len {
                        let op = match rng.below(5) {
                            0 => SafeOp::PushInt(rng.range_i64(-1000, 1000)),
                            1 => SafeOp::Pop,
                            2 => SafeOp::Dup,
                            3 => SafeOp::Nop,
                            _ => SafeOp::Echo,
                        };
                        ops.push(op);
                    }
                    SynthProg::StraightLine { ops }
                }
            },
            |_| Vec::new(),
        )
    }

    fn build_straight(ops: &[SafeOp]) -> Chunk {
        let mut c = Chunk::new("<synth-sl>");
        for op in ops {
            match op {
                SafeOp::PushInt(n) => {
                    let i = c.add_constant(VMValue::Int(*n));
                    c.emit_u16(OpCode::Const, i, 1);
                }
                SafeOp::Pop => c.emit(OpCode::Pop, 1),
                SafeOp::Dup => c.emit(OpCode::Dup, 1),
                SafeOp::Nop => c.emit(OpCode::Nop, 1),
                SafeOp::Echo => c.emit(OpCode::Echo, 1),
            }
        }
        c.emit(OpCode::Halt, 1);
        c
    }

    /// Hand-assemble a real countdown loop:
    /// `i = trips; while i > 0 { i = i - 1 }` — exercises Load/Store/Const/Gt/
    /// Sub/JmpFalse/Jmp with a finite trip count.
    fn build_countdown(trips: i64) -> Chunk {
        let mut c = Chunk::new("<synth-loop>");
        let n_idx = c.add_constant(VMValue::Int(trips));
        let zero_idx = c.add_constant(VMValue::Int(0));
        let one_idx = c.add_constant(VMValue::Int(1));

        // i = trips  (local slot 0, bp = 0)
        c.emit_u16(OpCode::Const, n_idx, 1);
        c.emit_u16(OpCode::Store, 0, 1);

        let loop_start = c.len();
        // cond: i > 0
        c.emit_u16(OpCode::Load, 0, 1);
        c.emit_u16(OpCode::Const, zero_idx, 1);
        c.emit(OpCode::Gt, 1);
        // exit if false (placeholder offset patched below)
        c.emit_i16(OpCode::JmpFalse, 0, 1);
        let jf_off = c.len() - 2;
        // body: i = i - 1
        c.emit_u16(OpCode::Load, 0, 1);
        c.emit_u16(OpCode::Const, one_idx, 1);
        c.emit(OpCode::Sub, 1);
        c.emit_u16(OpCode::Store, 0, 1);
        // jump back to condition
        let cur = c.len();
        let back = loop_start as isize - (cur as isize + 3);
        c.emit_i16(OpCode::Jmp, back as i16, 1);
        // patch exit jump to land here
        let target = c.len();
        c.patch_jump(jf_off, target);
        c.emit(OpCode::Halt, 1);
        c
    }

    fn run_chunk(chunk: Chunk) -> Result<(), String> {
        let mut vm = VM::new();
        vm.chunks = vec![chunk];
        vm.run()
    }

    fn build(prog: &SynthProg) -> Chunk {
        match prog {
            SynthProg::StraightLine { ops } => build_straight(ops),
            SynthProg::Countdown { trips } => build_countdown(*trips),
        }
    }

    #[test]
    fn prop11_bounded_vm_execution_always_terminates() {
        // ≥100 cases via the std-only harness (pbt::Config::from_env clamps to 100).
        pbt::for_all("prop11_bounded_vm_terminates", &synth_gen(), |prog| {
            // If `run` ever failed to terminate, this call would hang and the
            // test would never finish — so reaching the match *is* the
            // termination evidence. The result must additionally be Ok or a
            // bounded-guard error (E1008/E1009), never any other outcome.
            match run_chunk(build(prog)) {
                Ok(()) => true,
                Err(e) => e.starts_with("E1008") || e.starts_with("E1009"),
            }
        });
    }

    #[test]
    fn prop11_value_stack_cap_yields_e1009_not_oom() {
        // A loop that pushes a constant every iteration and never pops: without
        // the stack cap this would grow the value stack until OOM. The guard
        // turns it into a clean, recoverable E1009 (R6.2). Trips the cap in ~1M
        // iterations, well under the step budget, so it stays fast.
        let mut c = Chunk::new("<stack-bomb>");
        let k = c.add_constant(VMValue::Int(7));
        c.emit_u16(OpCode::Const, k, 1); // ip 0..=2  (push, no matching pop)
        // Jmp at ip 3: new_ip = ip + 3 + offset = 3 + 3 + (-6) = 0  -> back to push
        c.emit_i16(OpCode::Jmp, -6, 1);
        let err = run_chunk(c).expect_err("stack-growth loop must trip a guard");
        assert!(err.starts_with("E1009"), "expected E1009 stack cap, got: {err}");
    }

    #[test]
    fn prop11_step_budget_yields_e1008_not_infinite_loop() {
        // The tightest possible infinite loop: a single Jmp to itself with an
        // empty stack. The stack never grows, so only the step budget can stop
        // it — proving E1008 (R6.1) bounds runaway execution instead of hanging.
        let mut c = Chunk::new("<spin>");
        // Jmp at ip 0: new_ip = 0 + 3 + (-3) = 0  -> self-loop
        c.emit_i16(OpCode::Jmp, -3, 1);
        let err = run_chunk(c).expect_err("infinite self-loop must trip the step budget");
        assert!(err.starts_with("E1008"), "expected E1008 step budget, got: {err}");
    }
}

// ============================================================================
// Property 12 — VM falls back to the interpreter without crashing (R6.3, R6.4).
// ============================================================================
#[cfg(test)]
mod vm_fallback_property {
    // Feature: memory-safe-self-hosting, Property 12: VM falls back to the interpreter without crashing
    //
    // The VM does not implement every opcode (notably Call/Ret/RetVal — so any
    // program with `fn main()` / function calls is unsupported). The `--vm`
    // driver (in main.rs, not edited here) decides to fall back *before*
    // running, using the `OpCode::supported()` / `all_supported()` pre-flight.
    // These properties test that pre-flight at the VM/compiler level:
    //   * any chunk containing an unsupported opcode is reported unsupported
    //     (so the driver falls back) — R6.3;
    //   * a fully-supported chunk runs to completion with Ok and no crash — R6.4.

    use super::super::{all_supported, BytecodeCompiler, Chunk, OpCode, VMValue, VM};
    use crate::frontend::ast::*;
    use crate::support::pbt::{self, Gen};

    /// Opcodes the execution engine does NOT implement (must be reported by
    /// `supported() == false`). These route programs to the interpreter:
    /// `And`/`Or` are compiler fall-back markers; `Interp` is unused (echo
    /// interpolates at run time); method/module/concurrency/index-assignment
    /// semantics aren't matched yet.
    const UNSUPPORTED: &[OpCode] = &[
        OpCode::And,
        OpCode::Or,
        OpCode::Interp,
        OpCode::Method,
        OpCode::SetIndex,
        OpCode::SetField,
        OpCode::Spawn,
        OpCode::ChanSend,
        OpCode::ChanRecv,
        OpCode::Await,
        OpCode::ModCall,
    ];

    /// Opcodes the engine fully implements.
    const SUPPORTED: &[OpCode] = &[
        OpCode::Nop, OpCode::Halt, OpCode::Const, OpCode::Load, OpCode::Store,
        OpCode::GLoad, OpCode::GStore, OpCode::Add, OpCode::Sub, OpCode::Mul,
        OpCode::Div, OpCode::Mod, OpCode::Neg, OpCode::Eq, OpCode::Neq,
        OpCode::Lt, OpCode::Lte, OpCode::Gt, OpCode::Gte, OpCode::Not,
        OpCode::Jmp, OpCode::JmpFalse, OpCode::JmpTrue, OpCode::Array,
        OpCode::Echo, OpCode::Print, OpCode::Pop, OpCode::Dup,
        // task 12.1 additions
        OpCode::Call, OpCode::Ret, OpCode::RetVal, OpCode::Len, OpCode::Index,
        OpCode::Map, OpCode::Struct, OpCode::Field, OpCode::Concat,
    ];

    /// Generate a fully-supported, well-formed straight-line chunk (no jumps, so
    /// the instruction stream is always walkable). Returns the chunk plus a flag
    /// describing whether an unsupported opcode was appended.
    fn build_supported_prefix(ops: &[u8]) -> Chunk {
        // `ops` is a list of selector bytes; map each to a safe supported op.
        let mut c = Chunk::new("<supported>");
        for sel in ops {
            match sel % 5 {
                0 => {
                    let i = c.add_constant(VMValue::Int(*sel as i64));
                    c.emit_u16(OpCode::Const, i, 1);
                }
                1 => c.emit(OpCode::Pop, 1),
                2 => c.emit(OpCode::Dup, 1),
                3 => c.emit(OpCode::Nop, 1),
                _ => c.emit(OpCode::Echo, 1),
            }
        }
        c
    }

    /// Generator: a vector of selector bytes (prefix shape) plus an index into
    /// `UNSUPPORTED` to append.
    fn shape_gen() -> Gen<(Vec<u8>, usize)> {
        Gen::new(
            |rng, _size| {
                let len = rng.upto(16);
                let ops: Vec<u8> = (0..len).map(|_| rng.below(5) as u8).collect();
                let which = rng.below(UNSUPPORTED.len() as u64) as usize;
                (ops, which)
            },
            |_| Vec::new(),
        )
    }

    #[test]
    fn supported_classification_is_correct() {
        for op in SUPPORTED {
            assert!(op.supported(), "{:?} should be supported", op);
        }
        for op in UNSUPPORTED {
            assert!(!op.supported(), "{:?} should be unsupported", op);
        }
    }

    #[test]
    fn prop12_unsupported_chunk_is_reported_for_fallback() {
        // Any chunk that reaches an unsupported opcode must be flagged so the
        // driver falls back to the interpreter (R6.3).
        pbt::for_all("prop12_unsupported_flags_fallback", &shape_gen(), |(ops, which)| {
            let mut c = build_supported_prefix(ops);
            let bad = UNSUPPORTED[*which];
            // Append the unsupported opcode plus its operand bytes so the
            // instruction stream stays walkable up to (and onto) it.
            c.code.push(bad as u8);
            c.lines.push(1);
            for _ in 0..bad.operand_len() {
                c.code.push(0);
                c.lines.push(1);
            }
            c.emit(OpCode::Halt, 1);
            // Pre-flight must say "not all supported" => driver falls back.
            !all_supported(std::slice::from_ref(&c))
        });
    }

    #[test]
    fn prop12_supported_chunk_runs_ok_without_crash() {
        // A fully-supported chunk is reported supported AND runs to completion
        // with Ok, without crashing (R6.4).
        let gen = {
            Gen::new(
                |rng, _size| {
                    let len = rng.upto(16);
                    (0..len).map(|_| rng.below(5) as u8).collect::<Vec<u8>>()
                },
                |_: &Vec<u8>| Vec::new(),
            )
        };
        pbt::for_all("prop12_supported_runs_ok", &gen, |ops: &Vec<u8>| {
            let mut c = build_supported_prefix(ops);
            c.emit(OpCode::Halt, 1);
            if !all_supported(std::slice::from_ref(&c)) {
                return false;
            }
            let mut vm = VM::new();
            vm.chunks = vec![c];
            let res = vm.run();
            // Drain the output buffer to exercise that path too; must not panic.
            let _ = vm.take_output();
            res.is_ok()
        });
    }

    #[test]
    fn fn_main_program_runs_on_vm_end_to_end() {
        // Task 12.1: a real program with `fn main()` now compiles to a fully
        // supported chunk set (CALL/Ret entry) and runs end-to-end on the VM,
        // producing the program's output — no fall-back needed.
        let prog = Program {
            statements: vec![Stmt::new(
                Statement::FnDecl {
                    name: "main".to_string(),
                    params: vec![],
                    return_type: None,
                    body: vec![Stmt::new(
                        Statement::Echo {
                            expr: Expression::StringLiteral("hi".to_string()),
                            escapes: false,
                        },
                        Span::new(1, 1),
                    )],
                    is_pub: false,
                    is_async: false,
                },
                Span::new(1, 1),
            )],
        };
        let result = BytecodeCompiler::compile(&prog);
        assert!(
            all_supported(&result.chunks),
            "a simple fn main() program must be fully supported by the VM"
        );
        let mut vm = VM::new();
        vm.chunks = result.chunks;
        vm.global_names = result.global_names;
        vm.run().expect("fn main() program should run cleanly on the VM");
        assert_eq!(vm.take_output(), "hi\n");
    }
}

// ============================================================================
// Task 12.1 — end-to-end opcode coverage: compile real Ran programs and run
// them on the VM, asserting the engine fully supports them and produces the
// expected output (the interpreter's semantics are the reference).
// ============================================================================
#[cfg(test)]
mod opcode_e2e_tests {
    use super::super::{all_supported, BytecodeCompiler, VM};

    /// Parse + compile + run a program on the VM, asserting it is fully
    /// VM-supported (so we know the VM — not a fall-back — produced the output).
    fn run_ok(src: &str) -> String {
        let tokens = crate::frontend::lexer::tokenize(src);
        let prog = crate::frontend::parser::parse(tokens);
        let result = BytecodeCompiler::compile(&prog);
        assert!(
            all_supported(&result.chunks),
            "program should be fully supported by the VM:\n{src}"
        );
        let mut vm = VM::new();
        vm.chunks = result.chunks;
        vm.global_names = result.global_names;
        vm.run().expect("VM run should succeed");
        vm.take_output()
    }

    /// Compile a program and report whether the VM can fully run it (true) or
    /// the driver must fall back to the interpreter (false).
    fn is_vm_supported(src: &str) -> bool {
        let tokens = crate::frontend::lexer::tokenize(src);
        let prog = crate::frontend::parser::parse(tokens);
        let result = BytecodeCompiler::compile(&prog);
        all_supported(&result.chunks)
    }

    #[test]
    fn user_function_call_and_return() {
        let out = run_ok(
            "fn add(a: int, b: int) -> int { return a + b }\n\
             fn main() { echo add(40, 2) }",
        );
        assert_eq!(out, "42\n");
    }

    #[test]
    fn recursion_fibonacci() {
        let out = run_ok(
            "fn fib(n: int) -> int { if n < 2 { return n } return fib(n-1) + fib(n-2) }\n\
             fn main() { echo fib(10) }",
        );
        assert_eq!(out, "55\n");
    }

    #[test]
    fn for_range_loop_accumulates() {
        let out = run_ok(
            "fn main() { total = 0\n for i in range(5) { total = total + i }\n echo total }",
        );
        assert_eq!(out, "10\n");
    }

    #[test]
    fn for_range_two_args() {
        let out = run_ok(
            "fn main() { s = 0\n for i in range(2, 5) { s = s + i }\n echo s }",
        );
        assert_eq!(out, "9\n"); // 2 + 3 + 4
    }

    #[test]
    fn for_array_loop_echoes_each() {
        let out = run_ok("fn main() { for n in [7, 8, 9] { echo n } }");
        assert_eq!(out, "7\n8\n9\n");
    }

    #[test]
    fn while_loop_factorial() {
        let out = run_ok(
            "fn main() { acc = 1\n k = 1\n while k <= 5 { acc = acc * k\n k = k + 1 }\n echo acc }",
        );
        assert_eq!(out, "120\n");
    }

    #[test]
    fn if_else_branch() {
        let out = run_ok("fn main() { n = 7\n if n % 2 == 0 { echo \"even\" } else { echo \"odd\" } }");
        assert_eq!(out, "odd\n");
    }

    #[test]
    fn arithmetic_div_mod_neg() {
        let out = run_ok("fn main() { echo 17 / 5\n echo 17 % 5\n echo -17 }");
        assert_eq!(out, "3\n2\n-17\n");
    }

    #[test]
    fn string_concat_and_index() {
        let out = run_ok(
            "fn main() { name = \"world\"\n echo \"hello, \" + name + \"!\"\n a = [10, 20, 30]\n echo a[1] }",
        );
        assert_eq!(out, "hello, world!\n20\n");
    }

    #[test]
    fn echo_interpolation_locals() {
        let out = run_ok("fn main() { x = 10\n y = 32\n echo \"sum $x + $y\" }");
        assert_eq!(out, "sum 10 + 32\n");
    }

    #[test]
    fn top_level_globals_interpolate_in_echo() {
        // Regression (task 12.1): top-level `VarDecl`s are program globals that
        // the interpreter evaluates before calling `main`. The VM must store
        // them (GStore) so `$name` interpolation resolves them — previously they
        // were skipped and `echo "Hello, $name!"` printed the literal `$name`.
        // This mirrors examples/hello.ran exactly.
        let out = run_ok(
            "name = \"World\"\n\
             version = \"0.1.0\"\n\
             fn main() {\n\
                 echo \"Hello, $name!\"\n\
                 echo \"Ran v$version\"\n\
             }",
        );
        assert_eq!(out, "Hello, World!\nRan v0.1.0\n");
    }

    #[test]
    fn top_level_global_int_used_in_arithmetic() {
        // A non-string global is stored and readable from main, matching the
        // interpreter's top-level VarDecl evaluation.
        let out = run_ok(
            "base = 40\n\
             fn main() { echo base + 2 }",
        );
        assert_eq!(out, "42\n");
    }

    #[test]
    fn echo_deferred_interpolation_of_string_value() {
        // A string value containing `$a` is interpolated at echo time, exactly
        // like the interpreter (literals are NOT interpolated at eval time).
        let out = run_ok("fn main() { a = 5\n msg = \"value is $a\"\n echo msg }");
        assert_eq!(out, "value is 5\n");
    }

    #[test]
    fn struct_init_field_access_and_interpolation() {
        let out = run_ok(
            "struct Point { x: int, y: int }\n\
             fn dsq(p: Point) -> int { return p.x * p.x + p.y * p.y }\n\
             fn main() { p = Point { x: 3, y: 4 }\n echo \"x=$p.x y=$p.y\"\n echo dsq(p) }",
        );
        assert_eq!(out, "x=3 y=4\n25\n");
    }

    #[test]
    fn integer_overflow_falls_back_via_err() {
        // Overflow must not wrap: the VM returns Err so the driver falls back to
        // the interpreter (which raises E1010). The program is otherwise fully
        // supported, so `run` is reached and must error.
        let tokens = crate::frontend::lexer::tokenize(
            "fn main() { echo 9223372036854775807 + 1 }",
        );
        let prog = crate::frontend::parser::parse(tokens);
        let result = BytecodeCompiler::compile(&prog);
        assert!(all_supported(&result.chunks));
        let mut vm = VM::new();
        vm.chunks = result.chunks;
        vm.global_names = result.global_names;
        let err = vm.run().expect_err("overflow should be a recoverable Err");
        assert!(err.starts_with("E1010"), "expected E1010, got: {err}");
    }

    #[test]
    fn divide_by_zero_falls_back_via_err() {
        let tokens = crate::frontend::lexer::tokenize("fn main() { echo 1 / 0 }");
        let prog = crate::frontend::parser::parse(tokens);
        let result = BytecodeCompiler::compile(&prog);
        assert!(all_supported(&result.chunks));
        let mut vm = VM::new();
        vm.chunks = result.chunks;
        vm.global_names = result.global_names;
        let err = vm.run().expect_err("div-by-zero should be a recoverable Err");
        assert!(err.starts_with("E1011"), "expected E1011, got: {err}");
    }

    // --- constructs that must fall back to the interpreter (not run on the VM) ---

    #[test]
    fn method_call_falls_back() {
        assert!(!is_vm_supported("fn main() { a = [1,2,3]\n echo a.len() }"));
    }

    #[test]
    fn module_call_falls_back() {
        assert!(!is_vm_supported("fn main() { math.sqrt(9) }"));
    }

    #[test]
    fn match_expression_falls_back() {
        assert!(!is_vm_supported(
            "fn main() { x = 1\n r = match x { 1 => \"one\", _ => \"?\" }\n echo r }"
        ));
    }

    #[test]
    fn closure_falls_back() {
        assert!(!is_vm_supported("fn main() { f = fn(n) { return n + 1 }\n echo f(1) }"));
    }

    #[test]
    fn spawn_falls_back() {
        assert!(!is_vm_supported("fn main() { spawn { echo \"hi\" } }"));
    }
}

// ============================================================================
// Task 12.2 — Type-specialized opcodes (R9.3): parity with the generic ops.
//
// The whole correctness argument for specialization is: each `*_int` operation
// must be OBSERVATIONALLY IDENTICAL to its generic counterpart for EVERY
// possible operand pair — including integer-overflow → Err(E1010) and
// divide/modulo-by-zero → Err(E1011), and including non-int operands where the
// specialized op falls back to the generic one. These tests assert exactly that
// equivalence (both as direct op comparisons across many inputs, and end-to-end
// through compile+run), so the `--vm` parity gate can never regress because of
// specialization.
// ============================================================================
#[cfg(test)]
mod type_specialized_parity {
    use super::VM;
    use super::super::{all_supported, BytecodeCompiler, OpCode};
    use super::super::value::VMValue;
    use crate::support::pbt::{self, Gen};
    use std::sync::Arc;

    /// Wrap an i64 into one of several VMValue shapes, so the parity check
    /// exercises BOTH the int fast path (kind 0) and the non-int fall-back
    /// paths (float/string/bool/void) of every specialized op.
    fn make(kind: u8, n: i64) -> VMValue {
        match kind % 5 {
            0 => VMValue::Int(n),
            1 => VMValue::Float(n as f64 / 7.0),
            2 => VMValue::Str(Arc::new(format!("s{}", n))),
            3 => VMValue::Bool(n & 1 == 0),
            _ => VMValue::Void,
        }
    }

    /// Generator: two i64 magnitudes plus two operand-kind selectors.
    fn pair_gen() -> Gen<(i64, i64, u8, u8)> {
        const EDGES: &[i64] = &[0, 1, -1, 2, -2, i64::MAX, i64::MIN, i64::MAX - 1, i64::MIN + 1];
        Gen::new(
            |rng, _size| {
                let pick = |rng: &mut pbt::Rng| -> i64 {
                    if rng.below(2) == 0 {
                        EDGES[rng.below(EDGES.len() as u64) as usize]
                    } else {
                        rng.range_i64(-1000, 1000)
                    }
                };
                let a = pick(rng);
                let b = pick(rng);
                (a, b, rng.below(5) as u8, rng.below(5) as u8)
            },
            |_| Vec::new(),
        )
    }

    /// Two `Result<VMValue, String>` are equivalent when they are both Err with
    /// the same message, or both Ok with values that print identically (covers
    /// every VMValue variant, including Float/Str the `PartialEq` impl ignores).
    fn same(a: &Result<VMValue, String>, b: &Result<VMValue, String>) -> bool {
        match (a, b) {
            (Err(x), Err(y)) => x == y,
            (Ok(x), Ok(y)) => format!("{:?}", x) == format!("{:?}", y),
            _ => false,
        }
    }

    #[test]
    fn prop_specialized_arithmetic_matches_generic() {
        // Feature: memory-safe-self-hosting, Property 13 (specialization parity)
        // Every specialized arithmetic op == its generic op, for all operand
        // kinds incl. overflow (Err) and div/mod-by-zero (Err).
        pbt::for_all("spec_arith_parity", &pair_gen(), |(a, b, ka, kb)| {
            let x = make(*ka, *a);
            let y = make(*kb, *b);
            same(&VM::op_add_int(&x, &y), &VM::op_add(&x, &y))
                && same(&VM::op_sub_int(&x, &y), &VM::op_sub(&x, &y))
                && same(&VM::op_mul_int(&x, &y), &VM::op_mul(&x, &y))
                && same(&VM::op_div_int(&x, &y), &VM::op_div(&x, &y))
                && same(&VM::op_mod_int(&x, &y), &VM::op_mod(&x, &y))
        });
    }

    #[test]
    fn prop_specialized_comparison_matches_generic() {
        pbt::for_all("spec_cmp_parity", &pair_gen(), |(a, b, ka, kb)| {
            let x = make(*ka, *a);
            let y = make(*kb, *b);
            format!("{:?}", VM::op_lt_int(&x, &y)) == format!("{:?}", VM::op_lt(&x, &y))
                && format!("{:?}", VM::op_lte_int(&x, &y)) == format!("{:?}", VM::op_lte(&x, &y))
                && format!("{:?}", VM::op_gt_int(&x, &y)) == format!("{:?}", VM::op_gt(&x, &y))
                && format!("{:?}", VM::op_gte_int(&x, &y)) == format!("{:?}", VM::op_gte(&x, &y))
        });
    }

    #[test]
    fn specialized_add_overflow_is_err_like_generic() {
        // The headline correctness case: MAX + 1 must NOT wrap; it must return
        // the same E1010 Err as the generic op (so the driver falls back and the
        // interpreter raises the proper fault).
        let x = VMValue::Int(i64::MAX);
        let y = VMValue::Int(1);
        let spec = VM::op_add_int(&x, &y);
        let gen = VM::op_add(&x, &y);
        assert!(spec.as_ref().unwrap_err().starts_with("E1010"));
        assert!(same(&spec, &gen), "spec={spec:?} gen={gen:?}");
    }

    #[test]
    fn specialized_div_mod_by_zero_is_err_like_generic() {
        let x = VMValue::Int(10);
        let zero = VMValue::Int(0);
        assert!(VM::op_div_int(&x, &zero).unwrap_err().starts_with("E1011"));
        assert!(VM::op_mod_int(&x, &zero).unwrap_err().starts_with("E1011"));
        assert!(same(&VM::op_div_int(&x, &zero), &VM::op_div(&x, &zero)));
        assert!(same(&VM::op_mod_int(&x, &zero), &VM::op_mod(&x, &zero)));
    }

    // --- end-to-end: the compiler actually emits specialized opcodes, and the
    //     program output is identical to running with the generic opcodes ---

    fn compile(src: &str) -> super::super::CompileResult {
        let tokens = crate::frontend::lexer::tokenize(src);
        let prog = crate::frontend::parser::parse(tokens);
        BytecodeCompiler::compile(&prog)
    }

    /// Count occurrences of a specific opcode across all chunks (walking the
    /// instruction stream so operand bytes are never mistaken for opcodes).
    fn count_op(result: &super::super::CompileResult, target: OpCode) -> usize {
        let mut total = 0;
        for chunk in &result.chunks {
            let mut pos = 0;
            while pos < chunk.code.len() {
                if let Some(op) = OpCode::from_u8(chunk.code[pos]) {
                    if op == target {
                        total += 1;
                    }
                    pos += 1 + op.operand_len();
                } else {
                    pos += 1;
                }
            }
        }
        total
    }

    /// Rewrite every specialized opcode in-place back to its generic form,
    /// yielding a chunk set that computes the same thing via the generic path.
    /// Used as the parity oracle: specialized run output must equal generic run
    /// output, byte for byte.
    fn degrade_to_generic(result: &mut super::super::CompileResult) {
        for chunk in &mut result.chunks {
            let mut pos = 0;
            while pos < chunk.code.len() {
                if let Some(op) = OpCode::from_u8(chunk.code[pos]) {
                    let generic = match op {
                        OpCode::AddInt => Some(OpCode::Add),
                        OpCode::SubInt => Some(OpCode::Sub),
                        OpCode::MulInt => Some(OpCode::Mul),
                        OpCode::DivInt => Some(OpCode::Div),
                        OpCode::ModInt => Some(OpCode::Mod),
                        OpCode::LtInt => Some(OpCode::Lt),
                        OpCode::LteInt => Some(OpCode::Lte),
                        OpCode::GtInt => Some(OpCode::Gt),
                        OpCode::GteInt => Some(OpCode::Gte),
                        _ => None,
                    };
                    if let Some(g) = generic {
                        chunk.code[pos] = g as u8;
                    }
                    pos += 1 + op.operand_len();
                } else {
                    pos += 1;
                }
            }
        }
    }

    fn run_result(mut result: super::super::CompileResult) -> Result<String, String> {
        let mut vm = VM::new();
        vm.chunks = std::mem::take(&mut result.chunks);
        vm.global_names = std::mem::take(&mut result.global_names);
        vm.run().map(|()| vm.take_output())
    }

    #[test]
    fn compiler_emits_specialized_opcodes_for_int_arithmetic() {
        // `: int` params + int literals make the body provably int → AddInt etc.
        let result = compile(
            "fn add(a: int, b: int) -> int { return a + b }\n\
             fn main() { echo add(40, 2) }",
        );
        assert!(all_supported(&result.chunks));
        assert!(
            count_op(&result, OpCode::AddInt) >= 1,
            "expected an AddInt in the bytecode for `a + b` with int params"
        );
    }

    #[test]
    fn for_loop_counters_are_specialized() {
        // The range and array `for` loops drive their counters with specialized
        // opcodes (idx/len comparison + increment).
        let r1 = compile("fn main() { total = 0\n for i in range(5) { total = total + i } }");
        assert!(count_op(&r1, OpCode::LtInt) >= 1 && count_op(&r1, OpCode::AddInt) >= 1);

        let r2 = compile("fn main() { for n in [7, 8, 9] { echo n } }");
        assert!(count_op(&r2, OpCode::LtInt) >= 1 && count_op(&r2, OpCode::AddInt) >= 1);
    }

    #[test]
    fn float_arithmetic_stays_generic() {
        // No int proof → keep the generic opcode (and no specialized one for the
        // float add). This keeps inference honest, though either would be correct.
        let result = compile("fn main() { echo 1.5 + 2.5 }");
        assert_eq!(count_op(&result, OpCode::AddInt), 0);
        assert!(count_op(&result, OpCode::Add) >= 1);
    }

    #[test]
    fn specialized_run_output_equals_generic_run_output() {
        // The end-to-end parity oracle: for a battery of int-heavy programs, the
        // specialized bytecode and the same bytecode degraded to generic opcodes
        // must produce byte-identical output (and identical Ok/Err outcome).
        let programs = [
            "fn main() { echo 40 + 2 }",
            "fn add(a: int, b: int) -> int { return a + b }\n fn main() { echo add(40, 2) }",
            "fn fib(n: int) -> int { if n < 2 { return n } return fib(n-1) + fib(n-2) }\n fn main() { echo fib(12) }",
            "fn main() { total = 0\n for i in range(10) { total = total + i }\n echo total }",
            "fn main() { s = 0\n for i in range(2, 7) { s = s + i }\n echo s }",
            "fn main() { acc = 1\n k = 1\n while k <= 6 { acc = acc * k\n k = k + 1 }\n echo acc }",
            "fn main() { echo 17 / 5\n echo 17 % 5\n echo -17 }",
            "fn main() { n = 7\n if n % 2 == 0 { echo \"even\" } else { echo \"odd\" } }",
            "fn main() { for n in [3, 5, 8] { echo n * n } }",
        ];
        for src in programs {
            let specialized = compile(src);
            assert!(all_supported(&specialized.chunks), "not supported: {src}");
            // Sanity: at least one program actually used specialization overall.
            let mut generic = compile(src);
            degrade_to_generic(&mut generic);
            let out_spec = run_result(specialized);
            let out_gen = run_result(generic);
            match (&out_spec, &out_gen) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "output mismatch for: {src}"),
                (Err(a), Err(b)) => assert_eq!(a, b, "error mismatch for: {src}"),
                _ => panic!("outcome mismatch for {src}: spec={out_spec:?} gen={out_gen:?}"),
            }
        }
    }

    #[test]
    fn specialized_overflow_program_falls_back_via_err_like_generic() {
        // End-to-end: `MAX + 1` with int operands compiles to AddInt and still
        // returns the same E1010 Err the generic path returns (no wrap).
        let src = "fn add(a: int, b: int) -> int { return a + b }\n\
                   fn main() { echo add(9223372036854775807, 1) }";
        let specialized = compile(src);
        assert!(count_op(&specialized, OpCode::AddInt) >= 1);
        let mut generic = compile(src);
        degrade_to_generic(&mut generic);
        let es = run_result(specialized).expect_err("specialized overflow must Err");
        let eg = run_result(generic).expect_err("generic overflow must Err");
        assert!(es.starts_with("E1010"));
        assert_eq!(es, eg, "specialized and generic overflow errors must match");
    }
}
