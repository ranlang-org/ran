# `math` — Numeric Functions

```ran
import "std::math" as math
```

| Call | Returns | Description |
|------|---------|-------------|
| `math.abs(x)` | int/float | Absolute value (preserves type) |
| `math.max(a, b)` | int/float | Larger of two |
| `math.min(a, b)` | int/float | Smaller of two |
| `math.sqrt(x)` | float | Square root |
| `math.pow(base, exp)` | float | `base` raised to `exp` |
| `math.floor(x)` | int | Round down |
| `math.ceil(x)` | int | Round up |
| `math.round(x)` | int | Round to nearest |
| `math.sin(x)` / `math.cos(x)` / `math.tan(x)` | float | Trigonometry (radians) |
| `math.log(x)` | float | Natural log |
| `math.log10(x)` | float | Base-10 log |
| `math.pi()` | float | π |
| `math.e()` | float | Euler's number |

## Example

```ran
import "std::math" as math

fn main() {
    echo math.sqrt(144.0)       # 12
    echo math.max(3, 9)         # 9
    echo math.pow(2.0, 10.0)    # 1024
    let area = math.pi() * math.pow(2.0, 2.0)
    echo area
}
```

## Notes

- Integer arithmetic in Ran is checked: overflow aborts with `E1001` (see
  [errors.md](errors.md)). Use floats for very large magnitudes.
- Trig functions operate in radians.
