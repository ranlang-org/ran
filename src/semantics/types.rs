//! Type system & memory safety module.
//! Implements Rust-style ownership, borrowing, and lifetime analysis.
//!
//! This module hosts the *analysis model* used by the ownership checker
//! (see `analyzer.rs`). It deliberately contains only data structures and
//! queries — the actual diagnostic emission / abort behaviour lives in the
//! analyzer. Keeping the model here lets the analyzer stay focused on walking
//! the AST while this file owns the rules for move-state, borrow sets, and the
//! Copy/non-Copy classification.

use std::collections::{HashMap, HashSet};

use crate::frontend::ast::*;

/// Ownership state of a variable.
///
/// `Owned` is the initial state. A binding becomes `Borrowed` while one or more
/// shared (`&`) borrows are outstanding, `MutBorrowed` while a single exclusive
/// (`&mut`) borrow is outstanding, and `Moved` once its value has been moved
/// out (only meaningful for non-`Copy` types).
#[derive(Debug, Clone, PartialEq)]
pub enum OwnershipState {
    Owned,
    Borrowed,
    MutBorrowed,
    Moved,
}

/// The kind of borrow taken against a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorrowKind {
    /// Shared, immutable borrow (`&`). Multiple may coexist.
    Shared,
    /// Exclusive, mutable borrow (`&mut`). At most one, and not alongside `&`.
    Mut,
}

/// Result of asking whether a borrow can be taken against a binding.
///
/// The analyzer turns these into diagnostics (e.g. a blocked `&mut` becomes
/// `E0212`). The model only reports the conflict; it never decides severity.
#[derive(Debug, Clone, PartialEq)]
pub enum BorrowConflict {
    /// The borrow is allowed.
    None,
    /// An exclusive (`&mut`) borrow is already active at the given location;
    /// blocks any further `&` or `&mut`.
    ExclusiveActive(Span),
    /// `count` shared (`&`) borrows are active; blocks a new `&mut`.
    SharedActive(usize),
}

/// The set of borrows currently outstanding against a single binding.
///
/// Rules enforced here mirror Rust's borrow discipline:
/// - any number of shared borrows may coexist (R10.3);
/// - a single exclusive borrow may exist only when there are no other borrows
///   (R10.1);
/// - releasing all borrows re-enables a fresh `&mut` (R10.5).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BorrowSet {
    /// Number of active shared (`&`) borrows.
    shared: usize,
    /// Location of the single active exclusive (`&mut`) borrow, if any.
    exclusive: Option<Span>,
}

impl BorrowSet {
    pub fn new() -> Self {
        Self {
            shared: 0,
            exclusive: None,
        }
    }

    /// Number of active shared borrows.
    pub fn shared_count(&self) -> usize {
        self.shared
    }

    /// Whether any shared borrow is active.
    pub fn has_shared(&self) -> bool {
        self.shared > 0
    }

    /// Whether an exclusive (`&mut`) borrow is active.
    pub fn has_exclusive(&self) -> bool {
        self.exclusive.is_some()
    }

    /// Location of the active exclusive borrow, if any.
    pub fn exclusive_location(&self) -> Option<Span> {
        self.exclusive
    }

    /// Location of *some* active borrow, preferring the exclusive one. Shared
    /// borrows do not record individual spans, so this returns `None` when only
    /// shared borrows are outstanding. Used to enrich the `E0215`
    /// move-while-borrowed hint with the conflicting borrow's location.
    pub fn active_location(&self) -> Option<Span> {
        self.exclusive
    }

    /// Whether there are no outstanding borrows at all.
    pub fn is_empty(&self) -> bool {
        self.shared == 0 && self.exclusive.is_none()
    }

    /// Query (without mutating) whether a borrow of `kind` could be taken.
    ///
    /// - `Shared` is blocked only by an active exclusive borrow (R10.1).
    /// - `Mut` is blocked by any active borrow, shared or exclusive
    ///   (R10.1/R10.4).
    pub fn conflict(&self, kind: BorrowKind) -> BorrowConflict {
        match kind {
            BorrowKind::Shared => match self.exclusive {
                Some(sp) => BorrowConflict::ExclusiveActive(sp),
                None => BorrowConflict::None,
            },
            BorrowKind::Mut => {
                if let Some(sp) = self.exclusive {
                    BorrowConflict::ExclusiveActive(sp)
                } else if self.shared > 0 {
                    BorrowConflict::SharedActive(self.shared)
                } else {
                    BorrowConflict::None
                }
            }
        }
    }

    /// Try to record a borrow of `kind` at `at`.
    ///
    /// Returns `Ok(())` if the borrow was recorded, or `Err(conflict)`
    /// describing why it could not be (leaving the set unchanged).
    pub fn record(&mut self, kind: BorrowKind, at: Span) -> Result<(), BorrowConflict> {
        match self.conflict(kind) {
            BorrowConflict::None => {
                match kind {
                    BorrowKind::Shared => self.shared += 1,
                    BorrowKind::Mut => self.exclusive = Some(at),
                }
                Ok(())
            }
            conflict => Err(conflict),
        }
    }

    /// Release one borrow of `kind`. Releasing a shared borrow decrements the
    /// count (saturating at zero); releasing an exclusive borrow clears it.
    /// Once the set is empty again, a fresh `&mut` is permitted (R10.5).
    pub fn release(&mut self, kind: BorrowKind) {
        match kind {
            BorrowKind::Shared => self.shared = self.shared.saturating_sub(1),
            BorrowKind::Mut => self.exclusive = None,
        }
    }

    /// Release every outstanding borrow (e.g. when the referent's scope ends).
    pub fn release_all(&mut self) {
        self.shared = 0;
        self.exclusive = None;
    }
}

/// Type information
#[derive(Debug, Clone, PartialEq)]
pub enum RanType {
    Int,
    Float,
    Str,
    Bool,
    Void,
    Array(Box<RanType>),
    Channel(Box<RanType>),
    Function {
        params: Vec<RanType>,
        return_type: Box<RanType>,
    },
    Struct {
        name: String,
        fields: HashMap<String, RanType>,
    },
    Ref(Box<RanType>),
    MutRef(Box<RanType>),
    Unknown,
}

impl RanType {
    /// Whether values of this type have *copy* semantics.
    ///
    /// Per the design's move-tracking notes, the scalar types `int`, `float`
    /// and `bool` are `Copy` (assigning/passing them duplicates the value), as
    /// is `void`. Everything else — `str`, `array`, `map`/`object`, `channel`
    /// and opaque `handle`s — is non-`Copy` and therefore subject to move
    /// tracking. References themselves are not owned values and are treated as
    /// `Copy` for the purpose of move analysis. `Unknown` is conservatively
    /// treated as non-`Copy` so the checker does not silently skip a move.
    pub fn is_copy(&self) -> bool {
        match self {
            RanType::Int | RanType::Float | RanType::Bool | RanType::Void => true,
            RanType::Ref(_) | RanType::MutRef(_) => true,
            RanType::Str
            | RanType::Array(_)
            | RanType::Channel(_)
            | RanType::Function { .. }
            | RanType::Struct { .. }
            | RanType::Unknown => false,
        }
    }
}

/// Classify whether a *type name* (as written in Ran source / used by the
/// runtime) has copy semantics.
///
/// This complements [`RanType::is_copy`] for callers that only have a textual
/// type/value tag (for example the runtime `Value` discriminant) rather than a
/// fully-resolved [`RanType`]. Copy: `int`, `float`, `bool`, `void`. Non-Copy:
/// `str`/`string`, `array`, `map`, `object`, `channel`, `handle`. Unknown names
/// are treated as non-`Copy` (conservative).
pub fn is_copy_type_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "int" | "integer" | "float" | "double" | "bool" | "boolean" | "void" | "unit"
    )
}

/// Built-in functions that read (rather than consume) their arguments. Passing
/// a bare variable to one of these does *not* move it. Mirrors the analyzer's
/// `builtin_functions` set so move analysis and name resolution stay aligned.
fn is_readonly_builtin(name: &str) -> bool {
    matches!(
        name,
        "echo"
            | "print"
            | "println"
            | "len"
            | "typeof"
            | "str"
            | "int"
            | "float"
            | "push"
            | "map"
            | "set"
            | "get"
            | "exit"
            | "range"
            | "keys"
            | "values"
            | "abs"
            | "assert"
            | "bool"
            | "dec"
    )
}

/// Map a written [`TypeExpr`] to the analysis [`RanType`]. Only the cases that
/// matter for Copy/non-Copy classification (and thus move tracking) are
/// resolved precisely; everything else falls back to `Unknown` (conservatively
/// non-`Copy`). References (`&T`/`&mut T`) are themselves Copy.
fn type_expr_to_ran(ty: &TypeExpr) -> RanType {
    match ty {
        TypeExpr::Named(n) => match n.trim().to_ascii_lowercase().as_str() {
            "int" | "i64" | "i32" | "integer" => RanType::Int,
            "float" | "f64" | "f32" | "double" => RanType::Float,
            "bool" | "boolean" => RanType::Bool,
            "void" | "unit" => RanType::Void,
            "str" | "string" => RanType::Str,
            _ => RanType::Unknown,
        },
        TypeExpr::Ref { mutable, inner } => {
            let inner_ty = Box::new(type_expr_to_ran(inner));
            if *mutable {
                RanType::MutRef(inner_ty)
            } else {
                RanType::Ref(inner_ty)
            }
        }
        TypeExpr::Array(inner) => RanType::Array(Box::new(type_expr_to_ran(inner))),
        TypeExpr::Channel(inner) => RanType::Channel(Box::new(type_expr_to_ran(inner))),
        _ => RanType::Unknown,
    }
}

/// Variable binding with ownership tracking.
///
/// In addition to the resolved type and mutability, a binding carries its
/// current [`OwnershipState`], the source location it was moved from (if it has
/// been moved), and the set of borrows currently outstanding against it.
#[derive(Debug, Clone)]
pub struct Binding {
    pub name: String,
    pub ran_type: RanType,
    pub mutable: bool,
    /// True only for a binding introduced by an immutable `let x = …`
    /// declaration. Distinguishes it from other immutable-`mutable=false`
    /// bindings (parameters, `for` variables, `match` bindings) so that
    /// reassigning ONLY a `let` binding is the immutability error (E0100);
    /// `let mut`, `var`, bare `x = …`, params, and loop/match bindings are all
    /// freely assignable.
    pub let_locked: bool,
    pub ownership: OwnershipState,
    /// Where the value was moved from. `Some(span)` iff `ownership == Moved`.
    pub moved_at: Option<Span>,
    /// Borrows currently outstanding against this binding.
    pub borrows: BorrowSet,
}

impl Binding {
    /// Create a freshly-owned binding with no borrows.
    pub fn owned(name: String, ran_type: RanType, mutable: bool) -> Self {
        Self {
            name,
            ran_type,
            mutable,
            let_locked: false,
            ownership: OwnershipState::Owned,
            moved_at: None,
            borrows: BorrowSet::new(),
        }
    }

    /// An immutable `let` binding (reassignment is E0100). Like [`owned`] with
    /// `mutable = false`, but flagged so the analyzer can tell it apart from a
    /// parameter / loop / match binding (which are also `mutable = false` but
    /// may be reassigned).
    pub fn let_immutable(name: String, ran_type: RanType) -> Self {
        let mut b = Self::owned(name, ran_type, false);
        b.let_locked = true;
        b
    }

    /// Whether this binding is a `Copy`-typed value (not subject to moves).
    pub fn is_copy(&self) -> bool {
        self.ran_type.is_copy()
    }

    /// Whether the value has been moved out.
    pub fn is_moved(&self) -> bool {
        self.ownership == OwnershipState::Moved
    }

    /// The location the value was moved from, if any.
    pub fn move_location(&self) -> Option<Span> {
        self.moved_at
    }

    /// Mark the value as moved out at `at`.
    ///
    /// `Copy` values are never moved, so this is a no-op for them — matching the
    /// design rule that scalars are duplicated rather than moved.
    pub fn mark_moved(&mut self, at: Span) {
        if self.is_copy() {
            return;
        }
        self.ownership = OwnershipState::Moved;
        self.moved_at = Some(at);
    }

    /// Query whether a borrow of `kind` may be taken against this binding.
    pub fn borrow_conflict(&self, kind: BorrowKind) -> BorrowConflict {
        self.borrows.conflict(kind)
    }

    /// Record a borrow of `kind` at `at`, updating the ownership state to
    /// reflect the active borrow. Returns the conflict if the borrow is blocked.
    pub fn record_borrow(&mut self, kind: BorrowKind, at: Span) -> Result<(), BorrowConflict> {
        self.borrows.record(kind, at)?;
        self.ownership = match kind {
            BorrowKind::Shared => OwnershipState::Borrowed,
            BorrowKind::Mut => OwnershipState::MutBorrowed,
        };
        Ok(())
    }

    /// Release one borrow of `kind`. When no borrows remain (and the value has
    /// not been moved), the binding returns to `Owned`, re-enabling a fresh
    /// `&mut` (R10.5).
    pub fn release_borrow(&mut self, kind: BorrowKind) {
        self.borrows.release(kind);
        self.refresh_state_after_release();
    }

    /// Release all outstanding borrows (e.g. the referent scope ended).
    pub fn release_all_borrows(&mut self) {
        self.borrows.release_all();
        self.refresh_state_after_release();
    }

    fn refresh_state_after_release(&mut self) {
        if self.ownership == OwnershipState::Moved {
            return;
        }
        self.ownership = if self.borrows.has_exclusive() {
            OwnershipState::MutBorrowed
        } else if self.borrows.has_shared() {
            OwnershipState::Borrowed
        } else {
            OwnershipState::Owned
        };
    }
}

/// Scope for variable lookup
pub struct Scope {
    bindings: HashMap<String, Binding>,
    parent: Option<Box<Scope>>,
}

impl Scope {
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
            parent: None,
        }
    }

    pub fn child(parent: Scope) -> Self {
        Self {
            bindings: HashMap::new(),
            parent: Some(Box::new(parent)),
        }
    }

    pub fn define(&mut self, binding: Binding) {
        self.bindings.insert(binding.name.clone(), binding);
    }

    pub fn lookup(&self, name: &str) -> Option<&Binding> {
        self.bindings
            .get(name)
            .or_else(|| self.parent.as_ref().and_then(|p| p.lookup(name)))
    }

    /// Whether `name` is defined *directly* in this scope (not a parent). Used
    /// by the dangling-reference check to decide whether `return &x` refers to
    /// a value that lives only for this function body's frame (R10.7 → E0214).
    pub fn is_locally_defined(&self, name: &str) -> bool {
        self.bindings.contains_key(name)
    }

    /// Mutable lookup that walks the parent chain, like [`Scope::lookup`].
    pub fn lookup_mut(&mut self, name: &str) -> Option<&mut Binding> {
        if self.bindings.contains_key(name) {
            return self.bindings.get_mut(name);
        }
        match self.parent {
            Some(ref mut p) => p.lookup_mut(name),
            None => None,
        }
    }

    /// Mark `name` as moved at `at`, searching the parent chain.
    pub fn mark_moved(&mut self, name: &str, at: Span) {
        if let Some(b) = self.lookup_mut(name) {
            b.mark_moved(at);
        }
    }

    /// Query whether a borrow of `kind` may be taken against `name`.
    /// Returns `None` if the binding is unknown.
    pub fn borrow_conflict(&self, name: &str, kind: BorrowKind) -> Option<BorrowConflict> {
        self.lookup(name).map(|b| b.borrow_conflict(kind))
    }

    /// Record a borrow of `kind` against `name`. Returns `None` if the binding
    /// is unknown, otherwise the result of attempting the borrow.
    pub fn record_borrow(
        &mut self,
        name: &str,
        kind: BorrowKind,
        at: Span,
    ) -> Option<Result<(), BorrowConflict>> {
        self.lookup_mut(name).map(|b| b.record_borrow(kind, at))
    }

    /// Release one borrow of `kind` against `name`.
    pub fn release_borrow(&mut self, name: &str, kind: BorrowKind) {
        if let Some(b) = self.lookup_mut(name) {
            b.release_borrow(kind);
        }
    }

    /// Release all borrows against `name` (e.g. the referent scope ended).
    pub fn release_all_borrows(&mut self, name: &str) {
        if let Some(b) = self.lookup_mut(name) {
            b.release_all_borrows();
        }
    }

    /// Collect the names of every binding visible from this scope and its
    /// parents into `out`. Used by the `spawn` data-race check to determine the
    /// set of bindings a spawned thread captures from enclosing scopes (R14.6).
    pub fn collect_visible_names(&self, out: &mut HashSet<String>) {
        for name in self.bindings.keys() {
            out.insert(name.clone());
        }
        if let Some(parent) = &self.parent {
            parent.collect_visible_names(out);
        }
    }
}

/// A coded ownership/borrow finding produced by the checker.
///
/// Unlike the plain-string `errors`/`warnings` lists, a finding carries the
/// diagnostic `code` (`E0210` for use-after-move), the source `span` of the
/// offending use, and an optional refined fix hint (e.g. naming the location
/// the value was moved from). The analyzer turns each finding into a fully
/// rendered [`crate::support::diagnostics::Diagnostic`] (with `error[E0210]`
/// header and `file:line:col`), downgrading the severity to a warning when the
/// `--ownership=warn` migration mode is active.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnershipFinding {
    /// Registered diagnostic code (e.g. `"E0210"`).
    pub code: &'static str,
    /// Source location of the offending use.
    pub span: Span,
    /// Human-readable message describing the finding.
    pub message: String,
    /// Optional refined fix hint; falls back to the catalog hint when `None`.
    pub help: Option<String>,
    /// When true, this finding is ALWAYS an error and is never downgraded to a
    /// warning by `--ownership=warn`. Used for hard semantic errors that happen
    /// to flow through the ownership funnel, e.g. assigning to an immutable
    /// `let` binding (E0100) — choosing `let` is opting into immutability, so a
    /// write is rejected regardless of the ownership migration mode.
    pub always_error: bool,
}

/// Type checker - verifies types and ownership rules
pub struct TypeChecker {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// Coded ownership/borrow findings (use-after-move `E0210`, etc.). The
    /// analyzer renders these into diagnostics carrying the `E####` code and
    /// `file:line:col` location, applying the warn/strict severity routing.
    pub ownership_findings: Vec<OwnershipFinding>,
    /// When true, ownership/borrow findings that would normally be errors are
    /// emitted as warnings instead. This is the controlled error→warning
    /// downgrade path used by the `--ownership=warn` migration mode; the
    /// analyzer drives it (see `analyzer.rs`). Kept as a plain boolean so this
    /// task's model stays independent of the analyzer's `OwnershipMode` enum.
    pub downgrade_ownership_to_warning: bool,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            errors: Vec::new(),
            warnings: Vec::new(),
            ownership_findings: Vec::new(),
            downgrade_ownership_to_warning: false,
        }
    }

    /// Configure whether ownership findings are downgraded to warnings.
    pub fn set_downgrade_to_warning(&mut self, downgrade: bool) {
        self.downgrade_ownership_to_warning = downgrade;
    }

    /// Emit an ownership/borrow finding, honouring the downgrade mode: when
    /// downgrading is enabled the message lands in `warnings`, otherwise in
    /// `errors`. This is the single funnel the analyzer can route ownership
    /// diagnostics through so the warn/strict behaviour stays consistent.
    pub fn emit_ownership(&mut self, message: String) {
        if self.downgrade_ownership_to_warning {
            self.warnings.push(message);
        } else {
            self.errors.push(message);
        }
    }

    /// Emit a *coded* ownership/borrow finding (e.g. use-after-move `E0210`).
    ///
    /// The finding records the diagnostic code, the source location of the
    /// offending use, and an optional refined fix hint. Severity routing (error
    /// in `strict`, warning in `warn`) is applied later by the analyzer when it
    /// renders the finding, keeping a single funnel for ownership diagnostics.
    pub fn emit_ownership_coded(
        &mut self,
        code: &'static str,
        span: Span,
        message: String,
        help: Option<String>,
    ) {
        self.ownership_findings.push(OwnershipFinding {
            code,
            span,
            message,
            help,
            always_error: false,
        });
    }

    /// Emit a coded finding that is ALWAYS an error (never downgraded by
    /// `--ownership=warn`). For hard semantic errors routed through the
    /// ownership funnel, e.g. assigning to an immutable `let` binding (E0100).
    pub fn emit_hard_coded(
        &mut self,
        code: &'static str,
        span: Span,
        message: String,
        help: Option<String>,
    ) {
        self.ownership_findings.push(OwnershipFinding {
            code,
            span,
            message,
            help,
            always_error: true,
        });
    }

    /// Check a program for type/ownership errors
    pub fn check(&mut self, program: &Program) -> bool {
        let mut scope = Scope::new();

        for stmt in &program.statements {
            self.check_statement(stmt, &mut scope);
        }

        self.errors.is_empty()
    }

    fn check_statement(&mut self, stmt: &Stmt, scope: &mut Scope) {
        let sp = stmt.span;
        match &stmt.kind {
            Statement::VarDecl {
                name,
                mutable,
                is_decl,
                value,
                ..
            } => {
                // 1) Detect use of an already-moved binding read inside `value`.
                self.check_expr_ownership(value, scope, sp);
                // 2) Borrow checking: a `let r = &x` / `let r = &mut x` is a
                //    *bound* borrow that stays active for the rest of the scope;
                //    any other borrow inside `value` is *transient* (released at
                //    the end of this statement). Conflicts emit `E0212`.
                self.record_bound_or_transient_borrows(value, scope, sp);
                // 3) Apply moves caused by nested value-passing calls in `value`.
                self.apply_moves(value, scope, sp);
                // 4) `let b = a;` moves a bare non-Copy source binding `a`
                //    (emits `E0215` if `a` is still borrowed).
                if let Expression::Variable(src) = value {
                    self.move_if_non_copy(scope, src, sp);
                }
                // 5) Descend into any closure/match bodies nested in `value` so
                //    real violations inside them are still checked (R8.7).
                self.check_inner_bodies(value, scope, sp);
                let inferred_type = self.infer_type(value, scope);

                if *is_decl {
                    // Explicit declaration: `let`/`let mut`/`var`. Shadows any
                    // prior binding of the same name. A plain `let` (immutable)
                    // is reassignment-locked; `let mut`/`var` are not.
                    let binding = if *mutable {
                        Binding::owned(name.clone(), inferred_type, true)
                    } else {
                        Binding::let_immutable(name.clone(), inferred_type)
                    };
                    scope.define(binding);
                } else {
                    // Bare `x = …` (shell-style): assignment when the binding
                    // already exists, else a fresh mutable declaration. Writing
                    // to an immutable `let` binding is the hard error E0100.
                    let locked = scope.lookup(name).map(|b| b.let_locked);
                    match locked {
                        Some(true) => {
                            self.emit_hard_coded(
                                "E0100",
                                sp,
                                format!("cannot assign to immutable `let` binding `{}`", name),
                                Some(format!(
                                    "`{name}` was declared with `let`, which is immutable. \
                                     Declare it with `var {name} = …` (relaxed) or \
                                     `let mut {name} = …` (explicitly mutable) to allow reassignment.",
                                    name = name
                                )),
                            );
                        }
                        Some(false) => { /* mutable binding: ordinary reassignment */ }
                        None => {
                            scope.define(Binding::owned(name.clone(), inferred_type, true));
                        }
                    }
                }
            }

            Statement::FnDecl { name, params, body, .. } => {
                scope.define(Binding::owned(
                    name.clone(),
                    RanType::Function {
                        params: vec![],
                        return_type: Box::new(RanType::Void),
                    },
                    false,
                ));

                let parent = std::mem::replace(scope, Scope::new());
                *scope = Scope::child(parent);

                // Make parameters visible as bindings so move tracking inside
                // the body is meaningful. `&`/`&mut` params are references and
                // therefore Copy (not subject to moves).
                for p in params {
                    let ty = p
                        .type_annotation
                        .as_ref()
                        .map(type_expr_to_ran)
                        .unwrap_or(RanType::Unknown);
                    scope.define(Binding::owned(p.name.clone(), ty, p.is_mut));
                }

                for s in body {
                    self.check_statement(s, scope);
                }

                if let Some(parent) = scope.parent.take() {
                    *scope = *parent;
                }
            }

            Statement::Echo { expr, .. } => {
                self.check_expr_ownership(expr, scope, sp);
                let recorded = self.record_transient_borrows_top(expr, scope, sp);
                self.apply_moves(expr, scope, sp);
                self.release_borrows(scope, &recorded);
                self.check_inner_bodies(expr, scope, sp);
            }

            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                self.check_expr_ownership(condition, scope, sp);
                let recorded = self.record_transient_borrows_top(condition, scope, sp);
                self.apply_moves(condition, scope, sp);
                self.release_borrows(scope, &recorded);
                self.check_inner_bodies(condition, scope, sp);
                for s in then_body {
                    self.check_statement(s, scope);
                }
                if let Some(else_stmts) = else_body {
                    for s in else_stmts {
                        self.check_statement(s, scope);
                    }
                }
            }

            Statement::For {
                variable,
                iterable,
                body,
            } => {
                self.check_expr_ownership(iterable, scope, sp);
                let recorded = self.record_transient_borrows_top(iterable, scope, sp);
                self.apply_moves(iterable, scope, sp);
                self.release_borrows(scope, &recorded);
                self.check_inner_bodies(iterable, scope, sp);
                scope.define(Binding::owned(variable.clone(), RanType::Unknown, false));
                for s in body {
                    self.check_statement(s, scope);
                }
            }

            Statement::While { condition, body } => {
                self.check_expr_ownership(condition, scope, sp);
                let recorded = self.record_transient_borrows_top(condition, scope, sp);
                self.apply_moves(condition, scope, sp);
                self.release_borrows(scope, &recorded);
                self.check_inner_bodies(condition, scope, sp);
                for s in body {
                    self.check_statement(s, scope);
                }
            }

            Statement::Spawn { body } => {
                // Data-race detection (R14.6 → E0613). A spawned thread runs
                // with a *cloned* environment, so any binding it captures from
                // an enclosing scope becomes shared state once the body writes
                // to it without synchronization. Compute the captured set from
                // the enclosing scope *before* descending into the body's child
                // scope, then flag unsynchronized direct writes.
                let mut captured: HashSet<String> = HashSet::new();
                scope.collect_visible_names(&mut captured);
                self.check_spawn_data_races(body, &captured);

                let parent = std::mem::replace(scope, Scope::new());
                *scope = Scope::child(parent);
                for s in body {
                    self.check_statement(s, scope);
                }
                if let Some(parent) = scope.parent.take() {
                    *scope = *parent;
                }
            }

            Statement::Expr(expr) => {
                self.check_expr_ownership(expr, scope, sp);
                let recorded = self.record_transient_borrows_top(expr, scope, sp);
                self.apply_moves(expr, scope, sp);
                self.release_borrows(scope, &recorded);
                self.check_inner_bodies(expr, scope, sp);
            }

            Statement::Return(Some(expr)) => {
                self.check_expr_ownership(expr, scope, sp);
                // Dangling (R10.7 → E0214): returning a reference to a value
                // defined in this function body lets the borrow outlive its
                // referent's frame.
                self.check_return_dangling(expr, scope, sp);
                let recorded = self.record_transient_borrows_top(expr, scope, sp);
                self.apply_moves(expr, scope, sp);
                self.release_borrows(scope, &recorded);
                self.check_inner_bodies(expr, scope, sp);
                // `return a;` moves a bare non-Copy binding out of the scope
                // (emits `E0215` if `a` is still borrowed).
                if let Expression::Variable(src) = expr {
                    self.move_if_non_copy(scope, src, sp);
                }
            }

            // break/continue are loop-control statements with no expressions or
            // bindings to analyze — recognize them explicitly so they are never
            // flagged, rather than relying on the catch-all (R8.7).
            Statement::Break | Statement::Continue => {}

            // Trait declarations and `impl`/`impl Trait for Type` blocks: analyze
            // each method body like a normal function body (the `FnDecl` arm
            // gives each its own child scope with its params bound), so genuine
            // move/borrow violations inside method bodies are still caught while
            // valid trait/impl programs pass `--ownership=strict` (R8.6/R8.7).
            Statement::TraitDecl { methods, .. } | Statement::ImplBlock { methods, .. } => {
                for m in methods {
                    self.check_statement(m, scope);
                }
            }

            _ => {}
        }
    }

    /// Mark `name` as moved at `span` when it refers to a non-`Copy` binding
    /// that has not already been moved. `Copy` bindings (int/float/bool/void
    /// and references) are duplicated rather than moved, so this is a no-op for
    /// them.
    ///
    /// Move-while-borrowed (R10.8 → `E0215`): if the binding still has an
    /// active borrow, the move is rejected with `E0215` and the binding is left
    /// un-moved (so we don't cascade a spurious `E0210` on later uses).
    fn move_if_non_copy(&mut self, scope: &mut Scope, name: &str, span: Span) {
        let (should_move, borrowed, borrow_loc) = match scope.lookup(name) {
            Some(b) => (
                !b.is_copy() && !b.is_moved(),
                !b.borrows.is_empty(),
                b.borrows.active_location(),
            ),
            None => (false, false, None),
        };
        if !should_move {
            return;
        }
        if borrowed {
            let help = match borrow_loc {
                Some(s) => format!(
                    "Nilai `{}` masih dipinjam (borrow di {}:{}). Akhiri borrow sebelum memindahkan nilai.",
                    name, s.line, s.col
                ),
                None => format!(
                    "Nilai `{}` masih dipinjam. Akhiri borrow sebelum memindahkan nilai.",
                    name
                ),
            };
            self.emit_ownership_coded(
                "E0215",
                span,
                format!("cannot move out of `{}` because it is borrowed", name),
                Some(help),
            );
            return;
        }
        scope.mark_moved(name, span);
    }

    /// Map a unary operator to the borrow kind it introduces, if any. `&`
    /// (`Ref`) is a shared borrow, `&mut` (`MutRef`) an exclusive one; other
    /// unary operators (`Neg`/`Not`/`Deref`) take no borrow.
    fn borrow_kind(op: &UnaryOperator) -> Option<BorrowKind> {
        match op {
            UnaryOperator::Ref => Some(BorrowKind::Shared),
            UnaryOperator::MutRef => Some(BorrowKind::Mut),
            _ => None,
        }
    }

    /// Resolve the root variable being borrowed from a (possibly nested) lvalue
    /// expression: `x`, `obj.field`, `arr[i]` all root at `x`/`obj`/`arr`.
    /// Returns `None` for non-lvalue operands (e.g. a borrow of a temporary).
    fn borrow_root(expr: &Expression) -> Option<&str> {
        match expr {
            Expression::Variable(n) => Some(n),
            Expression::FieldAccess { object, .. } => Self::borrow_root(object),
            Expression::Index { object, .. } => Self::borrow_root(object),
            _ => None,
        }
    }

    /// Human-readable description of a borrow kind for diagnostics.
    fn borrow_kind_str(kind: BorrowKind) -> &'static str {
        match kind {
            BorrowKind::Shared => "immutable (`&`)",
            BorrowKind::Mut => "mutable (`&mut`)",
        }
    }

    /// Attempt to record a borrow of `kind` against `name`, emitting `E0212`
    /// on conflict (R10.2/R10.4). Returns `true` iff the borrow was actually
    /// recorded (so transient callers know what to release). Unknown bindings
    /// (e.g. module/global names the model does not track) are ignored.
    fn try_record_borrow(
        &mut self,
        scope: &mut Scope,
        name: &str,
        kind: BorrowKind,
        at: Span,
    ) -> bool {
        match scope.record_borrow(name, kind, at) {
            Some(Ok(())) => true,
            Some(Err(conflict)) => {
                let help = match conflict {
                    BorrowConflict::ExclusiveActive(sp) => format!(
                        "`{}` sudah dipinjam secara `&mut` di {}:{}. Persempit masa hidup borrow.",
                        name, sp.line, sp.col
                    ),
                    BorrowConflict::SharedActive(n) => format!(
                        "`{}` sudah dipinjam `&` ({} borrow aktif). Akhiri borrow `&` sebelum meminjam `&mut`.",
                        name, n
                    ),
                    BorrowConflict::None => String::new(),
                };
                self.emit_ownership_coded(
                    "E0212",
                    at,
                    format!(
                        "cannot borrow `{}` as {} because it is already borrowed",
                        name,
                        Self::borrow_kind_str(kind)
                    ),
                    Some(help),
                );
                false
            }
            None => false,
        }
    }

    /// Record the borrows introduced by a `let` initializer. A top-level
    /// `&x`/`&mut x` is a **bound** borrow: it is recorded against `x` and kept
    /// active for the rest of the scope (it is released when the scope's
    /// bindings are discarded). Any other shape delegates to the transient
    /// collector and releases immediately.
    ///
    /// Borrow-lifetime model (documented, intentionally lexical / pre-NLL):
    /// bound borrows live to the end of the enclosing function body; transient
    /// borrows live only for the statement in which they appear. This is sound
    /// (never accepts a clear violation) but incomplete (it may flag some
    /// NLL-valid reborrow-after-last-use patterns) — acceptable because the
    /// default `--ownership=warn` mode surfaces these as warnings.
    fn record_bound_or_transient_borrows(
        &mut self,
        value: &Expression,
        scope: &mut Scope,
        sp: Span,
    ) {
        if let Expression::UnaryOp { op, operand } = value {
            if let Some(kind) = Self::borrow_kind(op) {
                if let Some(root) = Self::borrow_root(operand).map(|s| s.to_string()) {
                    // Bound borrow: persists, never released here.
                    self.try_record_borrow(scope, &root, kind, sp);
                    return;
                }
            }
        }
        let recorded = self.record_transient_borrows_top(value, scope, sp);
        self.release_borrows(scope, &recorded);
    }

    /// Walk `expr` recording every borrow expression it contains as a transient
    /// borrow, emitting `E0212` on conflicts. Returns the list of borrows that
    /// were actually recorded so the caller can release them at the end of the
    /// statement.
    fn record_transient_borrows_top(
        &mut self,
        expr: &Expression,
        scope: &mut Scope,
        sp: Span,
    ) -> Vec<(String, BorrowKind)> {
        let mut recorded = Vec::new();
        self.record_transient_borrows(expr, scope, sp, &mut recorded);
        recorded
    }

    fn record_transient_borrows(
        &mut self,
        expr: &Expression,
        scope: &mut Scope,
        sp: Span,
        recorded: &mut Vec<(String, BorrowKind)>,
    ) {
        match expr {
            Expression::UnaryOp { op, operand } => {
                if let Some(kind) = Self::borrow_kind(op) {
                    if let Some(root) = Self::borrow_root(operand).map(|s| s.to_string()) {
                        if self.try_record_borrow(scope, &root, kind, sp) {
                            recorded.push((root, kind));
                        }
                        return;
                    }
                }
                self.record_transient_borrows(operand, scope, sp, recorded);
            }
            Expression::FnCall { callee, args } => {
                self.record_transient_borrows(callee, scope, sp, recorded);
                for a in args {
                    self.record_transient_borrows(a, scope, sp, recorded);
                }
            }
            Expression::MethodCall { object, args, .. } => {
                self.record_transient_borrows(object, scope, sp, recorded);
                for a in args {
                    self.record_transient_borrows(a, scope, sp, recorded);
                }
            }
            Expression::BinaryOp { left, right, .. } => {
                self.record_transient_borrows(left, scope, sp, recorded);
                self.record_transient_borrows(right, scope, sp, recorded);
            }
            Expression::Array(elems) => {
                for e in elems {
                    self.record_transient_borrows(e, scope, sp, recorded);
                }
            }
            Expression::Index { object, index } => {
                self.record_transient_borrows(object, scope, sp, recorded);
                self.record_transient_borrows(index, scope, sp, recorded);
            }
            Expression::FieldAccess { object, .. } => {
                self.record_transient_borrows(object, scope, sp, recorded)
            }
            Expression::Pipe { left, right } => {
                self.record_transient_borrows(left, scope, sp, recorded);
                self.record_transient_borrows(right, scope, sp, recorded);
            }
            Expression::ChanSend { channel, value } => {
                self.record_transient_borrows(channel, scope, sp, recorded);
                self.record_transient_borrows(value, scope, sp, recorded);
            }
            Expression::ChanRecv { channel } => {
                self.record_transient_borrows(channel, scope, sp, recorded)
            }
            Expression::Await(inner) => {
                self.record_transient_borrows(inner, scope, sp, recorded)
            }
            Expression::Match { subject, .. } => {
                self.record_transient_borrows(subject, scope, sp, recorded)
            }
            _ => {}
        }
    }

    /// Release a set of previously-recorded (transient) borrows.
    fn release_borrows(&mut self, scope: &mut Scope, recorded: &[(String, BorrowKind)]) {
        for (name, kind) in recorded {
            scope.release_borrow(name, *kind);
        }
    }

    /// Dangling-reference check (R10.7 → `E0214`): a `return &x` / `return &mut x`
    /// where `x` is defined *directly* in the current function body returns a
    /// reference to a value that will not outlive the call, so the borrow would
    /// dangle.
    ///
    /// Detected: `return &local` / `return &mut local` for a local (or
    /// by-value parameter) of the current function body. Out of scope for this
    /// task (documented): references escaping through aggregates, fields, or a
    /// reference *binding* whose referent lives in an inner block (the checker
    /// walks `if`/`for`/`while` bodies in the enclosing function scope rather
    /// than a nested one, so inner→outer escape is not modelled here).
    fn check_return_dangling(&mut self, expr: &Expression, scope: &Scope, sp: Span) {
        if let Expression::UnaryOp { op, operand } = expr {
            if Self::borrow_kind(op).is_some() {
                if let Some(root) = Self::borrow_root(operand) {
                    if scope.is_locally_defined(root) {
                        self.emit_ownership_coded(
                            "E0214",
                            sp,
                            format!(
                                "returns a reference to local value `{}`",
                                root
                            ),
                            Some(format!(
                                "Referensi ke `{}` hidup lebih lama dari nilainya. Kembalikan nilai berkepemilikan (mis. klona) alih-alih `&{}`.",
                                root, root
                            )),
                        );
                    }
                }
            }
        }
    }

    /// Walk an expression and apply move semantics for value-passing calls:
    /// passing a bare non-`Copy` variable *by value* to a user-defined function
    /// moves that variable. Read-only builtins (`echo`, `len`, `push`, …) and
    /// module/object method calls do not move their bare arguments. Borrows
    /// (`&x`/`&mut x`) are references, not moves, and are left untouched.
    ///
    /// Scope handled this iteration (kept conservative to avoid false positives
    /// in `strict` for ordinary correct code): `let b = a;`, `return a;`, and
    /// passing a bare variable by value to a user function. Moves into structs,
    /// arrays, or maps are out of scope for 7.1 (tasks 7.2/7.3 extend this).
    fn apply_moves(&mut self, expr: &Expression, scope: &mut Scope, span: Span) {
        match expr {
            Expression::FnCall { callee, args } => {
                self.apply_moves(callee, scope, span);
                for a in args {
                    self.apply_moves(a, scope, span);
                }
                if let Expression::Variable(fname) = callee.as_ref() {
                    if !is_readonly_builtin(fname) {
                        for a in args {
                            if let Expression::Variable(v) = a {
                                self.move_if_non_copy(scope, v, span);
                            }
                        }
                    }
                }
            }
            Expression::MethodCall { object, args, .. } => {
                self.apply_moves(object, scope, span);
                for a in args {
                    self.apply_moves(a, scope, span);
                }
            }
            Expression::BinaryOp { left, right, .. } => {
                self.apply_moves(left, scope, span);
                self.apply_moves(right, scope, span);
            }
            Expression::UnaryOp { operand, .. } => self.apply_moves(operand, scope, span),
            Expression::Array(elems) => {
                for e in elems {
                    self.apply_moves(e, scope, span);
                }
            }
            Expression::Index { object, index } => {
                self.apply_moves(object, scope, span);
                self.apply_moves(index, scope, span);
            }
            Expression::FieldAccess { object, .. } => self.apply_moves(object, scope, span),
            Expression::Pipe { left, right } => {
                self.apply_moves(left, scope, span);
                self.apply_moves(right, scope, span);
            }
            Expression::ChanSend { channel, value } => {
                self.apply_moves(channel, scope, span);
                self.apply_moves(value, scope, span);
            }
            Expression::ChanRecv { channel } => self.apply_moves(channel, scope, span),
            Expression::Await(inner) => self.apply_moves(inner, scope, span),
            Expression::Match { subject, .. } => self.apply_moves(subject, scope, span),
            _ => {}
        }
    }

    /// Traverse `expr` and analyze the statement bodies nested *inside* it —
    /// closure/lambda bodies and `match`-arm bodies — so genuine move/borrow
    /// violations there are still caught (R8.7), without producing
    /// false positives on captured outer bindings.
    ///
    /// * **Closures (`Expression::Lambda`)** are analyzed in a *fresh, isolated*
    ///   scope: the enclosing bindings are intentionally not visible, so reading
    ///   or using a captured variable inside the closure is treated as a
    ///   read/borrow and never moves or invalidates the outer binding (no false
    ///   `E0210`/`E0212`/`E0214`/`E0215`). The lambda's own parameters are fresh
    ///   bindings in that closure scope, and violations *among the closure's own
    ///   locals* are still detected.
    /// * **`match` arms** are analyzed in a child scope (like `if` branches), so
    ///   a `match`-arm `return` and any moves/borrows in the arm body are walked
    ///   normally. A `Pattern::Variable` binds a fresh local for the arm.
    ///
    /// All other expression shapes are traversed structurally so nested closures
    /// / matches anywhere in the tree are reached.
    fn check_inner_bodies(&mut self, expr: &Expression, scope: &mut Scope, sp: Span) {
        match expr {
            Expression::Lambda { params, body } => {
                // Isolated scope: captured outer bindings are not reachable here,
                // so they are treated as reads/borrows, never moves.
                let mut closure_scope = Scope::new();
                for p in params {
                    let ty = p
                        .type_annotation
                        .as_ref()
                        .map(type_expr_to_ran)
                        .unwrap_or(RanType::Unknown);
                    closure_scope.define(Binding::owned(p.name.clone(), ty, p.is_mut));
                }
                for s in body {
                    self.check_statement(s, &mut closure_scope);
                }
            }
            Expression::Match { subject, arms } => {
                self.check_inner_bodies(subject, scope, sp);
                for arm in arms {
                    // Each arm body runs in its own child scope (like an `if`
                    // branch), with any pattern-bound variable as a fresh local.
                    let parent = std::mem::replace(scope, Scope::new());
                    *scope = Scope::child(parent);
                    if let Pattern::Variable(n) = &arm.pattern {
                        scope.define(Binding::owned(n.clone(), RanType::Unknown, false));
                    }
                    for s in &arm.body {
                        self.check_statement(s, scope);
                    }
                    if let Some(parent) = scope.parent.take() {
                        *scope = *parent;
                    }
                }
            }
            Expression::BinaryOp { left, right, .. } => {
                self.check_inner_bodies(left, scope, sp);
                self.check_inner_bodies(right, scope, sp);
            }
            Expression::UnaryOp { operand, .. } => self.check_inner_bodies(operand, scope, sp),
            Expression::FnCall { callee, args } => {
                self.check_inner_bodies(callee, scope, sp);
                for a in args {
                    self.check_inner_bodies(a, scope, sp);
                }
            }
            Expression::MethodCall { object, args, .. } => {
                self.check_inner_bodies(object, scope, sp);
                for a in args {
                    self.check_inner_bodies(a, scope, sp);
                }
            }
            Expression::FieldAccess { object, .. } => self.check_inner_bodies(object, scope, sp),
            Expression::Index { object, index } => {
                self.check_inner_bodies(object, scope, sp);
                self.check_inner_bodies(index, scope, sp);
            }
            Expression::Array(elems) => {
                for e in elems {
                    self.check_inner_bodies(e, scope, sp);
                }
            }
            Expression::StructInit { fields, .. } => {
                for (_, e) in fields {
                    self.check_inner_bodies(e, scope, sp);
                }
            }
            Expression::Pipe { left, right } => {
                self.check_inner_bodies(left, scope, sp);
                self.check_inner_bodies(right, scope, sp);
            }
            Expression::ChanSend { channel, value } => {
                self.check_inner_bodies(channel, scope, sp);
                self.check_inner_bodies(value, scope, sp);
            }
            Expression::ChanRecv { channel } => self.check_inner_bodies(channel, scope, sp),
            Expression::Await(inner) => self.check_inner_bodies(inner, scope, sp),
            _ => {}
        }
    }

    /// Recursively scan an expression for reads/borrows of a binding that has
    /// already been moved, emitting a coded `E0210` use-after-move finding at
    /// `span` for each offending use (R9.1/R9.2). Reading, re-moving, or
    /// borrowing a moved binding all count as a use.
    fn check_expr_ownership(&mut self, expr: &Expression, scope: &Scope, span: Span) {
        match expr {
            Expression::Variable(name) => {
                if let Some(binding) = scope.lookup(name) {
                    if binding.is_moved() {
                        let help = match binding.move_location() {
                            Some(s) => format!(
                                "Nilai `{}` sudah dipindahkan di {}:{}. Klona nilai atau pinjam dengan `&`.",
                                name, s.line, s.col
                            ),
                            None => format!(
                                "Nilai `{}` sudah dipindahkan. Klona nilai atau pinjam dengan `&`.",
                                name
                            ),
                        };
                        self.emit_ownership_coded(
                            "E0210",
                            span,
                            format!("use of moved value: `{}`", name),
                            Some(help),
                        );
                    }
                }
            }
            Expression::BinaryOp { left, right, .. } => {
                self.check_expr_ownership(left, scope, span);
                self.check_expr_ownership(right, scope, span);
            }
            Expression::UnaryOp { operand, .. } => self.check_expr_ownership(operand, scope, span),
            Expression::FnCall { callee, args } => {
                self.check_expr_ownership(callee, scope, span);
                for a in args {
                    self.check_expr_ownership(a, scope, span);
                }
            }
            Expression::MethodCall { object, args, .. } => {
                self.check_expr_ownership(object, scope, span);
                for a in args {
                    self.check_expr_ownership(a, scope, span);
                }
            }
            Expression::FieldAccess { object, .. } => {
                self.check_expr_ownership(object, scope, span)
            }
            Expression::Index { object, index } => {
                self.check_expr_ownership(object, scope, span);
                self.check_expr_ownership(index, scope, span);
            }
            Expression::Array(elems) => {
                for e in elems {
                    self.check_expr_ownership(e, scope, span);
                }
            }
            Expression::Pipe { left, right } => {
                self.check_expr_ownership(left, scope, span);
                self.check_expr_ownership(right, scope, span);
            }
            Expression::ChanSend { channel, value } => {
                self.check_expr_ownership(channel, scope, span);
                self.check_expr_ownership(value, scope, span);
            }
            Expression::ChanRecv { channel } => self.check_expr_ownership(channel, scope, span),
            Expression::Await(inner) => self.check_expr_ownership(inner, scope, span),
            Expression::Match { subject, .. } => self.check_expr_ownership(subject, scope, span),
            _ => {}
        }
    }

    /// Data-race detection for `spawn` bodies (R14.6 → `E0613`).
    ///
    /// A spawned thread runs with a *cloned* environment, so any binding it
    /// captures from an enclosing scope becomes shared mutable state once the
    /// body writes to it without going through a synchronization primitive.
    ///
    /// Heuristic (intentionally **conservative** — prefer false negatives over
    /// false positives so that correct programs still pass `--ownership=strict`,
    /// which feeds property test P8):
    ///
    /// A captured outer binding is flagged as an unsynchronized data race when
    /// the body performs a *direct* write to it:
    ///   * **(W1)** a read-modify-write reassignment `x = <expr that reads x>`
    ///     where `x` is a captured outer binding (e.g. `counter = counter + 1`);
    ///   * **(W2)** a mutating builtin call `push(x, ...)` / `set(x, ...)` whose
    ///     target `x` is a captured outer binding.
    ///
    /// Writes performed through the concurrency module — `conc.shared(...)`,
    /// `conc.lock(...)`, `conc.set(...)`, `conc.shared_set(...)`, … — are
    /// *method calls* (`obj.method(args)`), never a bare `=`/`push`/`set`, so
    /// they are never matched here and are therefore treated as synchronized (no
    /// `E0613`). This is exactly the synchronized-handle exemption the design
    /// calls for, expressed as a form-based rule the static checker can apply
    /// without tracking value provenance.
    ///
    /// Why **W1** keys on a read-modify-write rather than on *any* assignment:
    /// the parser lowers both `let x = …` (fresh local) and `x = …`
    /// (reassignment) to the same `VarDecl` node, so the AST cannot tell a
    /// shadowing declaration from a write to the captured binding. Requiring the
    /// right-hand side to *read* the captured binding isolates the clear race
    /// pattern (mutating shared state based on its current value) and avoids
    /// false-positively flagging a local that merely shadows an outer name.
    /// Once a name is (re)declared inside the body it is treated as a body-local
    /// for the remainder of the walk, so later writes to it are not re-flagged.
    fn check_spawn_data_races(&mut self, body: &[Stmt], captured: &HashSet<String>) {
        // Names declared as locals *within* the spawn body. Such names shadow
        // any captured binding of the same name, so writes to them are local
        // (not a shared-state race).
        let mut local: HashSet<String> = HashSet::new();
        for s in body {
            self.scan_stmt_for_races(s, captured, &mut local);
        }
    }

    /// Walk one statement of a `spawn` body looking for unsynchronized writes to
    /// captured bindings (see [`TypeChecker::check_spawn_data_races`]).
    fn scan_stmt_for_races(
        &mut self,
        s: &Stmt,
        captured: &HashSet<String>,
        local: &mut HashSet<String>,
    ) {
        let sp = s.span;
        match &s.kind {
            Statement::VarDecl { name, value, .. } => {
                // (W1) read-modify-write of a captured binding not shadowed by a
                // body-local of the same name.
                if captured.contains(name)
                    && !local.contains(name)
                    && expr_reads_var(value, name)
                {
                    self.emit_data_race(name, sp);
                }
                // (W2) mutating-builtin writes nested in the initializer.
                self.scan_expr_for_races(value, captured, local, sp);
                // From here on, `name` is a body-local: subsequent writes to it
                // are local, not captured.
                local.insert(name.clone());
            }
            Statement::Expr(expr) | Statement::Echo { expr, .. } => {
                self.scan_expr_for_races(expr, captured, local, sp);
            }
            Statement::Return(Some(expr)) => {
                self.scan_expr_for_races(expr, captured, local, sp);
            }
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                self.scan_expr_for_races(condition, captured, local, sp);
                for st in then_body {
                    self.scan_stmt_for_races(st, captured, local);
                }
                if let Some(else_stmts) = else_body {
                    for st in else_stmts {
                        self.scan_stmt_for_races(st, captured, local);
                    }
                }
            }
            Statement::For {
                variable,
                iterable,
                body,
            } => {
                self.scan_expr_for_races(iterable, captured, local, sp);
                // The loop variable is a body-local.
                local.insert(variable.clone());
                for st in body {
                    self.scan_stmt_for_races(st, captured, local);
                }
            }
            Statement::While { condition, body } => {
                self.scan_expr_for_races(condition, captured, local, sp);
                for st in body {
                    self.scan_stmt_for_races(st, captured, local);
                }
            }
            // Nested `spawn` blocks are handled by `check_statement`'s own
            // `Spawn` arm (with the correct captured set for that point), so we
            // do not descend into them here to avoid double-reporting.
            _ => {}
        }
    }

    /// Scan an expression for **(W2)** mutating-builtin writes — `push(x, …)` /
    /// `set(x, …)` — whose target `x` is a captured outer binding.
    fn scan_expr_for_races(
        &mut self,
        expr: &Expression,
        captured: &HashSet<String>,
        local: &HashSet<String>,
        span: Span,
    ) {
        match expr {
            Expression::FnCall { callee, args } => {
                if let Expression::Variable(fname) = callee.as_ref() {
                    if matches!(fname.as_str(), "push" | "set") {
                        if let Some(Expression::Variable(target)) = args.first() {
                            if captured.contains(target) && !local.contains(target) {
                                self.emit_data_race(target, span);
                            }
                        }
                    }
                }
                self.scan_expr_for_races(callee, captured, local, span);
                for a in args {
                    self.scan_expr_for_races(a, captured, local, span);
                }
            }
            Expression::MethodCall { object, args, .. } => {
                // A `conc.set(...)` / `conc.shared_set(...)` write goes through a
                // synchronization handle (method call), so it is *not* a race.
                // We still scan the receiver and arguments for nested direct
                // writes.
                self.scan_expr_for_races(object, captured, local, span);
                for a in args {
                    self.scan_expr_for_races(a, captured, local, span);
                }
            }
            Expression::BinaryOp { left, right, .. } => {
                self.scan_expr_for_races(left, captured, local, span);
                self.scan_expr_for_races(right, captured, local, span);
            }
            Expression::UnaryOp { operand, .. } => {
                self.scan_expr_for_races(operand, captured, local, span)
            }
            Expression::Array(elems) => {
                for e in elems {
                    self.scan_expr_for_races(e, captured, local, span);
                }
            }
            Expression::Index { object, index } => {
                self.scan_expr_for_races(object, captured, local, span);
                self.scan_expr_for_races(index, captured, local, span);
            }
            Expression::FieldAccess { object, .. } => {
                self.scan_expr_for_races(object, captured, local, span)
            }
            Expression::Pipe { left, right } => {
                self.scan_expr_for_races(left, captured, local, span);
                self.scan_expr_for_races(right, captured, local, span);
            }
            Expression::ChanSend { channel, value } => {
                self.scan_expr_for_races(channel, captured, local, span);
                self.scan_expr_for_races(value, captured, local, span);
            }
            Expression::ChanRecv { channel } => {
                self.scan_expr_for_races(channel, captured, local, span)
            }
            Expression::Await(inner) => self.scan_expr_for_races(inner, captured, local, span),
            Expression::Match { subject, .. } => {
                self.scan_expr_for_races(subject, captured, local, span)
            }
            _ => {}
        }
    }

    /// Emit a coded `E0613` data-race finding for an unsynchronized write to a
    /// captured binding `name`. Routed through `emit_ownership_coded` so the
    /// analyzer downgrades it to a warning in `--ownership=warn` and aborts on
    /// it in `--ownership=strict`, consistent with tasks 7.1/7.2.
    fn emit_data_race(&mut self, name: &str, span: Span) {
        let help = format!(
            "Binding `{}` ditangkap oleh `spawn` lalu ditulis tanpa sinkronisasi. \
             Bungkus dengan `shared`/`lock` atau kirim lewat channel.",
            name
        );
        self.emit_ownership_coded(
            "E0613",
            span,
            format!("unsynchronized write to captured shared state: `{}`", name),
            Some(help),
        );
    }

    fn infer_type(&self, expr: &Expression, _scope: &Scope) -> RanType {
        match expr {
            Expression::IntLiteral(_) => RanType::Int,
            Expression::FloatLiteral(_) => RanType::Float,
            Expression::StringLiteral(_) => RanType::Str,
            Expression::BoolLiteral(_) => RanType::Bool,
            Expression::Array(_) => RanType::Array(Box::new(RanType::Unknown)),
            _ => RanType::Unknown,
        }
    }
}

/// Whether `expr` reads the variable `name` anywhere within it. Used by the
/// `spawn` data-race check to recognise a read-modify-write reassignment
/// (`x = … x …`) of a captured binding.
fn expr_reads_var(expr: &Expression, name: &str) -> bool {
    match expr {
        Expression::Variable(n) => n == name,
        Expression::BinaryOp { left, right, .. } => {
            expr_reads_var(left, name) || expr_reads_var(right, name)
        }
        Expression::UnaryOp { operand, .. } => expr_reads_var(operand, name),
        Expression::FnCall { callee, args } => {
            expr_reads_var(callee, name) || args.iter().any(|a| expr_reads_var(a, name))
        }
        Expression::MethodCall { object, args, .. } => {
            expr_reads_var(object, name) || args.iter().any(|a| expr_reads_var(a, name))
        }
        Expression::FieldAccess { object, .. } => expr_reads_var(object, name),
        Expression::Index { object, index } => {
            expr_reads_var(object, name) || expr_reads_var(index, name)
        }
        Expression::Array(elems) => elems.iter().any(|e| expr_reads_var(e, name)),
        Expression::Pipe { left, right } => {
            expr_reads_var(left, name) || expr_reads_var(right, name)
        }
        Expression::ChanSend { channel, value } => {
            expr_reads_var(channel, name) || expr_reads_var(value, name)
        }
        Expression::ChanRecv { channel } => expr_reads_var(channel, name),
        Expression::Await(inner) => expr_reads_var(inner, name),
        Expression::Match { subject, .. } => expr_reads_var(subject, name),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(line: usize, col: usize) -> Span {
        Span::new(line, col)
    }

    // ---- Copy / non-Copy classification ----

    #[test]
    fn scalars_are_copy() {
        assert!(RanType::Int.is_copy());
        assert!(RanType::Float.is_copy());
        assert!(RanType::Bool.is_copy());
        assert!(RanType::Void.is_copy());
    }

    #[test]
    fn aggregates_and_str_are_non_copy() {
        assert!(!RanType::Str.is_copy());
        assert!(!RanType::Array(Box::new(RanType::Int)).is_copy());
        assert!(!RanType::Channel(Box::new(RanType::Int)).is_copy());
        assert!(!RanType::Unknown.is_copy());
        assert!(!RanType::Struct {
            name: "P".into(),
            fields: HashMap::new()
        }
        .is_copy());
    }

    #[test]
    fn type_name_classifier_matches_design_table() {
        for copy in ["int", "Integer", "float", "double", "bool", "BOOLEAN", "void"] {
            assert!(is_copy_type_name(copy), "{copy} should be Copy");
        }
        for non_copy in ["str", "string", "array", "map", "object", "channel", "handle"] {
            assert!(!is_copy_type_name(non_copy), "{non_copy} should be non-Copy");
        }
    }

    // ---- Move-state transitions ----

    #[test]
    fn moving_non_copy_binding_records_location() {
        let mut b = Binding::owned("s".into(), RanType::Str, false);
        assert!(!b.is_moved());
        assert_eq!(b.move_location(), None);

        b.mark_moved(sp(3, 5));
        assert!(b.is_moved());
        assert_eq!(b.ownership, OwnershipState::Moved);
        assert_eq!(b.move_location(), Some(sp(3, 5)));
    }

    #[test]
    fn moving_copy_binding_is_noop() {
        let mut b = Binding::owned("n".into(), RanType::Int, false);
        b.mark_moved(sp(1, 1));
        assert!(!b.is_moved());
        assert_eq!(b.ownership, OwnershipState::Owned);
        assert_eq!(b.move_location(), None);
    }

    #[test]
    fn scope_mark_moved_walks_parent_chain() {
        let mut parent = Scope::new();
        parent.define(Binding::owned("outer".into(), RanType::Str, false));
        let mut child = Scope::child(parent);
        child.define(Binding::owned("inner".into(), RanType::Str, false));

        child.mark_moved("outer", sp(7, 2));
        assert!(child.lookup("outer").unwrap().is_moved());
        assert!(!child.lookup("inner").unwrap().is_moved());
    }

    // ---- Borrow conflict queries (R10.1 / R10.3 / R10.5) ----

    #[test]
    fn multiple_shared_borrows_allowed() {
        let mut set = BorrowSet::new();
        assert_eq!(set.record(BorrowKind::Shared, sp(1, 1)), Ok(()));
        assert_eq!(set.record(BorrowKind::Shared, sp(1, 2)), Ok(()));
        assert_eq!(set.record(BorrowKind::Shared, sp(1, 3)), Ok(()));
        assert_eq!(set.shared_count(), 3);
        assert!(!set.has_exclusive());
    }

    #[test]
    fn exclusive_borrow_blocks_other_borrows() {
        let mut set = BorrowSet::new();
        assert_eq!(set.record(BorrowKind::Mut, sp(2, 1)), Ok(()));
        // A second &mut is blocked by the active exclusive borrow.
        assert_eq!(
            set.record(BorrowKind::Mut, sp(2, 2)),
            Err(BorrowConflict::ExclusiveActive(sp(2, 1)))
        );
        // A & is also blocked while &mut is active.
        assert_eq!(
            set.record(BorrowKind::Shared, sp(2, 3)),
            Err(BorrowConflict::ExclusiveActive(sp(2, 1)))
        );
    }

    #[test]
    fn shared_borrow_blocks_exclusive() {
        let mut set = BorrowSet::new();
        set.record(BorrowKind::Shared, sp(1, 1)).unwrap();
        set.record(BorrowKind::Shared, sp(1, 2)).unwrap();
        assert_eq!(
            set.record(BorrowKind::Mut, sp(1, 9)),
            Err(BorrowConflict::SharedActive(2))
        );
    }

    #[test]
    fn releasing_borrows_re_allows_mut() {
        let mut set = BorrowSet::new();
        set.record(BorrowKind::Shared, sp(1, 1)).unwrap();
        set.record(BorrowKind::Shared, sp(1, 2)).unwrap();
        // Blocked while shared borrows are live.
        assert!(matches!(
            set.conflict(BorrowKind::Mut),
            BorrowConflict::SharedActive(2)
        ));

        set.release(BorrowKind::Shared);
        set.release(BorrowKind::Shared);
        assert!(set.is_empty());
        // Now a fresh &mut is permitted (R10.5).
        assert_eq!(set.conflict(BorrowKind::Mut), BorrowConflict::None);
        assert_eq!(set.record(BorrowKind::Mut, sp(5, 5)), Ok(()));
    }

    #[test]
    fn release_all_clears_every_borrow() {
        let mut set = BorrowSet::new();
        set.record(BorrowKind::Shared, sp(1, 1)).unwrap();
        set.record(BorrowKind::Shared, sp(1, 2)).unwrap();
        set.release_all();
        assert!(set.is_empty());
        assert_eq!(set.conflict(BorrowKind::Mut), BorrowConflict::None);
    }

    // ---- Borrow tracking through a Binding updates ownership state ----

    #[test]
    fn binding_borrow_updates_ownership_state() {
        let mut b = Binding::owned("v".into(), RanType::Str, true);
        b.record_borrow(BorrowKind::Shared, sp(1, 1)).unwrap();
        assert_eq!(b.ownership, OwnershipState::Borrowed);

        // &mut blocked while a shared borrow is active.
        assert_eq!(
            b.record_borrow(BorrowKind::Mut, sp(1, 5)),
            Err(BorrowConflict::SharedActive(1))
        );

        b.release_borrow(BorrowKind::Shared);
        assert_eq!(b.ownership, OwnershipState::Owned);

        b.record_borrow(BorrowKind::Mut, sp(2, 1)).unwrap();
        assert_eq!(b.ownership, OwnershipState::MutBorrowed);

        b.release_borrow(BorrowKind::Mut);
        assert_eq!(b.ownership, OwnershipState::Owned);
    }

    #[test]
    fn scope_borrow_apis_walk_parent_chain() {
        let mut parent = Scope::new();
        parent.define(Binding::owned("g".into(), RanType::Str, true));
        let mut child = Scope::child(parent);

        // Two shared borrows from the child scope: allowed.
        assert_eq!(
            child.record_borrow("g", BorrowKind::Shared, sp(1, 1)),
            Some(Ok(()))
        );
        assert_eq!(
            child.record_borrow("g", BorrowKind::Shared, sp(1, 2)),
            Some(Ok(()))
        );
        // &mut blocked.
        assert_eq!(
            child.borrow_conflict("g", BorrowKind::Mut),
            Some(BorrowConflict::SharedActive(2))
        );
        // Release and retry.
        child.release_all_borrows("g");
        assert_eq!(
            child.record_borrow("g", BorrowKind::Mut, sp(3, 1)),
            Some(Ok(()))
        );
        // Unknown binding => None.
        assert_eq!(child.borrow_conflict("missing", BorrowKind::Mut), None);
    }

    // ---- Controlled error->warning downgrade path ----

    #[test]
    fn downgrade_routes_findings_to_warnings() {
        let mut strict = TypeChecker::new();
        strict.emit_ownership("use after move".into());
        assert_eq!(strict.errors.len(), 1);
        assert_eq!(strict.warnings.len(), 0);

        let mut warn = TypeChecker::new();
        warn.set_downgrade_to_warning(true);
        warn.emit_ownership("use after move".into());
        assert_eq!(warn.errors.len(), 0);
        assert_eq!(warn.warnings.len(), 1);
    }

    // ---- Move tracking & use-after-move in the AST walk (task 7.1) ----

    fn stmt(kind: Statement) -> Stmt {
        Stmt::new(kind, sp(1, 1))
    }

    fn var_decl(name: &str, mutable: bool, value: Expression) -> Stmt {
        stmt(Statement::VarDecl {
            name: name.into(),
            mutable,
            is_decl: true,
            type_annotation: None,
            value,
        })
    }

    fn expr_stmt(expr: Expression) -> Stmt {
        stmt(Statement::Expr(expr))
    }

    fn var(name: &str) -> Expression {
        Expression::Variable(name.into())
    }

    /// Wrap statements in a `main` function body so they share one scope (the
    /// checker tracks ownership per function scope).
    fn program_in_main(body: Vec<Stmt>) -> Program {
        Program {
            statements: vec![stmt(Statement::FnDecl {
                name: "main".into(),
                params: vec![],
                return_type: None,
                body,
                is_pub: false,
                is_async: false,
            })],
        }
    }

    fn check_program(program: &Program) -> TypeChecker {
        let mut checker = TypeChecker::new();
        checker.check(program);
        checker
    }

    #[test]
    fn use_after_move_of_non_copy_via_let_is_flagged_e0210() {
        // let s = "hi"; let t = s; echo s   -> `s` (str, non-Copy) moved into t,
        // then read again => E0210.
        let program = program_in_main(vec![
            var_decl("s", false, Expression::StringLiteral("hi".into())),
            var_decl("t", false, var("s")),
            stmt(Statement::Echo {
                expr: var("s"),
                escapes: false,
            }),
        ]);
        let checker = check_program(&program);
        assert_eq!(
            checker.ownership_findings.len(),
            1,
            "expected exactly one use-after-move finding"
        );
        assert_eq!(checker.ownership_findings[0].code, "E0210");
        assert!(checker.ownership_findings[0]
            .message
            .contains("use of moved value"));
    }

    #[test]
    fn copy_typed_var_reused_freely_is_not_flagged() {
        // let n = 1; let m = n; echo n   -> n is int (Copy), no move, no finding.
        let program = program_in_main(vec![
            var_decl("n", false, Expression::IntLiteral(1)),
            var_decl("m", false, var("n")),
            stmt(Statement::Echo {
                expr: var("n"),
                escapes: false,
            }),
        ]);
        let checker = check_program(&program);
        assert!(
            checker.ownership_findings.is_empty(),
            "Copy-typed reuse must not be flagged: {:?}",
            checker.ownership_findings
        );
    }

    #[test]
    fn moved_value_not_used_again_is_clean() {
        // let s = "hi"; let t = s   -> s moved but never used again, no finding.
        let program = program_in_main(vec![
            var_decl("s", false, Expression::StringLiteral("hi".into())),
            var_decl("t", false, var("s")),
        ]);
        let checker = check_program(&program);
        assert!(
            checker.ownership_findings.is_empty(),
            "a move with no later use must be clean: {:?}",
            checker.ownership_findings
        );
    }

    #[test]
    fn pass_by_value_to_user_fn_then_use_is_flagged() {
        // fn consume(x) {}  fn main() { let s = "hi"; consume(s); echo s }
        let consume = stmt(Statement::FnDecl {
            name: "consume".into(),
            params: vec![Param {
                name: "x".into(),
                type_annotation: None,
                is_mut: false,
            }],
            return_type: None,
            body: vec![],
            is_pub: false,
            is_async: false,
        });
        let main = stmt(Statement::FnDecl {
            name: "main".into(),
            params: vec![],
            return_type: None,
            body: vec![
                var_decl("s", false, Expression::StringLiteral("hi".into())),
                expr_stmt(Expression::FnCall {
                    callee: Box::new(var("consume")),
                    args: vec![var("s")],
                }),
                stmt(Statement::Echo {
                    expr: var("s"),
                    escapes: false,
                }),
            ],
            is_pub: false,
            is_async: false,
        });
        let program = Program {
            statements: vec![consume, main],
        };
        let checker = check_program(&program);
        assert_eq!(checker.ownership_findings.len(), 1);
        assert_eq!(checker.ownership_findings[0].code, "E0210");
    }

    #[test]
    fn passing_to_readonly_builtin_does_not_move() {
        // let s = "hi"; echo(s); echo s  -> echo is read-only, s not moved.
        let program = program_in_main(vec![
            var_decl("s", false, Expression::StringLiteral("hi".into())),
            expr_stmt(Expression::FnCall {
                callee: Box::new(var("echo")),
                args: vec![var("s")],
            }),
            stmt(Statement::Echo {
                expr: var("s"),
                escapes: false,
            }),
        ]);
        let checker = check_program(&program);
        assert!(
            checker.ownership_findings.is_empty(),
            "read-only builtins must not move their args: {:?}",
            checker.ownership_findings
        );
    }

    #[test]
    fn borrow_of_moved_value_is_flagged() {
        // let s = "hi"; let t = s; greet(&s)  -> borrowing a moved value => E0210.
        let program = program_in_main(vec![
            var_decl("s", false, Expression::StringLiteral("hi".into())),
            var_decl("t", false, var("s")),
            expr_stmt(Expression::FnCall {
                callee: Box::new(var("greet")),
                args: vec![Expression::UnaryOp {
                    op: UnaryOperator::Ref,
                    operand: Box::new(var("s")),
                }],
            }),
        ]);
        let checker = check_program(&program);
        assert_eq!(checker.ownership_findings.len(), 1);
        assert_eq!(checker.ownership_findings[0].code, "E0210");
    }

    // ---- Borrow checking, dangling & move-while-borrowed (task 7.2) ----

    fn shared_ref(name: &str) -> Expression {
        Expression::UnaryOp {
            op: UnaryOperator::Ref,
            operand: Box::new(var(name)),
        }
    }

    fn mut_ref(name: &str) -> Expression {
        Expression::UnaryOp {
            op: UnaryOperator::MutRef,
            operand: Box::new(var(name)),
        }
    }

    #[test]
    fn mut_borrow_while_shared_active_is_flagged_e0212() {
        // let x = "hi"; let r = &x; let m = &mut x   -> `&mut` while `&` active => E0212.
        let program = program_in_main(vec![
            var_decl("x", true, Expression::StringLiteral("hi".into())),
            var_decl("r", false, shared_ref("x")),
            var_decl("m", false, mut_ref("x")),
        ]);
        let checker = check_program(&program);
        assert_eq!(
            checker.ownership_findings.len(),
            1,
            "expected one borrow-conflict finding: {:?}",
            checker.ownership_findings
        );
        assert_eq!(checker.ownership_findings[0].code, "E0212");
    }

    #[test]
    fn shared_borrow_while_mut_active_is_flagged_e0212() {
        // let x = "hi"; let m = &mut x; let r = &x   -> `&` while `&mut` active => E0212.
        let program = program_in_main(vec![
            var_decl("x", true, Expression::StringLiteral("hi".into())),
            var_decl("m", false, mut_ref("x")),
            var_decl("r", false, shared_ref("x")),
        ]);
        let checker = check_program(&program);
        assert_eq!(checker.ownership_findings.len(), 1);
        assert_eq!(checker.ownership_findings[0].code, "E0212");
    }

    #[test]
    fn multiple_shared_borrows_are_not_flagged() {
        // let x = "hi"; let a = &x; let b = &x; echo a; echo b -> all shared, no finding.
        let program = program_in_main(vec![
            var_decl("x", false, Expression::StringLiteral("hi".into())),
            var_decl("a", false, shared_ref("x")),
            var_decl("b", false, shared_ref("x")),
            stmt(Statement::Echo {
                expr: var("a"),
                escapes: false,
            }),
            stmt(Statement::Echo {
                expr: var("b"),
                escapes: false,
            }),
        ]);
        let checker = check_program(&program);
        assert!(
            checker.ownership_findings.is_empty(),
            "multiple shared borrows must be allowed: {:?}",
            checker.ownership_findings
        );
    }

    #[test]
    fn mut_borrow_after_transient_borrow_released_is_allowed() {
        // let x = "hi"; peek(&x); let m = &mut x
        // The `&x` passed to peek(...) is transient (released at end of the
        // statement), so the later `&mut x` is allowed (R10.5).
        let program = program_in_main(vec![
            var_decl("x", true, Expression::StringLiteral("hi".into())),
            expr_stmt(Expression::FnCall {
                callee: Box::new(var("peek")),
                args: vec![shared_ref("x")],
            }),
            var_decl("m", false, mut_ref("x")),
        ]);
        let checker = check_program(&program);
        assert!(
            checker.ownership_findings.is_empty(),
            "a fresh `&mut` after the previous borrow ended must be allowed: {:?}",
            checker.ownership_findings
        );
    }

    #[test]
    fn return_reference_to_local_is_flagged_e0214() {
        // fn f() { let x = "hi"; return &x }  -> dangling reference => E0214.
        let f = stmt(Statement::FnDecl {
            name: "f".into(),
            params: vec![],
            return_type: None,
            body: vec![
                var_decl("x", false, Expression::StringLiteral("hi".into())),
                stmt(Statement::Return(Some(shared_ref("x")))),
            ],
            is_pub: false,
            is_async: false,
        });
        let program = Program {
            statements: vec![f],
        };
        let checker = check_program(&program);
        assert_eq!(
            checker.ownership_findings.len(),
            1,
            "expected one dangling-reference finding: {:?}",
            checker.ownership_findings
        );
        assert_eq!(checker.ownership_findings[0].code, "E0214");
    }

    #[test]
    fn move_while_borrowed_is_flagged_e0215() {
        // let x = "hi"; let r = &x; let t = x  -> move of a borrowed value => E0215.
        let program = program_in_main(vec![
            var_decl("x", false, Expression::StringLiteral("hi".into())),
            var_decl("r", false, shared_ref("x")),
            var_decl("t", false, var("x")),
        ]);
        let checker = check_program(&program);
        assert_eq!(
            checker.ownership_findings.len(),
            1,
            "expected one move-while-borrowed finding: {:?}",
            checker.ownership_findings
        );
        assert_eq!(checker.ownership_findings[0].code, "E0215");
    }

    #[test]
    fn well_typed_program_with_borrows_has_no_findings() {
        // A clean program that borrows, reads through the borrow, and later
        // takes a fresh exclusive borrow once the shared one is no longer
        // recorded must produce NO findings (guard against false positives).
        //   let x = "hi";
        //   let a = &x;     // shared (bound)
        //   let b = &x;     // another shared (bound) - allowed
        //   echo a; echo b;
        //   update(&mut y); // independent variable, exclusive transient borrow
        let program = program_in_main(vec![
            var_decl("x", false, Expression::StringLiteral("hi".into())),
            var_decl("y", true, Expression::StringLiteral("yo".into())),
            var_decl("a", false, shared_ref("x")),
            var_decl("b", false, shared_ref("x")),
            stmt(Statement::Echo {
                expr: var("a"),
                escapes: false,
            }),
            stmt(Statement::Echo {
                expr: var("b"),
                escapes: false,
            }),
            expr_stmt(Expression::FnCall {
                callee: Box::new(var("update")),
                args: vec![mut_ref("y")],
            }),
        ]);
        let checker = check_program(&program);
        assert!(
            checker.ownership_findings.is_empty(),
            "well-typed borrowing program must be clean: {:?}",
            checker.ownership_findings
        );
    }

    // ---- Data-race detection on `spawn` (task 7.3, R14.6 → E0613) ----

    fn spawn_stmt(body: Vec<Stmt>) -> Stmt {
        stmt(Statement::Spawn { body })
    }

    fn add(left: Expression, right: Expression) -> Expression {
        Expression::BinaryOp {
            left: Box::new(left),
            op: BinaryOperator::Add,
            right: Box::new(right),
        }
    }

    #[test]
    fn spawn_writing_captured_binding_is_flagged_e0613() {
        // let counter = 0
        // spawn { counter = counter + 1 }   // read-modify-write of captured
        //                                    // state without synchronization
        let program = program_in_main(vec![
            var_decl("counter", true, Expression::IntLiteral(0)),
            spawn_stmt(vec![var_decl(
                "counter",
                true,
                add(var("counter"), Expression::IntLiteral(1)),
            )]),
        ]);
        let checker = check_program(&program);
        assert_eq!(
            checker.ownership_findings.len(),
            1,
            "expected one data-race finding: {:?}",
            checker.ownership_findings
        );
        assert_eq!(checker.ownership_findings[0].code, "E0613");
        assert!(checker.ownership_findings[0]
            .message
            .contains("counter"));
    }

    #[test]
    fn spawn_mutating_builtin_on_captured_binding_is_flagged_e0613() {
        // let items = [1]
        // spawn { push(items, 2) }   // mutating builtin on captured binding
        let program = program_in_main(vec![
            var_decl(
                "items",
                true,
                Expression::Array(vec![Expression::IntLiteral(1)]),
            ),
            spawn_stmt(vec![expr_stmt(Expression::FnCall {
                callee: Box::new(var("push")),
                args: vec![var("items"), Expression::IntLiteral(2)],
            })]),
        ]);
        let checker = check_program(&program);
        assert_eq!(
            checker.ownership_findings.len(),
            1,
            "expected one data-race finding: {:?}",
            checker.ownership_findings
        );
        assert_eq!(checker.ownership_findings[0].code, "E0613");
    }

    #[test]
    fn spawn_only_reading_captured_binding_has_no_finding() {
        // let counter = 0
        // spawn { echo counter }   // read-only capture is safe
        let program = program_in_main(vec![
            var_decl("counter", true, Expression::IntLiteral(0)),
            spawn_stmt(vec![stmt(Statement::Echo {
                expr: var("counter"),
                escapes: false,
            })]),
        ]);
        let checker = check_program(&program);
        assert!(
            checker.ownership_findings.is_empty(),
            "read-only capture must not be flagged: {:?}",
            checker.ownership_findings
        );
    }

    #[test]
    fn spawn_mutating_only_its_own_locals_has_no_finding() {
        // spawn { let local = 0; local = local + 1 }   // body-local, not shared
        let program = program_in_main(vec![spawn_stmt(vec![
            var_decl("local", true, Expression::IntLiteral(0)),
            var_decl("local", true, add(var("local"), Expression::IntLiteral(1))),
        ])]);
        let checker = check_program(&program);
        assert!(
            checker.ownership_findings.is_empty(),
            "writes to body-locals must not be flagged: {:?}",
            checker.ownership_findings
        );
    }

    #[test]
    fn spawn_writing_through_synchronized_handle_has_no_finding() {
        // let counter = conc.shared(0)
        // spawn { conc.set(counter, 5) }   // write goes through `conc.*`
        //                                  // synchronization → no E0613
        let program = program_in_main(vec![
            var_decl(
                "counter",
                true,
                Expression::MethodCall {
                    object: Box::new(var("conc")),
                    method: "shared".into(),
                    args: vec![Expression::IntLiteral(0)],
                },
            ),
            spawn_stmt(vec![expr_stmt(Expression::MethodCall {
                object: Box::new(var("conc")),
                method: "set".into(),
                args: vec![var("counter"), Expression::IntLiteral(5)],
            })]),
        ]);
        let checker = check_program(&program);
        assert!(
            checker.ownership_findings.is_empty(),
            "synchronized write through `conc.*` must not be flagged: {:?}",
            checker.ownership_findings
        );
    }

    #[test]
    fn spawn_local_shadowing_captured_name_is_not_flagged() {
        // let counter = 0
        // spawn { let counter = 99; counter = counter + 1 }
        // The body declares a fresh local `counter`; subsequent writes target
        // the local, not the captured binding, so nothing is flagged.
        let program = program_in_main(vec![
            var_decl("counter", true, Expression::IntLiteral(0)),
            spawn_stmt(vec![
                var_decl("counter", true, Expression::IntLiteral(99)),
                var_decl("counter", true, add(var("counter"), Expression::IntLiteral(1))),
            ]),
        ]);
        let checker = check_program(&program);
        assert!(
            checker.ownership_findings.is_empty(),
            "a body-local shadowing an outer name must not be flagged: {:?}",
            checker.ownership_findings
        );
    }

    // ---- Phase B constructs accepted without false positives (task 10.5, R8.7) ----

    /// Build a `Param` with no type annotation.
    fn param(name: &str, is_mut: bool) -> Param {
        Param {
            name: name.into(),
            type_annotation: None,
            is_mut,
        }
    }

    /// A valid program exercising a closure (capturing a non-Copy outer binding
    /// by read), `break`/`continue` inside a loop, and a `match`-arm `return`
    /// must pass with NO ownership findings: capturing a variable in a closure
    /// is a read/borrow, not a move, so the outer binding stays usable (R8.7).
    #[test]
    fn closure_break_continue_match_return_program_is_clean() {
        // fn main() {
        //   let prefix = "p"
        //   let f = fn(x) { return prefix }   // captures `prefix` by read
        //   echo prefix                        // still usable -> no E0210
        //   for i in range(0, 5) {
        //     if i { continue }
        //     if i { break }
        //   }
        //   match 0 { _ => { return 0 } }       // match-arm return traversed
        // }
        let closure = Expression::Lambda {
            params: vec![param("x", false)],
            body: vec![stmt(Statement::Return(Some(var("prefix"))))],
        };
        let loop_body = vec![
            stmt(Statement::If {
                condition: var("i"),
                then_body: vec![stmt(Statement::Continue)],
                else_body: None,
            }),
            stmt(Statement::If {
                condition: var("i"),
                then_body: vec![stmt(Statement::Break)],
                else_body: None,
            }),
        ];
        let match_stmt = expr_stmt(Expression::Match {
            subject: Box::new(Expression::IntLiteral(0)),
            arms: vec![MatchArm {
                pattern: Pattern::Wildcard,
                body: vec![stmt(Statement::Return(Some(Expression::IntLiteral(0))))],
            }],
        });
        let program = program_in_main(vec![
            var_decl("prefix", false, Expression::StringLiteral("p".into())),
            var_decl("f", false, closure),
            stmt(Statement::Echo {
                expr: var("prefix"),
                escapes: false,
            }),
            stmt(Statement::For {
                variable: "i".into(),
                iterable: Expression::FnCall {
                    callee: Box::new(var("range")),
                    args: vec![Expression::IntLiteral(0), Expression::IntLiteral(5)],
                },
                body: loop_body,
            }),
            match_stmt,
        ]);
        let checker = check_program(&program);
        assert!(
            checker.ownership_findings.is_empty(),
            "valid closure/break/continue/match-return program must be clean: {:?}",
            checker.ownership_findings
        );
    }

    /// A trait declaration with a default method body plus an `impl Trait for
    /// Type` block whose method body is well-formed must pass with NO ownership
    /// findings (R8.6/R8.7). Method bodies are analyzed like normal function
    /// bodies (each with its params, including `self`, bound).
    #[test]
    fn trait_and_impl_method_bodies_are_clean() {
        // trait Greeter { fn greet(self) -> str { return "hi" } }
        // impl Greeter for Dog { fn greet(self) -> str { let s = "woof"; return s } }
        let trait_method = stmt(Statement::FnDecl {
            name: "greet".into(),
            params: vec![param("self", false)],
            return_type: None,
            body: vec![stmt(Statement::Return(Some(Expression::StringLiteral(
                "hi".into(),
            ))))],
            is_pub: false,
            is_async: false,
        });
        let impl_method = stmt(Statement::FnDecl {
            name: "greet".into(),
            params: vec![param("self", false)],
            return_type: None,
            body: vec![
                var_decl("s", false, Expression::StringLiteral("woof".into())),
                stmt(Statement::Return(Some(var("s")))),
            ],
            is_pub: false,
            is_async: false,
        });
        let program = Program {
            statements: vec![
                stmt(Statement::TraitDecl {
                    name: "Greeter".into(),
                    methods: vec![trait_method],
                    is_pub: false,
                }),
                stmt(Statement::ImplBlock {
                    type_name: "Dog".into(),
                    trait_name: Some("Greeter".into()),
                    methods: vec![impl_method],
                }),
            ],
        };
        let checker = check_program(&program);
        assert!(
            checker.ownership_findings.is_empty(),
            "well-formed trait/impl method bodies must be clean: {:?}",
            checker.ownership_findings
        );
    }

    /// A genuine use-after-move *inside* a closure body is still detected
    /// (priority: catch real violations, not just skip the body). The closure's
    /// own local `s` is moved into `t`, then read again -> E0210.
    #[test]
    fn use_after_move_inside_closure_body_is_flagged_e0210() {
        // fn main() { let f = fn() { let s = "x"; let t = s; echo s } }
        let closure = Expression::Lambda {
            params: vec![],
            body: vec![
                var_decl("s", false, Expression::StringLiteral("x".into())),
                var_decl("t", false, var("s")),
                stmt(Statement::Echo {
                    expr: var("s"),
                    escapes: false,
                }),
            ],
        };
        let program = program_in_main(vec![var_decl("f", false, closure)]);
        let checker = check_program(&program);
        assert_eq!(
            checker.ownership_findings.len(),
            1,
            "expected one use-after-move finding inside the closure body: {:?}",
            checker.ownership_findings
        );
        assert_eq!(checker.ownership_findings[0].code, "E0210");
    }
}

// ============================================================================
// Property 8 — Soundness of move/borrow/data-race checking (task 7.5).
//
// Property-based test mapping Correctness Property 8 from design.md. It drives
// the real `TypeChecker` ownership/borrow/data-race analysis over two generator
// streams and asserts the soundness contract:
//
//   * Stream A (well-typed): programs known to be ownership-safe must be
//     accepted — `TypeChecker::check` produces NO ownership findings.
//   * Stream B (ill-typed): programs that deliberately violate exactly one rule
//     must be rejected — the corresponding diagnostic code
//     (E0210/E0212/E0214/E0215/E0613) appears among the findings (the analyzer
//     would abort in `--ownership=strict`).
//
// Programs are built directly as `Program`/`Stmt`/`Expression` AST values
// (mirroring the in-source unit tests above) to keep generation precise and
// independent of the parser. Uses the std-only PBT harness in
// `crate::support::pbt` (seedable RNG, ≥100 cases via `RAN_PBT_CASES`, seed
// printed on failure for reproduction).
// ============================================================================
#[cfg(test)]
mod ownership_soundness_property {
    // Feature: enterprise-runtime-capabilities, Property 8: Soundness move/borrow/data-race
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};

    /// A generated test case: an AST program plus its expected oracle outcome.
    /// `expected == None` means the program must be clean (Stream A); `Some(code)`
    /// means the program must surface that ownership diagnostic (Stream B).
    #[derive(Clone, Debug)]
    struct Case {
        program: Program,
        expected: Option<&'static str>,
        label: &'static str,
    }

    // ---- Identifier pool (kept disjoint from the literal callee/function names
    //      used below: `peek`, `greet`, `consume`, `conc`, `push`, `echo`, `f`,
    //      `main`, `set`) so randomized binding names never collide. ----
    const NAMES: &[&str] = &[
        "a", "b", "d", "e", "g", "h", "k", "n", "o", "p", "q", "r", "s", "t", "u",
        "v", "w", "x", "y", "z", "val", "tmp", "data", "item", "acc", "buf", "obj",
        "num", "cnt", "elem",
    ];

    /// Pick `k` distinct identifiers from the pool.
    fn distinct_names(rng: &mut Rng, k: usize) -> Vec<String> {
        let mut pool: Vec<&str> = NAMES.to_vec();
        let mut out = Vec::with_capacity(k);
        for _ in 0..k {
            let i = rng.below(pool.len() as u64) as usize;
            out.push(pool.remove(i).to_string());
        }
        out
    }

    /// A non-`Copy` initializer (str or array) — subject to move tracking.
    fn non_copy_value(rng: &mut Rng) -> Expression {
        if rng.boolean() {
            let len = rng.upto(5);
            let mut s = String::new();
            for _ in 0..len {
                s.push((b'a' + rng.below(26) as u8) as char);
            }
            Expression::StringLiteral(s)
        } else {
            let len = rng.upto(4);
            let elems = (0..len)
                .map(|_| Expression::IntLiteral(rng.range_i64(0, 100)))
                .collect();
            Expression::Array(elems)
        }
    }

    /// A `Copy` initializer (int/float/bool) — duplicated, never moved.
    fn copy_value(rng: &mut Rng) -> Expression {
        match rng.below(3) {
            0 => Expression::IntLiteral(rng.range_i64(-100, 100)),
            1 => Expression::FloatLiteral(rng.unit_f64() * 100.0),
            _ => Expression::BoolLiteral(rng.boolean()),
        }
    }

    // ---- Tiny AST constructors (local to this module). ----

    fn at() -> Span {
        Span::new(1, 1)
    }
    fn stmt(kind: Statement) -> Stmt {
        Stmt::new(kind, at())
    }
    fn var(n: &str) -> Expression {
        Expression::Variable(n.into())
    }
    fn var_decl(name: &str, mutable: bool, value: Expression) -> Stmt {
        stmt(Statement::VarDecl {
            name: name.into(),
            mutable,
            is_decl: true,
            type_annotation: None,
            value,
        })
    }
    fn echo(expr: Expression) -> Stmt {
        stmt(Statement::Echo {
            expr,
            escapes: false,
        })
    }
    fn expr_stmt(expr: Expression) -> Stmt {
        stmt(Statement::Expr(expr))
    }
    fn call(callee: &str, args: Vec<Expression>) -> Expression {
        Expression::FnCall {
            callee: Box::new(var(callee)),
            args,
        }
    }
    fn shared_ref(n: &str) -> Expression {
        Expression::UnaryOp {
            op: UnaryOperator::Ref,
            operand: Box::new(var(n)),
        }
    }
    fn mut_ref(n: &str) -> Expression {
        Expression::UnaryOp {
            op: UnaryOperator::MutRef,
            operand: Box::new(var(n)),
        }
    }
    fn add(left: Expression, right: Expression) -> Expression {
        Expression::BinaryOp {
            left: Box::new(left),
            op: BinaryOperator::Add,
            right: Box::new(right),
        }
    }
    fn conc_method(method: &str, args: Vec<Expression>) -> Expression {
        Expression::MethodCall {
            object: Box::new(var("conc")),
            method: method.into(),
            args,
        }
    }
    fn spawn(body: Vec<Stmt>) -> Stmt {
        stmt(Statement::Spawn { body })
    }
    fn fn_decl(name: &str, params: Vec<Param>, body: Vec<Stmt>) -> Stmt {
        stmt(Statement::FnDecl {
            name: name.into(),
            params,
            return_type: None,
            body,
            is_pub: false,
            is_async: false,
        })
    }
    fn program_in_main(body: Vec<Stmt>) -> Program {
        Program {
            statements: vec![fn_decl("main", vec![], body)],
        }
    }

    /// Run the real checker and return its ownership findings.
    fn findings_of(program: &Program) -> Vec<OwnershipFinding> {
        let mut checker = TypeChecker::new();
        checker.check(program);
        checker.ownership_findings
    }

    // ---- Stream A: well-typed (ownership-safe) program generator. ----
    fn gen_safe(rng: &mut Rng) -> Case {
        match rng.below(8) {
            // (A0) Copy-typed reuse: `let n = <copy>; let m = n; echo n` — no move.
            0 => {
                let ns = distinct_names(rng, 2);
                Case {
                    program: program_in_main(vec![
                        var_decl(&ns[0], false, copy_value(rng)),
                        var_decl(&ns[1], false, var(&ns[0])),
                        echo(var(&ns[0])),
                    ]),
                    expected: None,
                    label: "copy-typed reuse",
                }
            }
            // (A1) Move with no later use: `let s = <noncopy>; let t = s` — clean.
            1 => {
                let ns = distinct_names(rng, 2);
                Case {
                    program: program_in_main(vec![
                        var_decl(&ns[0], false, non_copy_value(rng)),
                        var_decl(&ns[1], false, var(&ns[0])),
                    ]),
                    expected: None,
                    label: "move then no use",
                }
            }
            // (A2) Multiple shared borrows held simultaneously — allowed.
            2 => {
                let k = 2 + rng.below(3) as usize; // 2..=4 shared borrows
                let ns = distinct_names(rng, 1 + k);
                let owner = ns[0].clone();
                let mut body = vec![var_decl(&owner, false, non_copy_value(rng))];
                for b in ns.iter().skip(1) {
                    body.push(var_decl(b, false, shared_ref(&owner)));
                }
                for b in ns.iter().skip(1) {
                    body.push(echo(var(b)));
                }
                Case {
                    program: program_in_main(body),
                    expected: None,
                    label: "multiple shared borrows",
                }
            }
            // (A3) Fresh `&mut` after a transient `&` is released — allowed (R11.5).
            3 => {
                let ns = distinct_names(rng, 2);
                Case {
                    program: program_in_main(vec![
                        var_decl(&ns[0], true, non_copy_value(rng)),
                        expr_stmt(call("peek", vec![shared_ref(&ns[0])])),
                        var_decl(&ns[1], false, mut_ref(&ns[0])),
                    ]),
                    expected: None,
                    label: "fresh &mut after released borrow",
                }
            }
            // (A4) Read-only builtin does not move its argument.
            4 => {
                let ns = distinct_names(rng, 1);
                Case {
                    program: program_in_main(vec![
                        var_decl(&ns[0], false, non_copy_value(rng)),
                        expr_stmt(call("echo", vec![var(&ns[0])])),
                        echo(var(&ns[0])),
                    ]),
                    expected: None,
                    label: "read-only builtin no move",
                }
            }
            // (A5) `spawn` that only reads a captured binding — safe.
            5 => {
                let ns = distinct_names(rng, 1);
                Case {
                    program: program_in_main(vec![
                        var_decl(&ns[0], true, copy_value(rng)),
                        spawn(vec![echo(var(&ns[0]))]),
                    ]),
                    expected: None,
                    label: "spawn read-only capture",
                }
            }
            // (A6) `spawn` mutating only its own body-locals — safe.
            6 => {
                let ns = distinct_names(rng, 1);
                Case {
                    program: program_in_main(vec![spawn(vec![
                        var_decl(&ns[0], true, Expression::IntLiteral(0)),
                        var_decl(&ns[0], true, add(var(&ns[0]), Expression::IntLiteral(1))),
                    ])]),
                    expected: None,
                    label: "spawn body-local write",
                }
            }
            // (A7) `spawn` writing through a synchronized `conc.*` handle — safe.
            _ => {
                let ns = distinct_names(rng, 1);
                Case {
                    program: program_in_main(vec![
                        var_decl(&ns[0], true, conc_method("shared", vec![Expression::IntLiteral(0)])),
                        spawn(vec![expr_stmt(conc_method(
                            "set",
                            vec![var(&ns[0]), Expression::IntLiteral(5)],
                        ))]),
                    ]),
                    expected: None,
                    label: "spawn synchronized write",
                }
            }
        }
    }

    // ---- Stream B: ill-typed program generator (exactly one violation each). ----
    fn gen_unsafe(rng: &mut Rng) -> Case {
        match rng.below(9) {
            // (B0) use-after-move: `let s = <noncopy>; let t = s; echo s` → E0210.
            0 => {
                let ns = distinct_names(rng, 2);
                Case {
                    program: program_in_main(vec![
                        var_decl(&ns[0], false, non_copy_value(rng)),
                        var_decl(&ns[1], false, var(&ns[0])),
                        echo(var(&ns[0])),
                    ]),
                    expected: Some("E0210"),
                    label: "use-after-move",
                }
            }
            // (B1) pass-by-value to a user fn then use → E0210.
            1 => {
                let ns = distinct_names(rng, 1);
                let consume = fn_decl(
                    "consume",
                    vec![Param {
                        name: "x".into(),
                        type_annotation: None,
                        is_mut: false,
                    }],
                    vec![],
                );
                let main = fn_decl(
                    "main",
                    vec![],
                    vec![
                        var_decl(&ns[0], false, non_copy_value(rng)),
                        expr_stmt(call("consume", vec![var(&ns[0])])),
                        echo(var(&ns[0])),
                    ],
                );
                Case {
                    program: Program {
                        statements: vec![consume, main],
                    },
                    expected: Some("E0210"),
                    label: "use-after-move via call",
                }
            }
            // (B2) borrow of a moved value → E0210.
            2 => {
                let ns = distinct_names(rng, 2);
                Case {
                    program: program_in_main(vec![
                        var_decl(&ns[0], false, non_copy_value(rng)),
                        var_decl(&ns[1], false, var(&ns[0])),
                        expr_stmt(call("greet", vec![shared_ref(&ns[0])])),
                    ]),
                    expected: Some("E0210"),
                    label: "borrow-after-move",
                }
            }
            // (B3) `&mut` while a `&` borrow is active → E0212.
            3 => {
                let ns = distinct_names(rng, 3);
                Case {
                    program: program_in_main(vec![
                        var_decl(&ns[0], true, non_copy_value(rng)),
                        var_decl(&ns[1], false, shared_ref(&ns[0])),
                        var_decl(&ns[2], false, mut_ref(&ns[0])),
                    ]),
                    expected: Some("E0212"),
                    label: "mut-while-shared",
                }
            }
            // (B4) `&` while a `&mut` borrow is active → E0212.
            4 => {
                let ns = distinct_names(rng, 3);
                Case {
                    program: program_in_main(vec![
                        var_decl(&ns[0], true, non_copy_value(rng)),
                        var_decl(&ns[1], false, mut_ref(&ns[0])),
                        var_decl(&ns[2], false, shared_ref(&ns[0])),
                    ]),
                    expected: Some("E0212"),
                    label: "shared-while-mut",
                }
            }
            // (B5) returning a reference to a local → E0214 (dangling).
            5 => {
                let ns = distinct_names(rng, 1);
                let f = fn_decl(
                    "f",
                    vec![],
                    vec![
                        var_decl(&ns[0], false, non_copy_value(rng)),
                        stmt(Statement::Return(Some(shared_ref(&ns[0])))),
                    ],
                );
                Case {
                    program: Program {
                        statements: vec![f],
                    },
                    expected: Some("E0214"),
                    label: "dangling return-of-local",
                }
            }
            // (B6) move while still borrowed → E0215.
            6 => {
                let ns = distinct_names(rng, 3);
                Case {
                    program: program_in_main(vec![
                        var_decl(&ns[0], false, non_copy_value(rng)),
                        var_decl(&ns[1], false, shared_ref(&ns[0])),
                        var_decl(&ns[2], false, var(&ns[0])),
                    ]),
                    expected: Some("E0215"),
                    label: "move-while-borrowed",
                }
            }
            // (B7) `spawn` read-modify-write of a captured binding → E0613.
            7 => {
                let ns = distinct_names(rng, 1);
                Case {
                    program: program_in_main(vec![
                        var_decl(&ns[0], true, copy_value(rng)),
                        spawn(vec![var_decl(
                            &ns[0],
                            true,
                            add(var(&ns[0]), Expression::IntLiteral(1)),
                        )]),
                    ]),
                    expected: Some("E0613"),
                    label: "spawn unsynchronized RMW",
                }
            }
            // (B8) `spawn` mutating builtin on a captured binding → E0613.
            _ => {
                let ns = distinct_names(rng, 1);
                Case {
                    program: program_in_main(vec![
                        var_decl(
                            &ns[0],
                            true,
                            Expression::Array(vec![Expression::IntLiteral(1)]),
                        ),
                        spawn(vec![expr_stmt(call(
                            "push",
                            vec![var(&ns[0]), Expression::IntLiteral(2)],
                        ))]),
                    ]),
                    expected: Some("E0613"),
                    label: "spawn unsynchronized push",
                }
            }
        }
    }

    fn safe_gen() -> Gen<Case> {
        Gen::new(|rng, _size| gen_safe(rng), |_| Vec::new())
    }
    fn unsafe_gen() -> Gen<Case> {
        Gen::new(|rng, _size| gen_unsafe(rng), |_| Vec::new())
    }

    /// Property 8: soundness of move/borrow/data-race checking.
    ///
    /// Validates Requirements 10.1, 10.2, 10.3, 11.1, 11.2, 11.3, 11.4, 11.5,
    /// 11.7, 11.8, 14.6.
    #[test]
    fn prop_soundness_move_borrow_data_race() {
        // Stream A — well-typed programs are accepted (no ownership findings).
        pbt::for_all("P8 well-typed accepted", &safe_gen(), |case: &Case| {
            findings_of(&case.program).is_empty()
        });

        // Stream B — ill-typed programs surface the expected diagnostic code.
        pbt::for_all("P8 ill-typed rejected", &unsafe_gen(), |case: &Case| {
            let want = case
                .expected
                .expect("Stream B case must carry an expected code");
            findings_of(&case.program).iter().any(|f| f.code == want)
        });
    }
}

// ============================================================================
// Property 15 — Ownership accepts the new core constructs without false
// positives (R8.7).
//
// Unlike the AST-built `ownership_soundness_property` module above, this module
// exercises the *full front-end path*: each case is generated as Ran **source
// text** from a correct-by-construction template (closures with capture,
// `break`/`continue` inside loops, `match`-arm `return`, and `trait` + `impl
// Trait for Type`), then lexed + parsed and run through the `TypeChecker` in
// STRICT mode (default `TypeChecker::new()`, i.e. findings are not downgraded
// to warnings). Because every generated program is genuinely well-formed and
// ownership-safe, the strict checker MUST report ZERO ownership violations —
// any finding is a false positive and fails the property.
//
// Generation varies binding/type/method names, literal values, loop bounds,
// arm counts, and capture arity, while only ever *reading* or *borrowing*
// captured variables (never moving, never using-after-move), so the input
// space stays inside "valid program" territory by construction.
//
// Uses the std-only PBT harness in `crate::support::pbt` (≥100 cases via
// `RAN_PBT_CASES`, seedable, prints the failing source + seed on failure).
// ============================================================================
#[cfg(test)]
mod ownership_accepts_new_constructs_property {
    // Feature: memory-safe-self-hosting, Property 15: Ownership accepts new
    // constructs (closures with capture, break/continue, match-arm return,
    // trait + impl Trait for Type) under --ownership=strict without false
    // positives.
    use super::*;
    use crate::frontend::lexer::tokenize;
    use crate::frontend::parser::parse_checked;
    use crate::support::pbt::{self, Gen, Rng};

    /// A generated well-formed program (Ran source text) plus a human label
    /// identifying the template it came from. The program is guaranteed
    /// ownership-safe by construction; the property asserts the strict checker
    /// reports no violations on it.
    #[derive(Clone, Debug)]
    struct Case {
        source: String,
        label: &'static str,
    }

    // Lower-case binding names: two letters can never collide with a keyword.
    const LOWER: &[&str] = &[
        "aa", "bb", "cc", "dd", "ee", "gg", "hh", "ii", "jj", "kk", "mm", "nn",
        "oo", "pp", "qq", "rr", "ss", "tt", "uu", "vv", "ww", "xx", "yy", "zz",
    ];
    // Upper-case names for trait / struct / method targets (identifiers).
    const UPPER: &[&str] = &[
        "Aa", "Bb", "Cc", "Dd", "Ee", "Ff", "Gg", "Hh", "Ii", "Jj", "Kk", "Ll",
    ];

    /// Pick `k` distinct names from `pool`.
    fn distinct<'a>(rng: &mut Rng, pool: &[&'a str], k: usize) -> Vec<&'a str> {
        let mut p: Vec<&str> = pool.to_vec();
        let mut out = Vec::with_capacity(k);
        for _ in 0..k {
            let i = rng.below(p.len() as u64) as usize;
            out.push(p.remove(i));
        }
        out
    }

    fn small_int(rng: &mut Rng) -> i64 {
        rng.range_i64(0, 99)
    }

    // ---- Template 0: closure with capture (read-only capture). ----
    // `let base = N; let f = fn(x) { return x + base }; let out = f(M); echo out`
    // The closure reads captured ints (Copy) — never moves them.
    fn gen_closure_capture(rng: &mut Rng) -> Case {
        let ns = distinct(rng, LOWER, 4);
        let (base, f, p, out) = (ns[0], ns[1], ns[2], ns[3]);
        // Optionally capture a second variable to vary capture arity.
        let mut src = String::from("fn main() {\n");
        src.push_str(&format!("    let {} = {}\n", base, small_int(rng)));
        if rng.boolean() {
            let extra = distinct(rng, LOWER, 1)[0];
            // Avoid clashing with the four already chosen (distinct() draws from
            // a fresh pool, so guard by skipping when equal).
            if extra != base && extra != f && extra != p && extra != out {
                src.push_str(&format!("    let {} = {}\n", extra, small_int(rng)));
                src.push_str(&format!(
                    "    let {} = fn({}) {{ return {} + {} + {} }}\n",
                    f, p, p, base, extra
                ));
            } else {
                src.push_str(&format!(
                    "    let {} = fn({}) {{ return {} + {} }}\n",
                    f, p, p, base
                ));
            }
        } else {
            src.push_str(&format!(
                "    let {} = fn({}) {{ return {} + {} }}\n",
                f, p, p, base
            ));
        }
        src.push_str(&format!("    let {} = {}({})\n", out, f, small_int(rng)));
        src.push_str(&format!("    echo {}\n", out));
        src.push_str("}\n");
        Case { source: src, label: "closure-capture" }
    }

    // ---- Template 1: break/continue inside a `for` loop. ----
    fn gen_for_break_continue(rng: &mut Rng) -> Case {
        let ns = distinct(rng, LOWER, 2);
        let (acc, i) = (ns[0], ns[1]);
        let len = 3 + rng.below(4) as usize; // 3..=6 elements
        let mut elems = Vec::with_capacity(len);
        for k in 0..len {
            elems.push((k as i64 + 1).to_string());
        }
        let cont_at = 1 + rng.below(2) as i64;
        let break_at = (len as i64) + 1; // never actually hit → loop runs fully
        let mut src = String::from("fn main() {\n");
        src.push_str(&format!("    let mut {} = 0\n", acc));
        src.push_str(&format!("    for {} in [{}] {{\n", i, elems.join(", ")));
        src.push_str(&format!("        if {} == {} {{ continue }}\n", i, cont_at));
        src.push_str(&format!("        if {} == {} {{ break }}\n", i, break_at));
        src.push_str(&format!("        {} = {} + {}\n", acc, acc, i));
        src.push_str("    }\n");
        src.push_str(&format!("    echo {}\n", acc));
        src.push_str("}\n");
        Case { source: src, label: "for-break-continue" }
    }

    // ---- Template 2: break/continue inside a `while` loop. ----
    fn gen_while_break_continue(rng: &mut Rng) -> Case {
        let ns = distinct(rng, LOWER, 2);
        let (cnt, total) = (ns[0], ns[1]);
        let limit = 5 + rng.below(8) as i64; // 5..=12
        let cont_at = 1 + rng.below(2) as i64;
        let break_at = limit + 5; // not reached → terminates via condition
        let mut src = String::from("fn main() {\n");
        src.push_str(&format!("    let mut {} = 0\n", cnt));
        src.push_str(&format!("    let mut {} = 0\n", total));
        src.push_str(&format!("    while {} < {} {{\n", cnt, limit));
        src.push_str(&format!("        {} = {} + 1\n", cnt, cnt));
        src.push_str(&format!("        if {} == {} {{ continue }}\n", cnt, cont_at));
        src.push_str(&format!("        if {} == {} {{ break }}\n", cnt, break_at));
        src.push_str(&format!("        {} = {} + {}\n", total, total, cnt));
        src.push_str("    }\n");
        src.push_str(&format!("    echo {}\n", total));
        src.push_str("}\n");
        Case { source: src, label: "while-break-continue" }
    }

    // ---- Template 3: `return` from inside a `match` arm. ----
    fn gen_match_arm_return(rng: &mut Rng) -> Case {
        let func = distinct(rng, LOWER, 1)[0];
        let param = distinct(rng, LOWER, 1)[0];
        let res = distinct(rng, LOWER, 1)[0];
        let arms = 2 + rng.below(3) as i64; // 2..=4 literal arms + wildcard
        let mut src = String::new();
        src.push_str(&format!("fn {}({}) -> int {{\n", func, param));
        src.push_str(&format!("    match {} {{\n", param));
        for a in 0..arms {
            src.push_str(&format!("        {} => {{ return {} }}\n", a, small_int(rng)));
        }
        src.push_str(&format!("        _ => {{ return {} }}\n", small_int(rng)));
        src.push_str("    }\n}\n");
        src.push_str("fn main() {\n");
        src.push_str(&format!("    let {} = {}({})\n", res, func, rng.below(arms as u64 + 2)));
        src.push_str(&format!("    echo {}\n", res));
        src.push_str("}\n");
        Case { source: src, label: "match-arm-return" }
    }

    // ---- Template 4: trait + `impl Trait for Type` + method dispatch. ----
    fn gen_trait_impl(rng: &mut Rng) -> Case {
        let tr = distinct(rng, UPPER, 1)[0];
        let st = distinct(rng, UPPER, 1)[0];
        let method = distinct(rng, LOWER, 1)[0];
        let field = distinct(rng, LOWER, 1)[0];
        let obj = distinct(rng, LOWER, 1)[0];
        let res = distinct(rng, LOWER, 1)[0];
        let field_val = small_int(rng);
        let mut src = String::new();
        src.push_str(&format!("trait {} {{ fn {}(self) -> int }}\n", tr, method));
        src.push_str(&format!("struct {} {{ {}: int }}\n", st, field));
        src.push_str(&format!(
            "impl {} for {} {{ fn {}(self) -> int {{ return self.{} }} }}\n",
            tr, st, method, field
        ));
        src.push_str("fn main() {\n");
        src.push_str(&format!("    let {} = {} {{ {}: {} }}\n", obj, st, field, field_val));
        src.push_str(&format!("    let {} = {}.{}()\n", res, obj, method));
        src.push_str(&format!("    echo {}\n", res));
        src.push_str("}\n");
        Case { source: src, label: "trait-impl-dispatch" }
    }

    fn gen_case(rng: &mut Rng) -> Case {
        match rng.below(5) {
            0 => gen_closure_capture(rng),
            1 => gen_for_break_continue(rng),
            2 => gen_while_break_continue(rng),
            3 => gen_match_arm_return(rng),
            _ => gen_trait_impl(rng),
        }
    }

    fn case_gen() -> Gen<Case> {
        Gen::new(|rng, _size| gen_case(rng), |_| Vec::new())
    }

    /// Lex + parse + strict-check `source`. Returns `true` iff the program is
    /// well-formed (no syntax diagnostics) AND the strict ownership checker
    /// reports zero violations (no false positives).
    fn accepted_in_strict_mode(source: &str) -> bool {
        let tokens = tokenize(source);
        let (program, diags) = parse_checked(tokens);
        // A generation bug (malformed source) would also fail the property and
        // is surfaced via the printed counterexample; well-formed is part of
        // the precondition we assert here.
        if !diags.is_empty() {
            return false;
        }
        // Default checker == strict mode (findings are NOT downgraded to
        // warnings), mirroring `--ownership=strict`.
        let mut checker = TypeChecker::new();
        assert!(!checker.downgrade_ownership_to_warning, "must be strict mode");
        checker.check(&program);
        checker.ownership_findings.is_empty()
    }

    /// Property 15: well-typed programs using the new core constructs are
    /// accepted under `--ownership=strict` with no false ownership violations.
    ///
    /// Validates: Requirements 8.7
    #[test]
    fn prop_ownership_accepts_new_constructs_no_false_positives() {
        pbt::for_all(
            "P15 strict-mode accepts new constructs",
            &case_gen(),
            |case: &Case| accepted_in_strict_mode(&case.source),
        );
    }
}
