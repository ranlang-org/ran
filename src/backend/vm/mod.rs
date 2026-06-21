//! Bytecode Virtual Machine - high-performance execution engine.
//!
//! ⚠️ STATUS: EXPERIMENTAL — NOT WIRED INTO EXECUTION.
//! As of now, both `ran run` and compiled binaries execute via the
//! tree-walking interpreter in `runtime/`. This VM is the foundation for the
//! performance roadmap (compiling the AST to bytecode for faster execution),
//! but nothing dispatches to it yet. Keep it compiling, or remove it if the
//! roadmap changes. See docs/16-roadmap.md.
//!
//! Design (target):
//! - Register-based VM (faster than stack-based for most operations)
//! - Compact bytecode format
//! - Constant pool for strings/numbers
//! - Call frames for function invocation
//! - GC-free: ownership tracked at compile time

#![allow(unused_imports)]

mod opcodes;
mod chunk;
mod value;
mod exec;
pub mod compiler;

#[cfg(test)]
mod equivalence_property;

pub use opcodes::OpCode;
pub use chunk::Chunk;
pub use value::VMValue;
pub use exec::VM;
pub use compiler::{BytecodeCompiler, CompileResult};

/// Whether every opcode in every chunk is implemented by the VM (`exec.rs`).
/// Used as a pre-flight check: if any chunk uses an unsupported opcode, the
/// program is run on the interpreter instead — so the VM never produces partial
/// or incorrect output for a program it cannot fully execute.
pub fn all_supported(chunks: &[Chunk]) -> bool {
    for chunk in chunks {
        let mut pos = 0usize;
        while pos < chunk.code.len() {
            match OpCode::from_u8(chunk.code[pos]) {
                Some(op) => {
                    if !op.supported() {
                        return false;
                    }
                    pos += 1 + op.operand_len();
                }
                None => return false,
            }
        }
    }
    true
}

/// Disassemble a whole compile result (all chunks + globals) into text, for the
/// build-time `target/<name>.bc.txt` dump.
pub fn disassemble(result: &CompileResult) -> String {
    let mut out = String::new();
    out.push_str("; Ran bytecode disassembly (experimental VM target)\n");
    out.push_str(&format!("; globals: {}\n", result.global_names.len()));
    for (i, g) in result.global_names.iter().enumerate() {
        out.push_str(&format!(";   [{}] {}\n", i, g));
    }
    out.push('\n');
    for chunk in &result.chunks {
        out.push_str(&chunk.disassemble());
        out.push('\n');
    }
    out
}

/// Compile a program to bytecode and execute it on the VM (the `--vm` path).
///
/// EXPERIMENTAL: the VM covers core arithmetic, comparisons, control flow,
/// functions, arrays, and `echo`/`print`. Module calls (http/web/db/log),
/// structs, maps, and methods are not implemented in the VM yet, so programs
/// using them will not behave correctly under `--vm`. The tree-walking
/// interpreter remains the complete, default engine.
pub fn run(program: &crate::frontend::ast::Program) {
    let result = BytecodeCompiler::compile(program);
    let mut vm = VM::new();
    vm.chunks = result.chunks;
    vm.global_names = result.global_names;
    let _ = vm.run();
    // Flush whatever the program emitted (Echo/Print are buffered in the VM).
    print!("{}", vm.take_output());
}
