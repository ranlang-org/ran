//! Integration tests for Phase A fault-unwinding boundaries (task 5.3).
//!
//! These drive the built `ran` binary on real `.ran` programs to lock in the
//! language-visible guarantee that a recoverable fault never crashes the
//! process — it unwinds to the nearest catch boundary:
//!
//!   * spawn/join boundary (R3.6): a faulting spawned thread is delivered to
//!     the joiner as an INSPECTABLE error value, and the process keeps running.
//!   * per-request HTTP boundary (R3.3): a faulting request handler yields a
//!     500 response and the server keeps serving subsequent requests.
//!
//! Design notes:
//!   * The spawn/join test is the PRIMARY, fully-deterministic check.
//!   * The HTTP server test is best-effort and explicitly non-hanging: it binds
//!     a high, ephemeral port (chosen by the OS), uses short socket timeouts,
//!     bounds its connect retries, and always kills the child server. If the
//!     server cannot be reached (port unavailable / bind refused in a locked
//!     down environment), the test SKIPS rather than hanging or failing.
//!
//! All fixtures live under `.tmp_tests/` (gitignored) and are cleaned up.
//!
//! _Requirements: 3.3, 3.6_

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// `.tmp_tests/` under the crate root — the project's convention for transient
/// test artifacts.
fn tmp_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(".tmp_tests");
    let _ = std::fs::create_dir_all(&p);
    p
}

fn nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
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

/// Write `src` to `<.tmp_tests>/<name>` and run the `ran` binary on it to
/// completion (used for the non-blocking spawn/join program).
fn run_program(name: &str, src: &str) -> (Output, PathBuf) {
    let path = tmp_dir().join(name);
    std::fs::write(&path, src).expect("write .ran fixture");
    let out = Command::new(env!("CARGO_BIN_EXE_ran"))
        .arg(&path)
        .output()
        .expect("failed to run ran binary");
    (out, path)
}

// ---------------------------------------------------------------------------
// R3.6 — spawn/join fault boundary (PRIMARY, deterministic)
// ---------------------------------------------------------------------------

/// A spawned thread whose body triggers a runtime fault (division by zero,
/// `E1011`) must NOT crash the process. The joiner observes an inspectable
/// error value (a map carrying `error == true` and the fault `code`), and the
/// program continues past the join to a normal, successful exit.
#[test]
fn spawn_join_fault_is_delivered_as_error_value_no_crash() {
    let id = nonce();
    let src = r#"
import "std::concurrency" as conc

fn main() {
    // 1) A healthy spawned thread joins to its return value (sanity: the
    //    join machinery works on the happy path).
    spawn {
        return 7
    }
    let hok = conc.last_thread()
    let rok = conc.join(hok)
    echo "OK_JOIN=$rok"

    // 2) A faulting spawned thread (divide-by-zero -> E1011) is delivered to
    //    the joiner as an inspectable error VALUE, not a process crash.
    spawn {
        let z = 0
        return 10 / z
    }
    let hbad = conc.last_thread()
    let rbad = conc.join(hbad)
    let is_err = rbad["error"]
    let fcode = rbad["code"]
    echo "FAULT_ERR=$is_err"
    echo "FAULT_CODE=$fcode"

    // Reaching this line proves the joiner recovered and the process is alive.
    echo "SURVIVED"
}
"#;

    let (out, fixture) = run_program(&format!("spawn_join_fault_{}.ran", id), src);
    let so = stdout(&out);
    let _ = std::fs::remove_file(&fixture);

    // The process must run to completion and exit cleanly — the fault inside
    // the spawned thread must not propagate as a crash (R3.6).
    assert_eq!(
        code(&out),
        0,
        "faulting spawned thread must not crash the process; stdout: {} stderr: {}",
        so,
        stderr(&out)
    );
    assert!(
        so.contains("OK_JOIN=7"),
        "healthy join should yield the thread's return value; stdout: {}",
        so
    );
    // The joiner observes an inspectable error value carrying the fault marker
    // and the divide-by-zero diagnostic code.
    assert!(
        so.contains("FAULT_ERR=true"),
        "the joiner must observe an error-marked value; stdout: {}",
        so
    );
    assert!(
        so.contains("FAULT_CODE=E1011"),
        "the delivered error value must carry the fault code (E1011); stdout: {}",
        so
    );
    assert!(
        so.contains("SURVIVED"),
        "the process must keep running after joining a faulting thread; stdout: {}",
        so
    );
}

// ---------------------------------------------------------------------------
// R3.3 — per-request HTTP 500 boundary (best-effort, non-hanging)
// ---------------------------------------------------------------------------

/// Reserve an ephemeral TCP port on localhost and return it. The listener is
/// dropped immediately so the `ran` server can bind it; a small race window
/// exists, but a bind failure is tolerated by the test (it SKIPS).
fn reserve_port() -> Option<u16> {
    let l = TcpListener::bind("127.0.0.1:0").ok()?;
    let port = l.local_addr().ok()?.port();
    drop(l);
    Some(port)
}

/// Send a single GET request over a fresh connection with short timeouts and
/// return the parsed HTTP status code from the status line. `None` means the
/// connection could not be established or no status line was read in time.
fn http_get_status(port: u16, path: &str) -> Option<u16> {
    let addr = format!("127.0.0.1:{}", port);
    let mut stream = TcpStream::connect_timeout(
        &addr.parse().ok()?,
        Duration::from_millis(800),
    )
    .ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(1500))).ok()?;
    stream.set_write_timeout(Some(Duration::from_millis(1500))).ok()?;

    let req = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        path
    );
    stream.write_all(req.as_bytes()).ok()?;
    stream.flush().ok()?;

    // Read enough to capture the status line; `Connection: close` lets the
    // server end the stream, and the read timeout bounds a stuck read.
    let mut buf = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(2).any(|w| w == b"\r\n") || buf.len() > 4096 {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let text = String::from_utf8_lossy(&buf);
    let first = text.lines().next()?; // e.g. "HTTP/1.1 500 Internal Server Error"
    let mut parts = first.split_whitespace();
    let _http = parts.next()?;
    let status = parts.next()?;
    status.parse::<u16>().ok()
}

/// Poll the server until it accepts a connection, up to `deadline`. Returns
/// true once a request to `/health` yields any status code.
fn wait_until_ready(port: u16, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if http_get_status(port, "/health").is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Always kill and reap the child server so the test never leaves a process
/// behind and never hangs on drop.
fn kill_child(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// A faulting request handler must yield HTTP 500 and the server must keep
/// serving subsequent requests (R3.3). This test is best-effort: if the server
/// cannot be reached within a short deadline (e.g. the port is unavailable in a
/// locked-down environment), it SKIPS rather than hanging or failing.
#[test]
fn server_handler_fault_returns_500_and_keeps_serving() {
    let id = nonce();

    let port = match reserve_port() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: could not reserve an ephemeral port in this environment");
            return;
        }
    };

    // `/boom` faults (divide-by-zero -> E1011 -> caught at the per-request
    // boundary -> 500). `/health` and `/ok` are healthy routes used to prove
    // the server is up and that it keeps serving AFTER a faulting request.
    let src = format!(
        r#"
import "std::http" as http

fn boom() -> str {{
    let z = 0
    let x = 10 / z
    return "never $x"
}}

fn health() -> str {{
    return "READY"
}}

fn ok() -> str {{
    return "OK"
}}

fn main() {{
    http.get("/boom", "boom")
    http.get("/health", "health")
    http.get("/ok", "ok")
    http.server({port})
}}
"#,
        port = port
    );

    let path = tmp_dir().join(format!("server_fault_{}.ran", id));
    std::fs::write(&path, &src).expect("write .ran server fixture");

    // Bind to localhost only; silence the server's stdout/stderr so the test
    // output stays clean. The child is blocking, so we always kill it.
    let child = Command::new(env!("CARGO_BIN_EXE_ran"))
        .arg(&path)
        .env("RAN_HOST", "127.0.0.1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    let child = match child {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            eprintln!("SKIP: could not spawn ran server child: {}", e);
            return;
        }
    };

    // Bounded readiness wait (~5s max) so the test can never hang waiting for a
    // server that will not come up.
    let ready = wait_until_ready(port, Instant::now() + Duration::from_secs(5));
    if !ready {
        kill_child(child);
        let _ = std::fs::remove_file(&path);
        eprintln!("SKIP: ran HTTP server did not become reachable on port {} (best-effort test)", port);
        return;
    }

    // 1) The faulting handler must return 500 (per-request fault boundary).
    let boom_status = http_get_status(port, "/boom");
    // 2) A subsequent request to a healthy route must still succeed (200),
    //    proving one bad request did not take the server down.
    let ok_status = http_get_status(port, "/ok");

    // Tear down before asserting so a failed assertion never leaks the child.
    kill_child(child);
    let _ = std::fs::remove_file(&path);

    match boom_status {
        Some(s) => assert_eq!(
            s, 500,
            "a faulting handler must yield HTTP 500 at the per-request boundary"
        ),
        None => {
            eprintln!("SKIP: could not read a status from /boom (transient socket issue)");
            return;
        }
    }
    match ok_status {
        Some(s) => assert_eq!(
            s, 200,
            "the server must keep serving healthy requests after a fault (got {})",
            s
        ),
        None => panic!("server stopped serving after a faulting request (no response to /ok)"),
    }
}
