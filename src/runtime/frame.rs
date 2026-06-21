//! The scope/frame engine: indexed local frames + shared globals, variable
//! access, and the call-stack frame management (no per-call environment clone).
//! This is the heart of the slot/frame variable model.

use super::*;

/// RAII guard for the Ran function-call depth counter (Recursion_Guard, R1.7).
///
/// `enter()` increments the thread-local `CALL_DEPTH`; `Drop` decrements it.
/// Because `Drop` runs on *both* a normal return and an unwind, the tracked
/// depth is always restored to its pre-call value even when the body raises a
/// `RuntimeFault` (panic-as-unwind) — no call site has to remember to undo the
/// increment. The guard must be bound to a named local (`_guard`) *before* the
/// over-limit check so that the `E1007` unwind passes back through this `Drop`.
struct DepthGuard;

impl DepthGuard {
    /// Increment the call depth and return the live guard. Always pair with a
    /// named binding so the matching decrement happens on scope exit/unwind.
    fn enter() -> DepthGuard {
        CALL_DEPTH.with(|d| d.set(d.get() + 1));
        DepthGuard
    }

    /// Current tracked call depth on this thread.
    fn depth() -> usize {
        CALL_DEPTH.with(|d| d.get())
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        // `saturating_sub` keeps the counter sane even on an unexpected path.
        CALL_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// Enforce the Recursion_Guard at the single function-call boundary (R1.2).
///
/// Returns a live `DepthGuard` whose `Drop` restores the depth (R1.7). When the
/// post-increment depth exceeds the effective limit, raises a recoverable
/// `E1007` fault *before* the next frame is allocated. The guard is already
/// bound here, so the unwind decrements the counter on the way out.
#[inline]
fn enter_call_frame() -> DepthGuard {
    let guard = DepthGuard::enter();
    if DepthGuard::depth() > current_max_depth() {
        // `guard` is bound, so this unwind runs `DepthGuard::drop` → depth is
        // restored before reaching the catch boundary (R1.7).
        runtime_error(
            "E1007",
            "recursion too deep: call-depth limit exceeded",
            "this usually means unbounded recursion (a base case is never reached); \
             reduce the recursion depth or raise the limit with `--max-depth=<N>`",
        );
    }
    guard
}

impl Environment {
    pub(crate) fn scope_push(&mut self) {
        self.frames.push(Vec::new());
    }

    pub(crate) fn scope_pop(&mut self) {
        // Pop the innermost block frame. Never pop below the current function's
        // base (into the caller's frames); the parameter frame at `frame_base`
        // is released by `run_function_frame` on return. Dropping the frame
        // releases every `Value` it owns exactly once (memory-model note above).
        if self.frames.len() > self.frame_base {
            self.frames.pop();
        }
    }

    /// If `expr` is a direct call to the built-in `range(...)`, evaluate its
    /// bounds and return `(start, end)` so a `for` loop can iterate numerically
    /// without materializing an array. Returns `None` for any other iterable.
    pub(crate) fn as_range_bounds(&mut self, expr: &Expression) -> Option<(i64, i64)> {
        if let Expression::FnCall { callee, args } = expr {
            if let Expression::Variable(name) = callee.as_ref() {
                if name == "range" {
                    let (start, end) = if args.len() >= 2 {
                        (self.eval_arg_int(args, 0, 0), self.eval_arg_int(args, 1, 0))
                    } else {
                        (0, self.eval_arg_int(args, 0, 0))
                    };
                    return Some((start, end));
                }
            }
        }
        None
    }

    /// Runtime memory guard (checked periodically inside loops). Probes system
    /// memory roughly every ~1M iterations and, if free memory falls below a
    /// safety floor, raises a recoverable fault so the process stops *itself*
    /// with a clear diagnostic instead of being OOM-killed by the OS. Probing
    /// is amortized by the stride so the per-iteration cost is a single bitmask.
    pub(crate) fn memory_guard_tick(&self, count: u64) {
        const STRIDE_MASK: u64 = (1 << 20) - 1; // ~every 1,048,576 iterations
        // Skip iteration 0 so short (including deeply nested) loops never probe;
        // only loops that actually run past the stride pay the probe cost.
        if count == 0 || count & STRIDE_MASK != 0 {
            return;
        }
        let avail = crate::support::sysinfo::mem_available();
        if avail == 0 {
            return; // probing unsupported/failed: never interfere with a run
        }
        let total = crate::support::sysinfo::mem_total();
        // Keep at least max(total/32, 128 MiB) free; abort before the OS does.
        let floor = (total / 32).max(128 * 1024 * 1024);
        if avail < floor {
            runtime_error(
                "E1006",
                &format!(
                    "out of memory: only {} free, below the {} safety floor",
                    crate::support::sysinfo::human_bytes(avail),
                    crate::support::sysinfo::human_bytes(floor),
                ),
                "reduce memory held across the loop (e.g. avoid building very large arrays); \
                 the process stopped itself before the system OOM-killer would",
            );
        }
    }

    /// Look up a variable: walk the current function's frames innermost ->
    /// outermost, then fall back to globals.
    pub(crate) fn var_get(&self, name: &str) -> Option<Value> {
        for frame in self.frames[self.frame_base..].iter().rev() {
            for (k, v) in frame.iter() {
                if k == name {
                    return Some(v.clone());
                }
            }
        }
        self.globals.get(name).cloned()
    }

    /// Mutable borrow of a variable's slot (frames innermost -> outermost, then
    /// globals).
    pub(crate) fn var_get_mut(&mut self, name: &str) -> Option<&mut Value> {
        let base = self.frame_base;
        for frame in self.frames[base..].iter_mut().rev() {
            for (k, v) in frame.iter_mut() {
                if k == name {
                    return Some(v);
                }
            }
        }
        self.globals.get_mut(name)
    }

    pub(crate) fn var_exists(&self, name: &str) -> bool {
        self.frames[self.frame_base..]
            .iter()
            .any(|f| f.iter().any(|(k, _)| k == name))
            || self.globals.contains_key(name)
    }

    /// Declare-or-assign: if the name exists in a visible frame or in globals,
    /// update it there (assignment semantics, e.g. `count = count + 1`);
    /// otherwise declare it in the innermost frame (or globals at top level).
    /// Updating in place avoids a `String` allocation on every assignment — the
    /// dominant per-iteration cost in tight loops like `total = total + i`.
    pub(crate) fn var_set(&mut self, name: &str, value: Value) {
        let base = self.frame_base;
        for frame in self.frames[base..].iter_mut().rev() {
            for (k, v) in frame.iter_mut() {
                if k == name {
                    *v = value;
                    return;
                }
            }
        }
        if let Some(slot) = self.globals.get_mut(name) {
            *slot = value;
            return;
        }
        // New binding: innermost frame if the current function has one, else a
        // global (top-level code with no active local frame).
        if self.frames.len() > self.frame_base {
            self.frames.last_mut().unwrap().push((name.to_string(), value));
        } else {
            self.globals.insert(name.to_string(), value);
        }
    }

    /// Force a binding into the innermost frame (params, loop var, match
    /// binding). Always shadows. When there is no active local frame (e.g. the
    /// pre-call request-variable injection for an HTTP handler), targets globals
    /// so the about-to-run handler frame can see it. Updates in place when the
    /// key already exists, avoiding a `String` alloc on loop-variable reuse.
    pub(crate) fn var_set_local(&mut self, name: &str, value: Value) {
        if self.frames.len() > self.frame_base {
            let top = self.frames.last_mut().unwrap();
            for (k, v) in top.iter_mut() {
                if k == name {
                    *v = value;
                    return;
                }
            }
            top.push((name.to_string(), value));
        } else if let Some(slot) = self.globals.get_mut(name) {
            *slot = value;
        } else {
            self.globals.insert(name.to_string(), value);
        }
    }

    /// Flatten everything visible (globals + the current function's frames) into
    /// a single map. Used to snapshot state for `spawn` and HTTP handler
    /// environments, which then carry it as their globals.
    pub(crate) fn flatten_scopes(&self) -> Scope {
        let mut out = self.globals.clone();
        for frame in &self.frames[self.frame_base..] {
            for (k, v) in frame {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }

    /// Run `body` in a fresh function frame: the callee sees globals and its own
    /// parameters/locals, never the caller's locals. No whole-environment clone
    /// per call — we just move the frame base and push one parameter frame.
    pub(crate) fn run_function_frame(&mut self, body: &[Stmt], params: Vec<(String, Value)>) -> Value {
        // Recursion_Guard (R1.2/R1.7): count this call and raise E1007 before the
        // next frame is allocated when the depth limit is exceeded. `_depth_guard`
        // restores the counter on normal return *and* on fault unwind.
        let _depth_guard = enter_call_frame();
        self.base_stack.push(self.frame_base);
        self.frame_base = self.frames.len();
        self.frames.push(Vec::new()); // parameter frame
        for (n, v) in params {
            self.var_set_local(&n, v);
        }
        let ret = self.exec_block_with_return(body);
        self.frames.truncate(self.frame_base);
        self.frame_base = self.base_stack.pop().unwrap_or(0);
        ret
    }

    /// Like `run_function_frame`, but additionally captures the final value of
    /// each parameter named in `capture` *before* the callee frame is released.
    /// Used to implement `&mut` write-back (R11.6).
    pub(crate) fn run_function_frame_capture(
        &mut self,
        body: &[Stmt],
        params: Vec<(String, Value)>,
        capture: &[String],
    ) -> (Value, Vec<(String, Value)>) {
        // Recursion_Guard (R1.2/R1.7): same single-boundary enforcement as
        // `run_function_frame` — this is the `&mut` write-back call path.
        let _depth_guard = enter_call_frame();
        self.base_stack.push(self.frame_base);
        self.frame_base = self.frames.len();
        self.frames.push(Vec::new()); // parameter frame
        for (n, v) in params {
            self.var_set_local(&n, v);
        }
        let ret = self.exec_block_with_return(body);
        // Read the (possibly mutated) final parameter values from the callee
        // frame BEFORE releasing it.
        let finals: Vec<(String, Value)> = capture
            .iter()
            .filter_map(|n| self.var_get(n).map(|v| (n.clone(), v)))
            .collect();
        self.frames.truncate(self.frame_base);
        self.frame_base = self.base_stack.pop().unwrap_or(0);
        (ret, finals)
    }
}

#[cfg(test)]
pub(crate) mod recursion_guard_tests {
    use super::*;

    /// Serializes every test that mutates the process-global `MAX_CALL_DEPTH`.
    ///
    /// `MAX_CALL_DEPTH` is a single process-wide `AtomicUsize`, but `cargo test`
    /// runs tests concurrently across threads. Without serialization, one test
    /// lowering the limit could make another test's "bounded" recursion fault
    /// spuriously. Both this module and the Property 1 module
    /// (`recursion_guard_property`) acquire this lock for their save/restore
    /// window. Poison is recovered (`into_inner`) so one failing test does not
    /// cascade lock-poison failures into the others.
    pub(crate) static DEPTH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Register the free functions in `src` into a fresh `Environment`, mirroring
    /// what `execute`/`run_tests` do, so a recursive function can be invoked.
    fn prep_funcs(src: &str) -> Environment {
        let tokens = crate::frontend::lexer::tokenize(src);
        let program = crate::frontend::parser::parse(tokens);
        let mut env = Environment::new();
        for stmt in &program.statements {
            if let Statement::FnDecl { name, params, body, .. } = &stmt.kind {
                env.functions.insert(name.clone(), std::sync::Arc::new(body.clone()));
                env.fn_params
                    .insert(name.clone(), params.iter().map(|p| p.name.clone()).collect());
            }
        }
        env
    }

    /// R1.2/R1.6/R1.7: unbounded recursion must raise a *catchable* `E1007`
    /// (not a SIGSEGV), the diagnostic must carry a help hint, and the tracked
    /// call depth must be restored after the fault unwinds back through the
    /// `DepthGuard` drops. A bounded call sequence under the same limit must
    /// return normally and also leave the counter at 0.
    ///
    /// Both checks live in one test so they share a single save/restore window
    /// for the process-global `MAX_CALL_DEPTH` (avoids cross-test contention).
    #[test]
    fn recursion_guard_bounds_depth_and_restores_counter() {
        let _serialize = DEPTH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = current_max_depth();
        set_max_call_depth(64); // small, well below any OS stack concern
        CALL_DEPTH.with(|d| d.set(0));

        // (1) Unbounded recursion: no base case — would overflow the OS stack
        //     without a guard. Must surface a catchable E1007 instead.
        let mut env = prep_funcs("fn boom(n: int) -> int { return boom(n + 1) }");
        let arg = [Expression::IntLiteral(0)];
        let outcome = catch_fault(std::panic::AssertUnwindSafe(|| {
            env.call_function("boom", &arg);
        }));
        match outcome {
            Err(fault) => {
                assert_eq!(fault.code, "E1007", "expected recursion-guard code");
                assert!(!fault.help.is_empty(), "E1007 must carry a help hint (R1.6)");
            }
            Ok(_) => panic!("expected E1007 fault, recursion was not bounded"),
        }
        // R1.7: depth restored on the fault-unwind path.
        assert_eq!(CALL_DEPTH.with(|d| d.get()), 0, "depth must be restored after a fault");

        // (2) Bounded recursion (depth 30 < limit 64): returns normally and the
        //     counter is restored on the normal-return path.
        let mut env = prep_funcs(
            "fn countdown(n: int) -> int { if n <= 0 { return 0 } return countdown(n - 1) }",
        );
        let arg = [Expression::IntLiteral(30)];
        let outcome = catch_fault(std::panic::AssertUnwindSafe(|| {
            env.call_function("countdown", &arg);
        }));

        set_max_call_depth(saved);

        assert!(outcome.is_ok(), "bounded recursion should not fault");
        assert_eq!(CALL_DEPTH.with(|d| d.get()), 0, "depth must return to 0 after normal return");
    }
}

// ============================================================================
// Property 1 — Recursion guard never SIGSEGVs (R1.2, R1.3, R1.4).
// ============================================================================
#[cfg(test)]
mod recursion_guard_property {
    // Feature: memory-safe-self-hosting, Property 1: Recursion guard never SIGSEGVs
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};

    /// One generated scenario: an effective call-depth limit `N >= 1`.
    ///
    /// `N` is kept small (32..=256) so the test is fast and stays far below the
    /// OS thread-stack ceiling — the point is to prove the *guard* fires (a
    /// catchable `E1007`) long before any real stack-overflow SIGSEGV could
    /// occur, for an arbitrary configured limit (covers the `--max-depth=<N>`
    /// path, R1.4, and exercises non-default limits around the R1.3 default).
    #[derive(Clone, Debug)]
    struct Case {
        limit: usize,
    }

    fn case_gen() -> Gen<Case> {
        Gen::new(
            |rng: &mut Rng, _size| Case {
                // [32, 256]: N >= 1 always holds; small for speed.
                limit: rng.range_i64(32, 256) as usize,
            },
            // Shrink toward the smallest still-valid limit (>= 1), halving down.
            |c: &Case| {
                if c.limit <= 1 {
                    return Vec::new();
                }
                let mut out = vec![Case { limit: 1 }];
                let half = c.limit / 2;
                if half >= 1 && half != c.limit {
                    out.push(Case { limit: half });
                }
                let down = c.limit - 1;
                if down >= 1 && !out.iter().any(|x| x.limit == down) {
                    out.push(Case { limit: down });
                }
                out
            },
        )
    }

    /// Register the free functions in `src` into a fresh `Environment`, mirroring
    /// what `execute`/`run_tests` do, so a recursive function can be invoked.
    fn prep_funcs(src: &str) -> Environment {
        let tokens = crate::frontend::lexer::tokenize(src);
        let program = crate::frontend::parser::parse(tokens);
        let mut env = Environment::new();
        for stmt in &program.statements {
            if let Statement::FnDecl { name, params, body, .. } = &stmt.kind {
                env.functions.insert(name.clone(), std::sync::Arc::new(body.clone()));
                env.fn_params
                    .insert(name.clone(), params.iter().map(|p| p.name.clone()).collect());
            }
        }
        env
    }

    /// Run one case: configure the effective limit `N`, then assert (1) an
    /// unbounded-recursion program raises a catchable `E1007` with the depth
    /// counter restored, and (2) a recursion strictly under `N` returns
    /// normally with the counter restored. Returns `true` iff all hold.
    fn check_case(case: &Case) -> bool {
        let n = case.limit;
        set_max_call_depth(n);

        // (1) Unbounded recursion: a synthetic self-call with no base case.
        //     Must surface a catchable E1007 — if the guard were missing the
        //     process would SIGSEGV (uncatchable) instead of returning here.
        CALL_DEPTH.with(|d| d.set(0));
        let mut env = prep_funcs("fn boom(n: int) -> int { return boom(n + 1) }");
        let arg = [Expression::IntLiteral(0)];
        let unbounded = catch_fault(std::panic::AssertUnwindSafe(|| {
            env.call_function("boom", &arg);
        }));
        let unbounded_ok = match unbounded {
            // Reaching this arm at all proves the process stayed alive.
            Err(fault) => fault.code == "E1007",
            Ok(_) => false, // no fault means recursion was not bounded
        };
        // R1.7 sanity: depth restored to 0 after the fault unwinds.
        let depth_restored_after_fault = CALL_DEPTH.with(|d| d.get()) == 0;

        // (2) Bounded recursion strictly under the limit returns normally.
        //     Max depth reached for countdown(k) is k+1, so pick k < N.
        let safe = (n / 2).max(1);
        CALL_DEPTH.with(|d| d.set(0));
        let mut env = prep_funcs(
            "fn countdown(n: int) -> int { if n <= 0 { return 0 } return countdown(n - 1) }",
        );
        let arg = [Expression::IntLiteral(safe as i64)];
        let bounded = catch_fault(std::panic::AssertUnwindSafe(|| {
            env.call_function("countdown", &arg);
        }));
        let bounded_ok = bounded.is_ok();
        let depth_restored_after_return = CALL_DEPTH.with(|d| d.get()) == 0;

        unbounded_ok && depth_restored_after_fault && bounded_ok && depth_restored_after_return
    }

    /// Property 1: for any effective depth limit `N >= 1`, executing an
    /// unbounded-recursion Ran program raises a *catchable* `RuntimeFault`
    /// `E1007` (`catch_fault` returns `Err` with that code) and the process
    /// stays alive — never an uncatchable SIGSEGV. Dually, a recursion that
    /// stays strictly under `N` returns normally.
    ///
    /// The property is run on a worker thread configured with a large explicit
    /// stack (mirroring `main.rs`'s 1 GiB `EXEC_STACK_BYTES`), because that is
    /// exactly the runtime contract that makes the guarantee hold: the depth
    /// guard (`E1007`) must fire *before* the OS stack is exhausted. A default
    /// `cargo test` worker thread (~2 MiB) cannot hold a few hundred
    /// tree-walking frames, so without the realistic stack the test would hit a
    /// genuine (uncatchable) overflow instead of exercising the guard.
    ///
    /// Validates: Requirements 1.2, 1.3, 1.4
    #[test]
    fn prop_recursion_guard_never_sigsegvs() {
        // Serialize with the other depth-mutating test: `MAX_CALL_DEPTH` is a
        // single process-global, while the bounded check relies on the limit it
        // just set. Hold the lock across the worker thread's whole run.
        let _serialize = super::recursion_guard_tests::DEPTH_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved = current_max_depth();

        // 1 GiB stack, matching the runtime, so E1007 fires well before the
        // stack is exhausted for any N in the generated range.
        const PROP_STACK_BYTES: usize = 1024 * 1024 * 1024;
        let handle = std::thread::Builder::new()
            .stack_size(PROP_STACK_BYTES)
            .spawn(|| {
                pbt::for_all(
                    "P1 recursion guard never SIGSEGVs",
                    &case_gen(),
                    check_case,
                );
            })
            .expect("spawn recursion-guard property worker");
        let result = handle.join();

        set_max_call_depth(saved);

        // Propagate a property failure (panic) from the worker as a test failure.
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
