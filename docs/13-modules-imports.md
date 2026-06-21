# Modules & Imports

Ran organizes code with a simple model: the standard library is exposed as built-in
modules, and you can split your own code across local files with `import`.

## Stdlib modules (import with a mandatory alias)

The standard library modules are built in, but you must **import each one with a
mandatory alias** before using it. The alias is the name you call methods on; by
convention, alias a module to its own name:

```ran
import "std::time" as time
import "std::fs" as fs
import "std::json" as json

fn main() {
    time.sleep(100)                 # time module
    let exists = fs.exists("a.txt") # fs module
    let now = time.now()            # time module
    echo json.encode([1, 2, 3])     # json module
}
```

Two rules to remember:

- Using a stdlib module without importing it is
  `error[E0001]: undefined variable or module: 'time'`.
- Importing a stdlib module without an alias is
  `error[E0005]: stdlib import 'time' requires an alias`.

The alias can be any identifier. `import "std::http" as srv` lets you call `srv.server(...)`,
but aliasing to the same name (`import "std::http" as http`) keeps code readable.

The built-in modules are:

| Module | Purpose | Chapter |
|--------|---------|---------|
| `http` | HTTP server and routing | [07 - Networking](07-networking.md) |
| `web` | Native HTML/CSS/asset serving | [stdlib/web.md](stdlib/web.md) |
| `db` | Embedded SQLite database | [stdlib/database.md](stdlib/database.md) |
| `concurrency` | Threads, channels, wait groups, shared state | [06 - Concurrency](06-concurrency.md) |
| `decimal` | Exact money & business math | [stdlib/decimal.md](stdlib/decimal.md) |
| `crypto` | Hashing (SHA-256, HMAC) & encoding | [stdlib/crypto.md](stdlib/crypto.md) |
| `env` | Environment & `.env` configuration | [stdlib/env.md](stdlib/env.md) |
| `log` | Leveled structured logging | [stdlib/log.md](stdlib/log.md) |
| `time` | Sleep and timestamps | [10 - Standard Library](10-stdlib.md) |
| `fs` | File system read/write and directory ops | [10 - Standard Library](10-stdlib.md) |
| `json` | Encode/decode JSON (full) | [10 - Standard Library](10-stdlib.md) |
| `os` | Args, environment, cwd, platform, exit | [10 - Standard Library](10-stdlib.md) |
| `math` | abs, max, min, sqrt, pow, trig (int and float) | [10 - Standard Library](10-stdlib.md) |
| `html` | Variable interpolation | [10 - Standard Library](10-stdlib.md) |
| `str` | String helper functions | [10 - Standard Library](10-stdlib.md) |
| `rand` | Random numbers (non-cryptographic) | [10 - Standard Library](10-stdlib.md) |

> The names `net`, `sync`, `fmt`, `hardware`, `io`, and `regex` are **not**
> importable stdlib modules; importing one reports `module 'name' not found`.
> (`hardware` remains library-only - see [08 - Hardware](08-hardware.md).)

## Importing local files

You can split code across files and pull one in with `import`. A local import loads the
target file, parses it, and **merges its declarations into one flat global namespace**.
That means you call imported functions directly - there is no module prefix.

`examples/modules/mathlib.ran`:

```ran
# A reusable module
pub fn square(n: int) -> int {
    return n * n
}

pub fn cube(n: int) -> int {
    return n * n * n
}
```

`examples/modules/app.ran`:

```ran
import "./mathlib"

fn main() {
    let a = square(5)     # called directly - no "mathlib." prefix
    let b = cube(3)
    echo "square(5) = $a"
    echo "cube(3) = $b"
}
```

```bash
ran app.ran
# square(5) = 25
# cube(3) = 27
```

Import cycles are guarded, so two files importing each other will not loop forever.

## How imports are resolved

The resolver handles three kinds of import path:

1. **Stdlib names with an alias** - `import "std::http" as http`, `import "std::time" as time`,
   etc. The alias is **mandatory**: the bare form `import "std::http"` is `error[E0005]`.
   The runtime handles these modules natively; you call methods on the alias.
2. **Relative paths** - `import "./mathlib"` resolves to `mathlib.ran` next to the
   importing file. A directory form (`./utils` -> `./utils/mod.ran`) is also tried.
   Local imports take **no alias** - their declarations merge into the flat namespace.
3. **Bare names** - `import "utils"` is looked up in the search paths, in order:
   - `.` (current directory)
   - `./lib`
   - `./modules`
   So `import "utils"` resolves to `./utils.ran`, then `./lib/utils.ran`, then
   `./modules/utils.ran`. Like relative imports, bare-name local imports take no alias.

> Status note: **remote packages are not supported yet.** An import whose path contains
> a slash and looks like a package (for example `import "github.com/user/pkg"`) prints
> `ran: remote packages not yet supported` and is skipped. There is no package manager
> today, and `ran.toml` dependencies are not read.

## Project layout for modules

`ran init` scaffolds a project with a `lib/` directory, which is one of the search
paths:

```
myapp/
+-- ran.toml      # project manifest (not read yet)
+-- main.ran      # entry point
+-- lib/          # local modules (a search path)
+-- public/       # static files for web apps
```

Put reusable code in `lib/` and import it by bare name, or keep modules next to your
entry point and import them with `./name`.

## A practical multi-file example

```
project/
+-- main.ran
+-- lib/
    +-- users.ran
```

`lib/users.ran`:

```ran
import "std::fs" as fs

pub fn load_users() -> str {
    if fs.exists("users.json") {
        return fs.read("users.json")
    }
    return "[]"
}
```

`main.ran`:

```ran
import "std::http" as http
import "users"          # resolved via the ./lib search path

fn handle_users() -> str {
    return load_users()
}

fn main() {
    http.get("/api/users", "handle_users")
    http.server(8080)
}
```

## Tips and gotchas

- **Import stdlib modules with an alias.** Write `import "std::fs" as fs` before calling
  `fs.read(...)`. The bare form `import "std::fs"` is `error[E0005]`, and skipping the
  import entirely is `error[E0001]`.
- **Local imports take no alias.** `import "./mathlib"` merges declarations into a flat
  namespace; call imported functions directly, with no prefix. Avoid name collisions.
- **Relative imports are relative to the importing file**, not your current directory.
- **Bare names search `.`, then `./lib`, then `./modules`.**
- **Remote packages do not work yet.**

Next: [Security](14-security.md).
