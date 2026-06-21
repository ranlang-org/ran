# Interop and the Ran Ecosystem

This page explains how Ran reuses code and talks to existing native libraries.
For the full module system and the planned package manager, see
[22 - Ecosystem & Packages](22-ecosystem-and-packages.md).

## Reusing Ran code today

Within a repository, reuse is by file import (one file = one module):

```ran
import "./lib/users" as users
import "../shared/money.ran" as money
import "billing" as billing      # searched in ., ./lib, ./modules
```

Standard-library modules use the `std::` prefix:

```ran
import "std::http" as http
```

This fully covers code reuse inside a single project. Cross-repository
dependencies (a package manager) are planned — see chapter 22.

## Talking to native libraries

Ran's core promise is a **single self-contained binary**. It does not embed
another managed runtime or pull in foreign module systems, because that would
break the one-binary, no-external-toolchain model.

The intended path to existing native code is a thin **C-ABI FFI layer**: Ran
calls into a system shared library through a small, explicit binding (the same
mechanism the runtime already uses to link the system TLS library). This keeps
Ran in control and adds no managed runtime.

```
  Ran program  ->  Ran stdlib (FFI binding)  ->  system shared library (.so)
```

This is already how secure connections work: the HTTPS client links the system
TLS library directly. The same approach can expose other vetted system
libraries as stdlib modules over time.

## Summary

| Question | Answer |
|----------|--------|
| Reuse Ran code across files | Yes — local and parent-path imports |
| Reuse Ran code across repos | Planned — git-native packages (chapter 22) |
| Call system native libraries | Via a C-ABI FFI binding exposed as a stdlib module |
| Embed a foreign managed runtime | No — it breaks the single-binary model |

See [22 - Ecosystem & Packages](22-ecosystem-and-packages.md) for the dependency
and security model.
