# `fs` — Filesystem

```ran
import "std::fs" as fs
```

| Call | Returns | Description |
|------|---------|-------------|
| `fs.read(path)` | str | Read a file as UTF-8 text (`void` on error) |
| `fs.write(path, content)` | bool | Write/overwrite a file |
| `fs.append(path, content)` | bool | Append to a file (creates if missing) |
| `fs.exists(path)` | bool | Path exists |
| `fs.is_file(path)` | bool | Path is a regular file |
| `fs.is_dir(path)` | bool | Path is a directory |
| `fs.readdir(path)` | array | File/dir names in a directory |
| `fs.mkdir(path)` | bool | Create directory (recursive) |
| `fs.remove(path)` | bool | Delete a file |
| `fs.size(path)` | int | File size in bytes (`-1` on error) |
| `fs.copy(from, to)` | bool | Copy a file |
| `fs.rename(from, to)` | bool | Move/rename a file |

## Example

```ran
import "std::fs" as fs
import "std::log" as log

fn main() {
    if !fs.exists("data") {
        fs.mkdir("data")
    }
    fs.write("data/hello.txt", "hi")
    fs.append("data/hello.txt", "\nmore")
    let bytes = fs.size("data/hello.txt")
    log.info("wrote bytes:", bytes)
    for name in fs.readdir("data") {
        echo name
    }
}
```

## Notes

- Read/write errors are reported to stderr; check the boolean/`void` return.
- Paths are resolved relative to the process working directory
  (`os.cwd()`).
