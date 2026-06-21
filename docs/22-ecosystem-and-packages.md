# Ecosystem, Modules & Package Management

This page documents how code reuse works in Ran **today**, what is **missing**,
and a concrete **recommendation** for a package and dependency system suited to
internal use.

Ran is an internal-use language. The ecosystem design below optimizes for
privacy, auditability, and reproducibility over public package discovery.

---

## 1. What works today

### Standard library
Built into the runtime. Imported with the mandatory `std::` prefix and an alias:

```ran
import "std::http" as http
import "std::decimal" as decimal
```

There is no version to manage — the stdlib ships with the `ran` binary.

### Local modules (source reuse)
Any `.ran` file can be imported by path. One file is one module; its top-level
declarations merge into the importing program.

```ran
import "./lib/users" as users        # ./lib/users.ran
import "../shared/money.ran" as money # parent paths allowed
import "billing" as billing          # searched in ., ./lib, ./modules
```

- Relative paths (`./`, `../`) resolve from the importing file's directory.
- A bare name is searched in `.`, `./lib`, `./modules`.
- A directory may expose `mod.ran` as its entry.
- Import cycles are detected and de-duplicated.

This covers reuse **within a single repository** completely.

### Project manifest
`ran.toml` records the entry point and (auto-managed) standard-library usage:

```toml
[project]
name = "shopapp"
version = "0.1.0"

[build]
entry = "src/main.ran"

[dependencies]
# auto-filled by `ran build` from std:: imports
http = "std"
decimal = "std"
```

`ran.toml` is optional for single files and is created/updated by `ran build`
only when a project actually uses dependencies.

---

## 2. What is missing

| Capability | Status |
|------------|--------|
| Third-party (cross-repo) libraries | **Not supported** |
| A dependency resolver / lockfile | Not supported |
| Versioned releases of a library | Not supported |
| A central registry or index | Not supported |
| Fetching code from a remote source | Not supported (`ran get` is planned) |
| Vendoring / offline reproducible builds | Not supported |
| Capability/permission model for libraries | Not supported |

Today, to share code across repositories you copy files in or use a git
submodule and import by path. That is acceptable for a small number of internal
projects but does not scale.

---

## 3. Recommended dependency model

Two established models exist for distributing libraries:

1. **Source-from-version-control (git-native).** Dependencies are git
   repositories pinned to a tag/commit. No central registry. Simple to host
   privately. Best fit for an internal language.
2. **Central registry.** A server hosts published, versioned packages. Better
   discovery, more infrastructure to run and secure.

**Recommendation: git-native first.** It needs no registry server, works with
private/internal git hosts out of the box, and is straightforward to make
reproducible. A registry can come later if the library count grows.

### Proposed manifest

```toml
[dependencies]
# internal git dependency, pinned to an immutable commit
router  = { git = "https://git.internal/you/ran-router", rev = "a1b2c3d" }
# or a tag
money   = { git = "https://git.internal/you/ran-money", tag = "v1.2.0" }
# a vendored local path
charts  = { path = "vendor/ran-charts" }
```

### Proposed commands

```
ran get                 # resolve + fetch all dependencies into ./.ran/pkg
ran get <git-url>       # add and fetch a single dependency
ran update              # re-resolve within constraints, refresh the lockfile
ran vendor              # copy all resolved deps into ./vendor for offline builds
```

### Resolution & layout

- Fetched sources cache under `./.ran/pkg/<name>@<rev>/` (gitignored).
- Imports reference dependencies by their manifest name:
  `import "router" as router`.
- A name in `[dependencies]` shadows the local search path, so dependency vs
  local module is unambiguous.

### Lockfile (reproducibility)

`ran.lock` records the exact resolved commit and a content hash per dependency:

```toml
[[package]]
name = "router"
git  = "https://git.internal/you/ran-router"
rev  = "a1b2c3d4e5f6..."
hash = "sha256:..."
```

Builds verify the hash before use. Commit `ran.lock` to get byte-identical
builds across machines and CI.

---

## 4. Ecosystem security

For a language that handles money, supply-chain integrity is non-negotiable.

| Risk | Mitigation in the proposed model |
|------|----------------------------------|
| A dependency changes under you | Pin by **commit**, not a moving branch |
| Tampered download | `ran.lock` content **hash** verified on fetch/build |
| Compromised upstream host | Prefer **private/internal** git; mirror externals |
| Unreviewed code execution at build | Ran libraries are pure `.ran` source — **no build scripts run**, unlike ecosystems that execute arbitrary build hooks |
| Transitive sprawl | Keep the tree shallow; `ran vendor` makes the full set auditable in-repo |
| Secrets in dependencies | Forbid embedding secrets; config comes from the `env` module at runtime |

Recommended internal policy:

1. **Vendor everything** (`ran vendor`) for production builds — no network at
   build time, the entire dependency set is reviewable in your repository.
2. **Pin by commit + hash**; never depend on a branch.
3. **Review dependencies** like first-party code; they run with full host
   capability (filesystem, network, env) — there is no sandbox yet.
4. **Mirror external sources** into your own git host so an upstream takedown or
   compromise cannot affect your builds.

### A note on capabilities (future)

Today any imported module has the same power as your own code (it can read
files, open sockets, read the environment). A future hardening step is a
**capability model** where a dependency must declare the host features it needs
(e.g. `fs`, `net`) and is denied the rest. Until that exists, treat every
dependency as fully trusted and review accordingly.

---

## 5. Summary & roadmap

- **Now:** stdlib via `std::`, unlimited local/parent-path module reuse within a
  repo, optional auto-managed `ran.toml`.
- **Next:** git-native dependencies (`ran get`), a verified `ran.lock`, and
  `ran vendor` for offline reproducible builds.
- **Later:** an optional registry and a capability-based permission model for
  third-party code.

See `docs/16-roadmap.md` for overall status and `docs/21-dependency-policy.md`
for the toolchain's own (separate) zero-third-party-crate policy.
