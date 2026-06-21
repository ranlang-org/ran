//! Property 13 — VM and interpreter produce equivalent output.
//!
//! Feature: memory-safe-self-hosting, Property 13: VM and interpreter produce
//! equivalent output.
//!
//! Model-based property test (std-only PBT harness, ≥100 cases). The bytecode
//! VM is the *implementation under test*; an independent Rust oracle plays the
//! role of the *reference model*. For every generated program drawn from the
//! VM-supported subset we assert that the VM's stdout equals the model's
//! stdout for the same program.
//!
//! ## Why a model oracle rather than the live interpreter
//!
//! The tree-walking interpreter emits output with `println!`/`print!` straight
//! to the *process* stdout (see `runtime/builtins.rs` and `runtime/mod.rs`).
//! There is no in-process output sink to capture, and redirecting the global
//! stdout file descriptor would corrupt every *other* unit test that runs in
//! parallel under `cargo test`. The task brief explicitly allows this case:
//! when true in-process stdout capture of the interpreter is not feasible,
//! compute the expected output with an *independent* Rust oracle for the
//! generated program shapes and assert `VM output == oracle`. That is exactly
//! what this module does — the same approach already used by the Property 14 /
//! specialization-parity tests in this crate.
//!
//! The oracle is sound because:
//!   * it is an independent implementation of the language semantics for the
//!     generated subset (it never shares code with the VM), and
//!   * its value formatting matches the language exactly — `VMValue`'s
//!     `Display` and the interpreter's `Value` `Display` are identical (int →
//!     decimal, bool → `true`/`false`, string → verbatim), and `echo` appends
//!     a newline while interpolating `$name` against the live scope. The
//!     existing hand-written end-to-end tests in `exec.rs` (e.g.
//!     `recursion_fibonacci`, `for_range_loop_accumulates`,
//!     `struct_init_field_access_and_interpolation`) confirm these are the
//!     very strings the interpreter produces.
//!
//! ## Generated subset (only constructs `OpCode::supported()` / `all_supported`)
//!
//! integer/string/bool literals and variables; arithmetic `+ - * / %`;
//! comparisons; `if`/`else`; `while`; `for ... in range(..)`; `for ... in
//! <array>`; user `fn` definitions, calls, and recursion; `echo`; string
//! concatenation; `echo` interpolation of locals/globals (incl. dotted struct
//! fields); arrays + indexing; structs + field access. Names, values,
//! structure, and nesting all vary. Everything generated is deterministic — no
//! RNG/time/map-ordering — so output equality is meaningful, and operands are
//! bounded so no integer overflow / divide-by-zero ever forces a VM fallback.

#![cfg(test)]

use super::{all_supported, BytecodeCompiler, VM};
use crate::support::pbt::{self, Gen, Rng};

// ============================================================================
// Generated program: carries the rendered Ran source plus the oracle's exact
// expected stdout. `Debug` prints the source on failure for easy diagnosis.
// ============================================================================

#[derive(Clone)]
struct GenProg {
    src: String,
    expected: String,
}

impl std::fmt::Debug for GenProg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "\n--- generated program ---\n{}\n--- expected stdout ({} bytes) ---\n{}\n-------------------------",
            self.src,
            self.expected.len(),
            self.expected
        )
    }
}

// ============================================================================
// Program builder + oracle. Each snippet appends both source (to `defs`/`main`)
// and the exact stdout it produces (to `expected`), evaluating the same
// semantics the VM implements. Fresh, numerically-suffixed names keep every
// snippet self-contained, so there is no cross-snippet scope interference.
// ============================================================================

struct Builder {
    defs: String,    // top-level struct/fn declarations
    main: String,    // statements inside fn main()
    expected: String, // accumulated, exact expected stdout
    counter: usize,  // unique-name source
}

impl Builder {
    fn new() -> Self {
        Builder {
            defs: String::new(),
            main: String::new(),
            expected: String::new(),
            counter: 0,
        }
    }

    /// A fresh, collision-free identifier (never a keyword).
    fn fresh(&mut self, prefix: &str) -> String {
        let n = self.counter;
        self.counter += 1;
        format!("{}{}", prefix, n)
    }

    fn line(&mut self, s: &str) {
        self.main.push_str(s);
        self.main.push('\n');
    }

    /// Record one `echo`-produced line (value display + newline).
    fn echo_out(&mut self, s: &str) {
        self.expected.push_str(s);
        self.expected.push('\n');
    }

    fn finish(self) -> GenProg {
        let src = format!("{}fn main() {{\n{}}}\n", self.defs, self.main);
        GenProg {
            src,
            expected: self.expected,
        }
    }
}

// --- safe value generators ---------------------------------------------------

/// A short string literal from a safe charset: ASCII letters/digits/spaces
/// only — never `$`, quotes, braces or backslashes — so it is neither
/// interpolated nor needs escaping.
fn safe_word(rng: &mut Rng) -> String {
    const WORDS: &[&str] = &[
        "alpha", "beta", "gamma", "hello", "world", "foo", "bar", "ok", "data",
        "node", "ran", "value", "ready", "42 cats", "go go", "left right",
    ];
    let a = *rng.choose(WORDS);
    if rng.below(2) == 0 {
        a.to_string()
    } else {
        let b = *rng.choose(WORDS);
        format!("{} {}", a, b)
    }
}

/// Generate an integer expression as `(source, value)`. Pure literals and
/// nested binary ops only (parenthesized so precedence can never diverge).
/// Operands are bounded (literals 0..=20, depth ≤ 2, divisors 1..=9) so the
/// result NEVER overflows `i64` and division/modulo is never by zero — the VM
/// therefore always runs it (no overflow/`E1011` fallback), and the Rust oracle
/// (with overflow-checks ON in test builds) never panics.
fn gen_int_expr(rng: &mut Rng, depth: usize) -> (String, i64) {
    if depth == 0 || rng.below(2) == 0 {
        let v = rng.range_i64(0, 20);
        return (format!("{}", v), v);
    }
    let (ls, lv) = gen_int_expr(rng, depth - 1);
    match rng.below(5) {
        0 => {
            let (rs, rv) = gen_int_expr(rng, depth - 1);
            (format!("({} + {})", ls, rs), lv + rv)
        }
        1 => {
            let (rs, rv) = gen_int_expr(rng, depth - 1);
            (format!("({} - {})", ls, rs), lv - rv)
        }
        2 => {
            let (rs, rv) = gen_int_expr(rng, depth - 1);
            (format!("({} * {})", ls, rs), lv * rv)
        }
        3 => {
            let d = rng.range_i64(1, 9);
            (format!("({} / {})", ls, d), lv / d)
        }
        _ => {
            let d = rng.range_i64(2, 9);
            (format!("({} % {})", ls, d), lv % d)
        }
    }
}

// --- snippet kinds -----------------------------------------------------------

/// `v = <int expr>` then `echo v` (covers int literals, variables, arithmetic).
fn snip_echo_int(b: &mut Builder, rng: &mut Rng) {
    let name = b.fresh("v");
    let (src, val) = gen_int_expr(rng, 2);
    b.line(&format!("{} = {}", name, src));
    b.line(&format!("echo {}", name));
    b.echo_out(&val.to_string());
}

/// `echo "literal"` and a string-concat echo (covers strings + `+` concat).
fn snip_echo_str(b: &mut Builder, rng: &mut Rng) {
    let w1 = safe_word(rng);
    let w2 = safe_word(rng);
    let name = b.fresh("s");
    // s = "w1" + " " + "w2"
    b.line(&format!("{} = \"{}\" + \" \" + \"{}\"", name, w1, w2));
    b.line(&format!("echo {}", name));
    b.echo_out(&format!("{} {}", w1, w2));
}

/// `echo b` for a bool literal (covers bool literal + bool display).
fn snip_echo_bool(b: &mut Builder, rng: &mut Rng) {
    let val = rng.boolean();
    let name = b.fresh("flag");
    b.line(&format!("{} = {}", name, val));
    b.line(&format!("echo {}", name));
    b.echo_out(&val.to_string());
}

/// `if a <cmp> b { echo T } else { echo F }` (covers comparisons + branching).
fn snip_if_else(b: &mut Builder, rng: &mut Rng) {
    let a = rng.range_i64(0, 10);
    let c = rng.range_i64(0, 10);
    let (op, taken) = match rng.below(6) {
        0 => ("==", a == c),
        1 => ("!=", a != c),
        2 => ("<", a < c),
        3 => ("<=", a <= c),
        4 => (">", a > c),
        _ => (">=", a >= c),
    };
    let tword = safe_word(rng);
    let eword = safe_word(rng);
    b.line(&format!("if {} {} {} {{", a, op, c));
    b.line(&format!("echo \"{}\"", tword));
    b.line("} else {");
    b.line(&format!("echo \"{}\"", eword));
    b.line("}");
    b.echo_out(if taken { &tword } else { &eword });
}

/// `total = 0; i = 0; while i < N { total = total + i; i = i + 1 }; echo total`.
fn snip_while_sum(b: &mut Builder, rng: &mut Rng) {
    let n = rng.range_i64(0, 8);
    let total = b.fresh("total");
    let i = b.fresh("i");
    b.line(&format!("{} = 0", total));
    b.line(&format!("{} = 0", i));
    b.line(&format!("while {} < {} {{", i, n));
    b.line(&format!("{} = {} + {}", total, total, i));
    b.line(&format!("{} = {} + 1", i, i));
    b.line("}");
    b.line(&format!("echo {}", total));
    let sum: i64 = (0..n).sum();
    b.echo_out(&sum.to_string());
}

/// `acc = 0; for j in range(N) { acc = acc + j }; echo acc`.
fn snip_for_range_sum(b: &mut Builder, rng: &mut Rng) {
    let n = rng.range_i64(0, 8);
    let acc = b.fresh("acc");
    let j = b.fresh("j");
    b.line(&format!("{} = 0", acc));
    b.line(&format!("for {} in range({}) {{", j, n));
    b.line(&format!("{} = {} + {}", acc, acc, j));
    b.line("}");
    b.line(&format!("echo {}", acc));
    let sum: i64 = (0..n).sum();
    b.echo_out(&sum.to_string());
}

/// `for j in range(N) { echo j }` or `range(A, B)` (covers both range forms).
fn snip_for_range_echo(b: &mut Builder, rng: &mut Rng) {
    let j = b.fresh("j");
    if rng.below(2) == 0 {
        let n = rng.range_i64(1, 6);
        b.line(&format!("for {} in range({}) {{", j, n));
        b.line(&format!("echo {}", j));
        b.line("}");
        for k in 0..n {
            b.echo_out(&k.to_string());
        }
    } else {
        let a = rng.range_i64(0, 4);
        let len = rng.range_i64(0, 5);
        let bend = a + len;
        b.line(&format!("for {} in range({}, {}) {{", j, a, bend));
        b.line(&format!("echo {}", j));
        b.line("}");
        for k in a..bend {
            b.echo_out(&k.to_string());
        }
    }
}

/// Nested control flow: `for j in range(N) { if j % 2 == 0 { echo j } }`.
fn snip_for_nested_if(b: &mut Builder, rng: &mut Rng) {
    let n = rng.range_i64(1, 9);
    let j = b.fresh("j");
    b.line(&format!("for {} in range({}) {{", j, n));
    b.line(&format!("if {} % 2 == 0 {{", j));
    b.line(&format!("echo {}", j));
    b.line("}");
    b.line("}");
    for k in 0..n {
        if k % 2 == 0 {
            b.echo_out(&k.to_string());
        }
    }
}

/// `arr = [..]; for x in arr { echo x }` then `echo arr[idx]` (array + index).
fn snip_array(b: &mut Builder, rng: &mut Rng) {
    let len = rng.upto(4) + 1; // 1..=5
    let vals: Vec<i64> = (0..len).map(|_| rng.range_i64(-9, 30)).collect();
    let arr = b.fresh("arr");
    let elems: Vec<String> = vals.iter().map(|v| v.to_string()).collect();
    b.line(&format!("{} = [{}]", arr, elems.join(", ")));
    // iterate
    let x = b.fresh("x");
    b.line(&format!("for {} in {} {{", x, arr));
    b.line(&format!("echo {}", x));
    b.line("}");
    for v in &vals {
        b.echo_out(&v.to_string());
    }
    // index a valid element
    let idx = rng.below(len as u64) as usize;
    b.line(&format!("echo {}[{}]", arr, idx));
    b.echo_out(&vals[idx].to_string());
}

/// Struct def + init + field access + dotted interpolation.
fn snip_struct(b: &mut Builder, rng: &mut Rng) {
    let nfields = 2 + rng.below(2) as usize; // 2 or 3
    let sname = b.fresh("S");
    // Top-level struct declaration.
    let field_decls: Vec<String> = (0..nfields).map(|k| format!("f{}: int", k)).collect();
    b.defs
        .push_str(&format!("struct {} {{ {} }}\n", sname, field_decls.join(", ")));
    // Instance with known field values.
    let fvals: Vec<i64> = (0..nfields).map(|_| rng.range_i64(-20, 50)).collect();
    let inits: Vec<String> = (0..nfields)
        .map(|k| format!("f{}: {}", k, fvals[k]))
        .collect();
    let p = b.fresh("p");
    b.line(&format!("{} = {} {{ {} }}", p, sname, inits.join(", ")));
    // Direct field access echo.
    b.line(&format!("echo {}.f0", p));
    b.echo_out(&fvals[0].to_string());
    // Dotted interpolation of the last field.
    let last = nfields - 1;
    b.line(&format!("echo \"got ${}.f{} done\"", p, last));
    b.echo_out(&format!("got {} done", fvals[last]));
}

/// Interpolation of an int local and a string local (locals/globals).
fn snip_interpolation(b: &mut Builder, rng: &mut Rng) {
    let (isrc, ival) = gen_int_expr(rng, 2);
    let iname = b.fresh("iv");
    let sname = b.fresh("sv");
    let word = safe_word(rng);
    b.line(&format!("{} = {}", iname, isrc));
    b.line(&format!("{} = \"{}\"", sname, word));
    b.line(&format!("echo \"n=${} s=${} end\"", iname, sname));
    b.echo_out(&format!("n={} s={} end", ival, word));
}

/// Non-recursive user function `add(a, b)` defined + called.
fn snip_fn_add(b: &mut Builder, rng: &mut Rng) {
    let fname = b.fresh("add");
    b.defs.push_str(&format!(
        "fn {}(a: int, b: int) -> int {{ return a + b }}\n",
        fname
    ));
    let a = rng.range_i64(-30, 30);
    let c = rng.range_i64(-30, 30);
    b.line(&format!("echo {}({}, {})", fname, a, c));
    b.echo_out(&(a + c).to_string());
}

/// Recursive user function (sum-to-n or factorial) defined + called.
fn snip_recursion(b: &mut Builder, rng: &mut Rng) {
    if rng.below(2) == 0 {
        // sum 1..=n
        let fname = b.fresh("sumto");
        b.defs.push_str(&format!(
            "fn {f}(n: int) -> int {{\nif n <= 0 {{\nreturn 0\n}}\nreturn n + {f}(n - 1)\n}}\n",
            f = fname
        ));
        let n = rng.range_i64(0, 10);
        b.line(&format!("echo {}({})", fname, n));
        let v: i64 = (1..=n).sum();
        b.echo_out(&v.to_string());
    } else {
        // factorial (n ≤ 7 keeps the result well within i64)
        let fname = b.fresh("fact");
        b.defs.push_str(&format!(
            "fn {f}(n: int) -> int {{\nif n <= 1 {{\nreturn 1\n}}\nreturn n * {f}(n - 1)\n}}\n",
            f = fname
        ));
        let n = rng.range_i64(0, 7);
        b.line(&format!("echo {}({})", fname, n));
        let mut v: i64 = 1;
        let mut k = 2;
        while k <= n {
            v *= k;
            k += 1;
        }
        b.echo_out(&v.to_string());
    }
}

const SNIPPETS: &[fn(&mut Builder, &mut Rng)] = &[
    snip_echo_int,
    snip_echo_str,
    snip_echo_bool,
    snip_if_else,
    snip_while_sum,
    snip_for_range_sum,
    snip_for_range_echo,
    snip_for_nested_if,
    snip_array,
    snip_struct,
    snip_interpolation,
    snip_fn_add,
    snip_recursion,
];

/// Generator: assemble a whole program from 2..=7 random snippets.
fn prog_gen() -> Gen<GenProg> {
    Gen::new(
        |rng, _size| {
            let mut b = Builder::new();
            let n = 2 + rng.upto(5); // 2..=7 snippets
            for _ in 0..n {
                let idx = rng.below(SNIPPETS.len() as u64) as usize;
                SNIPPETS[idx](&mut b, rng);
            }
            b.finish()
        },
        // Random programs do not shrink structurally; `Debug` already prints the
        // full source + expected output, which is enough to reproduce/diagnose.
        |_| Vec::new(),
    )
}

// ============================================================================
// VM driver: compile + run a program on the VM, asserting the engine fully
// supports it (so we compare the VM's real output, never an interpreter
// fallback), and return its captured stdout.
// ============================================================================

fn vm_output(src: &str) -> Result<String, String> {
    let tokens = crate::frontend::lexer::tokenize(src);
    let prog = crate::frontend::parser::parse(tokens);
    let result = BytecodeCompiler::compile(&prog);
    if !all_supported(&result.chunks) {
        return Err("program was not fully VM-supported (would fall back)".to_string());
    }
    let mut vm = VM::new();
    vm.chunks = result.chunks;
    vm.global_names = result.global_names;
    vm.run()?;
    Ok(vm.take_output())
}

// ============================================================================
// The property.
// ============================================================================

// Feature: memory-safe-self-hosting, Property 13: VM and interpreter produce equivalent output
//
// Validates: Requirements 9.2, 9.3
#[test]
fn prop13_vm_output_equivalent_to_interpreter() {
    pbt::for_all(
        "P13 VM↔interpreter output equivalence",
        &prog_gen(),
        |p: &GenProg| {
            match vm_output(&p.src) {
                // The VM must (a) fully support the program and (b) produce
                // exactly the reference (interpreter-equivalent) stdout.
                Ok(out) => out == p.expected,
                Err(_) => false,
            }
        },
    );
}

// --- harness sanity unit tests (guard the oracle/runner themselves) ----------

#[test]
fn oracle_matches_vm_for_a_known_program() {
    // A hand-written program touching most supported constructs; the literal
    // expected string is exactly what the interpreter prints (cross-checked by
    // the e2e tests in exec.rs), so this guards that `vm_output` + the oracle
    // formatting agree on a fixed case.
    let src = "struct P { x: int, y: int }\n\
               fn add(a: int, b: int) -> int { return a + b }\n\
               fn main() {\n\
               echo add(40, 2)\n\
               total = 0\n\
               for j in range(5) { total = total + j }\n\
               echo total\n\
               p = P { x: 3, y: 4 }\n\
               echo p.x\n\
               echo \"y=$p.y end\"\n\
               }\n";
    let out = vm_output(src).expect("known program must run on the VM");
    assert_eq!(out, "42\n10\n3\ny=4 end\n");
}

#[test]
fn every_generated_program_is_fully_vm_supported() {
    // Independent of the equivalence assertion, confirm the generator only ever
    // emits the VM-supported subset (so Property 13 never silently degrades
    // into a no-op via fallback).
    let mut rng = Rng::new(0xC0FFEE);
    let gen = prog_gen();
    for _ in 0..200 {
        let p = gen.generate(&mut rng, 32);
        let tokens = crate::frontend::lexer::tokenize(&p.src);
        let prog = crate::frontend::parser::parse(tokens);
        let result = BytecodeCompiler::compile(&prog);
        assert!(
            all_supported(&result.chunks),
            "generated program must be fully VM-supported:{:?}",
            p
        );
    }
}
