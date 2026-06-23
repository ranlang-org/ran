//! End-to-end tests: run the `ran` binary against small programs and assert on
//! stdout, stderr, and exit codes. These lock in language-level behavior
//! (control flow, arithmetic safety, diagnostics, stdlib).

use std::io::Write;
use std::process::{Command, Output};

/// Write `src` to a temp .ran file and run the `ran` binary on it.
fn run(src: &str) -> Output {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("ran_it_{}.ran", nonce()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(src.as_bytes()).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_ran"))
        .arg(&path)
        .output()
        .expect("failed to run ran");
    let _ = std::fs::remove_file(&path);
    out
}

fn nonce() -> u128 {
    // Collision-free across parallel test binaries: high bits = process id
    // (unique per test executable), low bits = a per-process atomic counter
    // (unique per call). A bare nanosecond timestamp could collide when two
    // binaries run a test in the same instant, clobbering each other's temp
    // file and flaking an unrelated test.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    ((std::process::id() as u128) << 64) | (COUNTER.fetch_add(1, Ordering::Relaxed) as u128)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}
fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

#[test]
fn decimal_cobol_money_helpers() {
    // COBOL-grade business helpers: PICTURE-style formatting, fixed rescale,
    // exact batch sum, min/max, percent, and banker's rounding — all exact.
    let out = run(r#"
import "std::decimal" as decimal
import "std::str" as str
fn main() {
    echo decimal.format(dec("1234567.5"), 2)
    echo decimal.format(dec("1234567.5"), 2, ".", ",")
    echo decimal.format(dec("-1234"), 0)
    echo str.from(decimal.to_fixed(dec("19.996"), 2))
    echo str.from(decimal.sum([dec("19.99"), dec("5.00"), dec("0.01")]))
    echo str.from(decimal.to_fixed(decimal.percent(dec("100.00"), dec("8.25")), 2))
    echo str.from(decimal.round(dec("2.5"), 0, "bankers"))
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines[0], "1,234,567.50");
    assert_eq!(lines[1], "1.234.567,50");
    assert_eq!(lines[2], "-1,234");
    assert_eq!(lines[3], "20.00");
    assert_eq!(lines[4], "25.00");
    assert_eq!(lines[5], "8.25");
    assert_eq!(lines[6], "2");
}

#[test]
fn unused_variable_and_import_warn_but_run() {
    // Unused `let`/`var` and unused imports are warnings (W0601/W0602): the
    // program still runs (exit 0), but the diagnostics appear on stderr.
    let out = run(r#"
import "std::time" as unused_mod
fn main() {
    let dead = 10
    let live = 5
    let _ignored = 1
    echo "live = $live"
}
"#);
    assert_eq!(code(&out), 0, "unused lints must not be fatal: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "live = 5");
    let err = stderr(&out);
    assert!(err.contains("W0601"), "expected unused-variable warning: {}", err);
    assert!(err.contains("dead"), "should name `dead`: {}", err);
    assert!(err.contains("W0602"), "expected unused-import warning: {}", err);
    assert!(!err.contains("live"), "`live` is used (interp), must not warn: {}", err);
    assert!(!err.contains("_ignored"), "`_`-prefixed must be silent: {}", err);
}

#[test]
fn let_is_immutable_var_is_mutable() {
    // Reassigning a `let` binding is the hard error E0100 (even in default mode).
    let out = run(r#"
fn main() {
    let x = 5
    x = 6
    echo "$x"
}
"#);
    assert_ne!(code(&out), 0, "reassigning a `let` must fail");
    assert!(stderr(&out).contains("E0100"), "stderr: {}", stderr(&out));

    // `var` and `let mut` allow reassignment; params too.
    let ok = run(r#"
fn bump(n: int) -> int { n = n + 1; return n }
fn main() {
    var a = 1
    a = a + 1
    let mut b = 10
    b = b + 1
    let c = bump(7)
    echo "$a $b $c"
}
"#);
    assert_eq!(code(&ok), 0, "stderr: {}", stderr(&ok));
    assert_eq!(stdout(&ok).trim(), "2 11 8");
}

#[test]
fn var_keyword_is_mutable_go_style() {
    // `var` (Go-style) declares a mutable binding — lighter than `let mut`.
    // `let` is the immutable form; a bare `x = ...` also declares/assigns.
    let out = run(r#"
fn main() {
    var total = 0
    let limit = 100
    count = 0
    for n in range(1, 11) {
        total = total + n
        count = count + 1
    }
    var label: str = "sum"
    echo "$label = $total over $count (limit $limit)"
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "sum = 55 over 10 (limit 100)");
}

#[test]
fn return_inside_loop_exits_function() {
    let out = run(r#"
fn first_even(n: int) -> int {
    for x in range(n) {
        if x > 0 {
            if x % 2 == 0 { return x }
        }
    }
    return 0 - 1
}
fn main() { echo first_even(10) }
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "2");
}

#[test]
fn division_by_zero_aborts_with_e1011() {
    let out = run(r#"
fn main() {
    let d = 0
    echo 10 / d
}
"#);
    assert_eq!(code(&out), 70);
    assert!(stderr(&out).contains("E1011"), "stderr: {}", stderr(&out));
}

#[test]
fn integer_overflow_aborts_with_e1010() {
    let out = run(r#"
fn main() {
    let big = 9223372036854775807
    echo big + 1
}
"#);
    assert_eq!(code(&out), 70);
    assert!(stderr(&out).contains("E1010"), "stderr: {}", stderr(&out));
}

#[test]
fn undefined_variable_is_e0001() {
    let out = run(r#"
fn main() { echo "$missing" }
"#);
    // missing var inside interpolation is fine; reference it directly instead
    let out2 = run(r#"
fn main() { echo missing_value }
"#);
    let _ = out;
    assert_eq!(code(&out2), 1);
    assert!(stderr(&out2).contains("E0001"), "stderr: {}", stderr(&out2));
}

#[test]
fn syntax_error_aborts_before_running() {
    let out = run("fn main() {\n  let x = \n  echo \"hi\"\n}\n");
    assert_eq!(code(&out), 1);
    let e = stderr(&out);
    assert!(e.contains("E0101") || e.contains("E0100"), "stderr: {}", e);
    // The program body must NOT have executed.
    assert!(!stdout(&out).contains("hi"));
}

#[test]
fn arity_mismatch_is_e0003() {
    let out = run(r#"
fn add(a: int, b: int) -> int { return a + b }
fn main() { echo add(1) }
"#);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("E0003"), "stderr: {}", stderr(&out));
}

#[test]
fn stdlib_str_helpers() {
    let out = run(r#"
import "std::str" as s
fn main() {
    echo s.pad_left("7", 4, "0")
    echo s.index_of("hello world", "world")
    echo s.to_int("  42 ") + 1
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines[0], "0007");
    assert_eq!(lines[1], "6");
    assert_eq!(lines[2], "43");
}

#[test]
fn json_round_trip() {
    let out = run(r#"
import "std::json" as json
fn main() {
    let data = json.decode("{\"n\": 7, \"name\": \"ran\"}")
    echo data["n"]
    echo data["name"]
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("7"));
    assert!(s.contains("ran"));
}

#[test]
fn log_writes_to_stderr_not_stdout() {
    let out = run(r#"
import "std::log" as log
fn main() {
    log.info("hello")
    echo "stdout-line"
}
"#);
    assert_eq!(code(&out), 0);
    assert!(stdout(&out).contains("stdout-line"));
    assert!(!stdout(&out).contains("INFO"));
    assert!(stderr(&out).contains("INFO"));
}

#[test]
fn http_client_https_now_supported() {
    // TLS is implemented via system OpenSSL: https no longer returns a
    // "not supported" error. With network it returns a status; offline it
    // returns a tls/connect error — never the old "no TLS" message.
    let out = run(r#"
import "std::http" as http
fn main() {
    let r = http.fetch("https://example.com")
    echo r["error"]
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(!stdout(&out).to_lowercase().contains("not supported"));
}

#[test]
fn decimal_money_is_exact() {
    let out = run(r#"
fn main() {
    let a = dec("0.1")
    let b = dec("0.2")
    echo a + b
    echo dec("19.99") * dec("3")
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines[0], "0.3");      // not 0.30000000000000004
    assert_eq!(lines[1], "59.97");
}

#[test]
fn decimal_rounding_modes() {
    let out = run(r#"
import "std::decimal" as decimal
fn main() {
    echo decimal.round(dec("2.5"), 0, "half_up")
    echo decimal.round(dec("2.5"), 0, "half_even")
    echo decimal.div(dec("10"), dec("3"), 2, "half_up")
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines[0], "3");
    assert_eq!(lines[1], "2");
    assert_eq!(lines[2], "3.33");
}

#[test]
fn decimal_division_by_zero_aborts() {
    let out = run(r#"
fn main() { echo dec("1") / dec("0") }
"#);
    assert_eq!(code(&out), 70);
    assert!(stderr(&out).contains("E1002"), "stderr: {}", stderr(&out));
}

#[test]
fn decimal_typeof() {
    let out = run(r#"
fn main() { echo typeof(dec("1.00")) }
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "decimal");
}

#[test]
fn struct_methods_and_fields() {
    let out = run(r#"
struct Account { owner: str, balance: decimal }
impl Account {
    fn deposit(self, amt) -> Account {
        return Account { owner: self.owner, balance: self.balance + amt }
    }
    fn describe(self) -> str { return self.owner + ":" + self.balance }
}
fn main() {
    let a = Account { owner: "Risqi", balance: dec("100.00") }
    let b = a.deposit(dec("50.25"))
    echo b.describe()
    echo b.balance
    echo typeof(b)
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines[0], "Risqi:150.25");
    assert_eq!(lines[1], "150.25");
    assert_eq!(lines[2], "Account");
}

#[test]
fn associated_constructor_and_interpolation() {
    let out = run(r#"
struct User { name: str, age: int }
impl User {
    fn new(n, a) -> User { return User { name: n, age: a } }
}
fn main() {
    let u = User.new("Alice", 30)
    echo "name=$u.name age=$u.age"
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "name=Alice age=30");
}

#[test]
fn http_client_handles_https_scheme() {
    // Network-lenient: with connectivity a valid TLS site returns 2xx; offline
    // it returns a graceful error. Either way: no panic and exit code 0.
    let out = run(r#"
import "std::http" as http
fn main() {
    let r = http.fetch("https://example.com")
    echo "status=" + r["status"]
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("status="));
}

#[test]
fn https_invalid_scheme_is_rejected() {
    let out = run(r#"
import "std::http" as http
fn main() {
    let r = http.fetch("ftp://example.com")
    echo r["error"]
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert!(stdout(&out).to_lowercase().contains("invalid url"));
}

#[test]
fn crypto_hashing_and_encoding() {
    let out = run(r#"
import "std::crypto" as crypto
fn main() {
    echo crypto.sha256("abc")
    echo crypto.hmac_sha256("Jefe", "what do ya want for nothing?")
    echo crypto.base64("hello")
    echo crypto.base64_decode("aGVsbG8=")
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let body = stdout(&out);
    let s: Vec<&str> = body.lines().collect();
    assert_eq!(s[0], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(s[1], "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    assert_eq!(s[2], "aGVsbG8=");
    assert_eq!(s[3], "hello");
}

#[test]
fn lexical_scoping() {
    let out = run(r#"
let g = "global"
fn sees_global() -> str { return g }
fn main() {
    let total = 0
    for x in [10, 20, 30] {
        total = total + x
        let temp = x * 2
    }
    echo "total=$total"
    echo "leak_x=$x"
    echo "fn=" + sees_global()
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("total=60"), "accumulation broke: {}", s);
    assert!(s.contains("leak_x=$x"), "loop var leaked: {}", s);
    assert!(s.contains("fn=global"), "fn saw caller local: {}", s);
}

#[test]
fn block_locals_do_not_leak() {
    let out = run(r#"
fn main() {
    if true {
        let secret = "x"
        echo $secret
    }
    echo "after=$secret"
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("after=$secret"), "block local leaked: {}", s);
}

#[test]
fn enum_and_match() {
    let out = run(r#"
enum Status { Active, Inactive, Pending }
fn label(s) -> str {
    return match s {
        Status.Active => "active"
        Status.Inactive => "inactive"
        _ => "other"
    }
}
fn grade(n: int) -> str {
    return match n {
        100 => "perfect"
        other => "scored " + other
    }
}
fn main() {
    echo label(Status.Active)
    echo label(Status.Pending)
    echo grade(100)
    echo grade(73)
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines[0], "active");
    assert_eq!(lines[1], "other");
    assert_eq!(lines[2], "perfect");
    assert_eq!(lines[3], "scored 73");
}

#[test]
fn match_statement_with_actions() {
    let out = run(r#"
fn main() {
    let day = "sat"
    match day {
        "sat" => echo "weekend"
        _ => echo "weekday"
    }
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "weekend");
}

#[test]
fn env_typed_getters() {
    let out = run(r#"
import "std::env" as env
fn main() {
    env.set("PORT", "8443")
    env.set("DEBUG", "yes")
    env.set("RATE", "0.11")
    echo env.int("PORT", 80)
    echo env.bool("DEBUG", false)
    echo env.decimal("RATE", "0.00")
    echo env.get_or("MISSING", "default")
    echo env.int("MISSING_INT", 7)
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines[0], "8443");
    assert_eq!(lines[1], "true");
    assert_eq!(lines[2], "0.11");
    assert_eq!(lines[3], "default");
    assert_eq!(lines[4], "7");
}

#[test]
fn json_unicode_paths_and_validity() {
    let out = run(r#"
import "std::json" as json
fn main() {
    let obj = json.decode("{\"u\":{\"name\":\"caf\u00e9\",\"tags\":[\"a\",\"b\"]}}")
    echo json.get(obj, "u.name")
    echo json.get(obj, "u.tags.1")
    echo json.valid("{\"ok\":true}")
    echo json.valid("{oops")
    echo json.encode(obj)
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines[0], "café");
    assert_eq!(lines[1], "b");
    assert_eq!(lines[2], "true");
    assert_eq!(lines[3], "false");
    assert!(lines[4].contains("café"));
}

#[test]
fn json_encode_escapes_control_chars() {
    let out = run(r#"
import "std::json" as json
fn main() {
    # decode a JSON string containing a real newline, then re-encode it
    let s = json.decode("\"a\nb\"")
    let m = map()
    set(m, "line", s)
    let enc = json.encode(m)
    echo enc
    # the re-encoded output must contain an escaped newline, not a raw one
    if json.valid(enc) { echo "roundtrip-valid" }
}
"#);
    assert_eq!(code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("\\n"), "expected escaped newline in: {}", s);
    assert!(s.contains("roundtrip-valid"));
}

// ===========================================================================
// Short-circuit `&&` / `||` (R: queued language fix). The interpreter now
// evaluates the right operand of `&&`/`||` ONLY when its value can change the
// result, matching the native AOT path (C `&&`/`||`). These end-to-end tests
// run via the `ran` binary on BOTH the default engine and the forced
// interpreter (`--interp`) so the two paths stay in lockstep.
// ===========================================================================

/// Like `run`, but passes extra CLI flags (e.g. `--interp`) before the file.
fn run_with_flags(src: &str, flags: &[&str]) -> Output {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("ran_it_{}.ran", nonce()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(src.as_bytes()).unwrap();
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ran"));
    for flag in flags {
        cmd.arg(flag);
    }
    let out = cmd.arg(&path).output().expect("failed to run ran");
    let _ = std::fs::remove_file(&path);
    out
}

#[test]
fn and_short_circuits_skips_out_of_bounds_index() {
    // `false && a[99] > 0` must NOT touch a[99] (no E1012); prints "ok".
    let src = r#"
fn main() {
    let a = [1, 2, 3]
    if false && a[99] > 0 { echo "x" } else { echo "ok" }
}
"#;
    for flags in [&[][..], &["--interp"][..]] {
        let out = run_with_flags(src, flags);
        assert_eq!(code(&out), 0, "flags={:?} stderr: {}", flags, stderr(&out));
        assert!(!stderr(&out).contains("E1012"), "flags={:?} unexpected bounds fault: {}", flags, stderr(&out));
        assert_eq!(stdout(&out).trim(), "ok", "flags={:?}", flags);
    }
}

#[test]
fn or_short_circuits_skips_div_by_zero() {
    // `true || (1/0) > 0` must NOT evaluate 1/0 (no E1011); prints "ok".
    let src = r#"
fn main() {
    if true || (1 / 0) > 0 { echo "ok" }
}
"#;
    for flags in [&[][..], &["--interp"][..]] {
        let out = run_with_flags(src, flags);
        assert_eq!(code(&out), 0, "flags={:?} stderr: {}", flags, stderr(&out));
        assert!(!stderr(&out).contains("E1011"), "flags={:?} unexpected div-by-zero fault: {}", flags, stderr(&out));
        assert_eq!(stdout(&out).trim(), "ok", "flags={:?}", flags);
    }
}

#[test]
fn and_bounds_guard_loop_does_not_fault_at_boundary() {
    // The classic guard pattern `i < n && a[i] > 0`: when i reaches n the right
    // side must be skipped, so a[n] is never read out of bounds.
    let src = r#"
fn main() {
    let a = [5, 4, 3]
    let n = 3
    let i = 0
    let count = 0
    while i < n && a[i] > 0 {
        count = count + 1
        i = i + 1
    }
    echo count
}
"#;
    for flags in [&[][..], &["--interp"][..]] {
        let out = run_with_flags(src, flags);
        assert_eq!(code(&out), 0, "flags={:?} stderr: {}", flags, stderr(&out));
        assert!(!stderr(&out).contains("E1012"), "flags={:?} boundary fault: {}", flags, stderr(&out));
        assert_eq!(stdout(&out).trim(), "3", "flags={:?}", flags);
    }
}

#[test]
fn and_short_circuit_skips_right_side_effects() {
    // The right operand's observable side effects (a `&mut` increment) must NOT
    // happen when the left side already decides the result.
    let src = r#"
fn bump(c: &mut int) -> bool {
    c = c + 1
    return true
}
fn main() {
    let calls = 0
    if false && bump(&mut calls) { echo "x" }
    echo calls
}
"#;
    for flags in [&[][..], &["--interp"][..]] {
        let out = run_with_flags(src, flags);
        assert_eq!(code(&out), 0, "flags={:?} stderr: {}", flags, stderr(&out));
        // calls stays 0 because bump() was short-circuited away.
        assert_eq!(stdout(&out).trim(), "0", "flags={:?}", flags);
    }
}

#[test]
fn or_short_circuit_skips_right_side_effects() {
    // Mirror of the `&&` case for `||`: a truthy left skips the right call.
    let src = r#"
fn bump(c: &mut int) -> bool {
    c = c + 1
    return true
}
fn main() {
    let calls = 0
    if true || bump(&mut calls) { echo "ok" }
    echo calls
}
"#;
    for flags in [&[][..], &["--interp"][..]] {
        let out = run_with_flags(src, flags);
        assert_eq!(code(&out), 0, "flags={:?} stderr: {}", flags, stderr(&out));
        let s = stdout(&out);
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "ok", "flags={:?}", flags);
        assert_eq!(lines[1], "0", "flags={:?}", flags);
    }
}
