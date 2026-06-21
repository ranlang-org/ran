//! Bytecode instruction set for the Ran VM.

/// Each instruction is a single byte opcode followed by operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OpCode {
    /// No operation
    Nop = 0,

    // --- Constants & Variables ---
    /// Push constant from pool: CONST <idx:u16>
    Const = 1,
    /// Load local variable: LOAD <slot:u16>
    Load = 2,
    /// Store to local variable: STORE <slot:u16>
    Store = 3,
    /// Load global variable: GLOAD <idx:u16>
    GLoad = 4,
    /// Store global variable: GSTORE <idx:u16>
    GStore = 5,

    // --- Arithmetic ---
    /// Add top two values
    Add = 10,
    /// Subtract
    Sub = 11,
    /// Multiply
    Mul = 12,
    /// Divide
    Div = 13,
    /// Modulo
    Mod = 14,
    /// Negate (unary -)
    Neg = 15,

    // --- Type-specialized arithmetic (task 12.2, R9.3) ---
    // Emitted by the compiler ONLY when both operands are statically proven to
    // be integers (conservative compiler-local int inference, see
    // `BytecodeCompiler::infer_int`). They let the VM take an int-first fast
    // path that skips the generic operand type-match. CRITICAL: each has the
    // EXACT same observable semantics as its generic counterpart — including
    // checked overflow → recoverable `Err` (E1010) and divide/modulo-by-zero →
    // `Err` (E1011) — and falls back to the identical generic operation for any
    // non-int operands, so parity is preserved for every possible input.
    /// Integer-specialized add (== Add for all inputs)
    AddInt = 16,
    /// Integer-specialized subtract (== Sub for all inputs)
    SubInt = 17,
    /// Integer-specialized multiply (== Mul for all inputs)
    MulInt = 18,
    /// Integer-specialized divide (== Div for all inputs)
    DivInt = 19,

    // --- Comparison ---
    /// Equal
    Eq = 20,
    /// Not equal
    Neq = 21,
    /// Less than
    Lt = 22,
    /// Less than or equal
    Lte = 23,
    /// Greater than
    Gt = 24,
    /// Greater than or equal
    Gte = 25,

    // --- Type-specialized comparison + modulo (task 12.2, R9.3) ---
    // Same contract as the specialized arithmetic above: emitted only when both
    // operands are statically proven int, identical observable behavior to the
    // generic opcode for every input (modulo-by-zero → `Err` E1011), int-first
    // fast path with a generic fallback for non-int operands.
    /// Integer-specialized modulo (== Mod for all inputs)
    ModInt = 26,
    /// Integer-specialized `<` (== Lt for all inputs)
    LtInt = 27,
    /// Integer-specialized `<=` (== Lte for all inputs)
    LteInt = 28,
    /// Integer-specialized `>` (== Gt for all inputs)
    GtInt = 29,
    /// Integer-specialized `>=` (== Gte for all inputs)
    GteInt = 33,

    // --- Logic ---
    /// Logical NOT
    Not = 30,
    /// Logical AND (short-circuit)
    And = 31,
    /// Logical OR (short-circuit)
    Or = 32,

    // --- Control Flow ---
    /// Unconditional jump: JMP <offset:i16>
    Jmp = 40,
    /// Jump if false: JF <offset:i16>
    JmpFalse = 41,
    /// Jump if true: JT <offset:i16>
    JmpTrue = 42,

    // --- Functions ---
    /// Call function: CALL <arg_count:u8>
    Call = 50,
    /// Return from function
    Ret = 51,
    /// Return with value
    RetVal = 52,

    // --- Data Structures ---
    /// Create array: ARRAY <count:u16>
    Array = 60,
    /// Create map: MAP <count:u16> (count = number of key-value pairs)
    Map = 61,
    /// Index access: obj[idx]
    Index = 62,
    /// Field access: obj.field (field name idx in constant pool)
    Field = 63,
    /// Set index: obj[idx] = val
    SetIndex = 64,
    /// Set field: obj.field = val
    SetField = 65,
    /// Length of array/string/map on top of stack (no operand). Used by `for`
    /// loops to bound iteration without materializing a counter externally.
    Len = 66,

    // --- Strings ---
    /// String concatenation
    Concat = 70,
    /// String interpolation: INTERP <segment_count:u8>
    Interp = 71,

    // --- Methods & OOP ---
    /// Method call: METHOD <name_idx:u16> <arg_count:u8>
    Method = 80,
    /// Construct struct: STRUCT <type_idx:u16> <field_count:u8>
    Struct = 81,

    // --- Concurrency ---
    /// Spawn task: SPAWN <fn_idx:u16>
    Spawn = 90,
    /// Channel send
    ChanSend = 91,
    /// Channel receive
    ChanRecv = 92,
    /// Await future
    Await = 93,

    // --- Built-ins ---
    /// Echo (print with interpolation)
    Echo = 100,
    /// Print without interpolation
    Print = 101,
    /// Module method call: MODCALL <module_idx:u16> <method_idx:u16> <arg_count:u8>
    ModCall = 102,

    // --- Stack ---
    /// Pop top of stack
    Pop = 110,
    /// Duplicate top of stack
    Dup = 111,

    // --- VM Control ---
    /// Halt execution
    Halt = 255,
}

impl OpCode {
    /// Number of operand bytes that follow this opcode in the bytecode stream.
    /// Single source of truth used by the disassembler and the VM pre-flight
    /// support scan.
    pub fn operand_len(&self) -> usize {
        match self {
            // u16 operand
            OpCode::Const | OpCode::Load | OpCode::Store | OpCode::GLoad
            | OpCode::GStore | OpCode::Array | OpCode::Map | OpCode::Field
            | OpCode::SetField | OpCode::Spawn => 2,
            // i16 jump offset
            OpCode::Jmp | OpCode::JmpFalse | OpCode::JmpTrue => 2,
            // u8 operand
            OpCode::Call | OpCode::Interp => 1,
            // u16 + u8
            OpCode::Method | OpCode::Struct => 3,
            // u16 + u16 + u8
            OpCode::ModCall => 5,
            // no operand
            _ => 0,
        }
    }

    /// Whether the execution engine (`exec.rs`) implements this opcode. The VM is
    /// experimental; a program whose bytecode uses any unsupported opcode is run
    /// on the tree-walking interpreter instead (no partial/incorrect output).
    ///
    /// Implemented (task 12.1): user function calls (`Call`/`Ret`/`RetVal`),
    /// arrays + `Len`/`Index`, `Map`, structs + `Field`, `Concat`, on top of the
    /// arithmetic/comparison/control-flow core. Still unsupported (so programs
    /// using them fall back to the interpreter): `And`/`Or` (used as compiler
    /// fall-back markers for trait decls and `break`/`continue`), string
    /// `Interp` (echo interpolation is done at run time, not via this opcode),
    /// `Method`/`ModCall` (method + module/stdlib calls), `SetIndex`/`SetField`
    /// (index/field assignment), `Spawn`, and the channel/await opcodes. These
    /// involve semantics (concurrency ordering, stdlib side effects, mutation
    /// through references) the VM cannot yet match the interpreter on exactly,
    /// so falling back keeps output correct (R6.3 / R9.1).
    pub fn supported(&self) -> bool {
        matches!(
            self,
            OpCode::Nop | OpCode::Halt | OpCode::Const | OpCode::Load | OpCode::Store
                | OpCode::GLoad | OpCode::GStore | OpCode::Add | OpCode::Sub | OpCode::Mul
                | OpCode::Div | OpCode::Mod | OpCode::Neg | OpCode::Eq | OpCode::Neq
                | OpCode::Lt | OpCode::Lte | OpCode::Gt | OpCode::Gte | OpCode::Not
                | OpCode::Jmp | OpCode::JmpFalse | OpCode::JmpTrue | OpCode::Array
                | OpCode::Echo | OpCode::Print | OpCode::Pop | OpCode::Dup
                // --- task 12.1 additions ---
                | OpCode::Call | OpCode::Ret | OpCode::RetVal
                | OpCode::Len | OpCode::Index | OpCode::Map
                | OpCode::Struct | OpCode::Field | OpCode::Concat
                // --- task 12.2 type-specialized opcodes (R9.3) ---
                | OpCode::AddInt | OpCode::SubInt | OpCode::MulInt | OpCode::DivInt
                | OpCode::ModInt | OpCode::LtInt | OpCode::LteInt | OpCode::GtInt
                | OpCode::GteInt
        )
    }

    pub fn from_u8(byte: u8) -> Option<Self> {
        // Safety: we validate the range
        match byte {
            0 => Some(Self::Nop),
            1 => Some(Self::Const),
            2 => Some(Self::Load),
            3 => Some(Self::Store),
            4 => Some(Self::GLoad),
            5 => Some(Self::GStore),
            10 => Some(Self::Add),
            11 => Some(Self::Sub),
            12 => Some(Self::Mul),
            13 => Some(Self::Div),
            14 => Some(Self::Mod),
            15 => Some(Self::Neg),
            16 => Some(Self::AddInt),
            17 => Some(Self::SubInt),
            18 => Some(Self::MulInt),
            19 => Some(Self::DivInt),
            20 => Some(Self::Eq),
            21 => Some(Self::Neq),
            22 => Some(Self::Lt),
            23 => Some(Self::Lte),
            24 => Some(Self::Gt),
            25 => Some(Self::Gte),
            26 => Some(Self::ModInt),
            27 => Some(Self::LtInt),
            28 => Some(Self::LteInt),
            29 => Some(Self::GtInt),
            30 => Some(Self::Not),
            31 => Some(Self::And),
            32 => Some(Self::Or),
            33 => Some(Self::GteInt),
            40 => Some(Self::Jmp),
            41 => Some(Self::JmpFalse),
            42 => Some(Self::JmpTrue),
            50 => Some(Self::Call),
            51 => Some(Self::Ret),
            52 => Some(Self::RetVal),
            60 => Some(Self::Array),
            61 => Some(Self::Map),
            62 => Some(Self::Index),
            63 => Some(Self::Field),
            64 => Some(Self::SetIndex),
            65 => Some(Self::SetField),
            66 => Some(Self::Len),
            70 => Some(Self::Concat),
            71 => Some(Self::Interp),
            80 => Some(Self::Method),
            81 => Some(Self::Struct),
            90 => Some(Self::Spawn),
            91 => Some(Self::ChanSend),
            92 => Some(Self::ChanRecv),
            93 => Some(Self::Await),
            100 => Some(Self::Echo),
            101 => Some(Self::Print),
            102 => Some(Self::ModCall),
            110 => Some(Self::Pop),
            111 => Some(Self::Dup),
            255 => Some(Self::Halt),
            _ => None,
        }
    }
}
