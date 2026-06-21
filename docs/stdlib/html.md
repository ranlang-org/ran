# `html` — Template Rendering

```ran
import "std::html" as html
```

| Call | Returns | Description |
|------|---------|-------------|
| `html.render(template)` | str | Interpolate `$variables` into a template string |

`html.render` performs the same `$var` / `${var}` interpolation used elsewhere
in Ran, substituting variables that are in scope.

## Example

```ran
import "std::html" as html

fn page() -> str {
    let title = "Dashboard"
    let user = "Risqi"
    return html.render("<h1>$title</h1><p>Welcome, $user</p>")
}

fn main() {
    echo page()
}
```

## ⚠️ No automatic escaping

`html.render` does **not** HTML-escape values. Never interpolate untrusted input
(request bodies, query params) directly into markup without escaping it first,
or you risk XSS. Escape user data before rendering, or build responses from
trusted values only. Auto-escaping is a roadmap item.

For serving full pages and assets, prefer static files under `public/` (see
[http.md](http.md)).
