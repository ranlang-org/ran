//! Bytecode chunk - a compiled unit of code (function body, module, etc.)

use super::value::VMValue;
use super::opcodes::OpCode;

/// A chunk of bytecode with its constant pool
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Raw bytecode
    pub code: Vec<u8>,
    /// Constant pool (strings, numbers, etc.)
    pub constants: Vec<VMValue>,
    /// Line number mapping: bytecode offset -> source line
    pub lines: Vec<usize>,
    /// Name of this chunk (function name, "<main>", etc.)
    pub name: String,
    /// Number of local variable slots needed
    pub local_count: u16,
    /// Local-slot names, indexed by slot. Lets the VM resolve `$name`
    /// interpolation against this frame's locals (most-recent name per slot).
    pub local_names: Vec<String>,
}

impl Chunk {
    pub fn new(name: &str) -> Self {
        Self {
            code: Vec::new(),
            constants: Vec::new(),
            lines: Vec::new(),
            name: name.to_string(),
            local_count: 0,
            local_names: Vec::new(),
        }
    }

    /// Emit a single-byte instruction
    pub fn emit(&mut self, op: OpCode, line: usize) {
        self.code.push(op as u8);
        self.lines.push(line);
    }

    /// Emit opcode + u8 operand
    pub fn emit_u8(&mut self, op: OpCode, operand: u8, line: usize) {
        self.code.push(op as u8);
        self.code.push(operand);
        self.lines.push(line);
        self.lines.push(line);
    }

    /// Emit opcode + u16 operand (big-endian)
    pub fn emit_u16(&mut self, op: OpCode, operand: u16, line: usize) {
        self.code.push(op as u8);
        self.code.push((operand >> 8) as u8);
        self.code.push((operand & 0xFF) as u8);
        self.lines.push(line);
        self.lines.push(line);
        self.lines.push(line);
    }

    /// Emit opcode + i16 operand (for jumps)
    pub fn emit_i16(&mut self, op: OpCode, offset: i16, line: usize) {
        self.code.push(op as u8);
        let bytes = offset.to_be_bytes();
        self.code.push(bytes[0]);
        self.code.push(bytes[1]);
        self.lines.push(line);
        self.lines.push(line);
        self.lines.push(line);
    }

    /// Add a constant and return its index
    pub fn add_constant(&mut self, value: VMValue) -> u16 {
        self.constants.push(value);
        (self.constants.len() - 1) as u16
    }

    /// Current bytecode length (useful for jump patching)
    pub fn len(&self) -> usize {
        self.code.len()
    }

    /// Patch a jump offset at a given position
    pub fn patch_jump(&mut self, offset_pos: usize, target: usize) {
        let jump = (target as isize - offset_pos as isize - 2) as i16;
        let bytes = jump.to_be_bytes();
        self.code[offset_pos] = bytes[0];
        self.code[offset_pos + 1] = bytes[1];
    }

    /// Read u16 at position
    pub fn read_u16(&self, pos: usize) -> u16 {
        ((self.code[pos] as u16) << 8) | (self.code[pos + 1] as u16)
    }

    /// Read i16 at position
    pub fn read_i16(&self, pos: usize) -> i16 {
        i16::from_be_bytes([self.code[pos], self.code[pos + 1]])
    }

    /// Human-readable disassembly of this chunk (best-effort: operand widths
    /// follow the documented instruction encoding). Used for the build-time
    /// `target/<name>.bc.txt` dump so developers can inspect the bytecode the
    /// compiler emits.
    pub fn disassemble(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "== chunk '{}' (locals: {}, constants: {}, {} bytes) ==\n",
            self.name,
            self.local_count,
            self.constants.len(),
            self.code.len()
        ));
        let mut pos = 0usize;
        while pos < self.code.len() {
            let off = pos;
            let byte = self.code[pos];
            pos += 1;
            let op = match OpCode::from_u8(byte) {
                Some(o) => o,
                None => {
                    out.push_str(&format!("{:04}  .byte 0x{:02x}\n", off, byte));
                    continue;
                }
            };
            let name = format!("{:?}", op).to_uppercase();
            match op {
                // Single u16 operand.
                OpCode::Const => {
                    let idx = self.read_u16(pos);
                    pos += 2;
                    let cval = self
                        .constants
                        .get(idx as usize)
                        .map(|c| format!("{}", c))
                        .unwrap_or_default();
                    out.push_str(&format!("{:04}  {:<8} {:<5} ; {}\n", off, name, idx, cval));
                }
                OpCode::Load
                | OpCode::Store
                | OpCode::GLoad
                | OpCode::GStore
                | OpCode::Array
                | OpCode::Map
                | OpCode::Field
                | OpCode::SetField
                | OpCode::Spawn => {
                    let v = self.read_u16(pos);
                    pos += 2;
                    out.push_str(&format!("{:04}  {:<8} {}\n", off, name, v));
                }
                // Signed i16 jump offset.
                OpCode::Jmp | OpCode::JmpFalse | OpCode::JmpTrue => {
                    let v = self.read_i16(pos);
                    pos += 2;
                    out.push_str(&format!("{:04}  {:<8} {:+} (-> {})\n", off, name, v, off as i32 + 3 + v as i32));
                }
                // Single u8 operand.
                OpCode::Call | OpCode::Interp => {
                    let v = self.code.get(pos).copied().unwrap_or(0);
                    pos += 1;
                    out.push_str(&format!("{:04}  {:<8} {}\n", off, name, v));
                }
                // u16 + u8.
                OpCode::Method | OpCode::Struct => {
                    let a = self.read_u16(pos);
                    pos += 2;
                    let b = self.code.get(pos).copied().unwrap_or(0);
                    pos += 1;
                    out.push_str(&format!("{:04}  {:<8} {} {}\n", off, name, a, b));
                }
                // u16 + u16 + u8.
                OpCode::ModCall => {
                    let a = self.read_u16(pos);
                    pos += 2;
                    let b = self.read_u16(pos);
                    pos += 2;
                    let c = self.code.get(pos).copied().unwrap_or(0);
                    pos += 1;
                    out.push_str(&format!("{:04}  {:<8} {} {} {}\n", off, name, a, b, c));
                }
                // No operand.
                _ => {
                    out.push_str(&format!("{:04}  {}\n", off, name));
                }
            }
        }
        out
    }
}
