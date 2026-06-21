# `decimal` — Exact Money & Business Math

> **Use `decimal` for anything involving money.** Never use `float` for
> currency. Binary floating point cannot represent values like `0.10` exactly,
> so `0.1 + 0.2` becomes `0.30000000000000004`. `decimal` is exact.

Ran's `decimal` is a base-10 fixed-point type: an integer mantissa scaled by a
power of ten, backed by a 128-bit integer (~38 significant digits). Addition,
subtraction, and multiplication are **always exact**; only division and explicit
rounding apply a rounding mode you choose.

## Creating decimals

```ran
let price = dec("19.99")        # concise builtin, no import needed
```

or via the module:

```ran
import "std::decimal" as decimal
let price = decimal.new("19.99")
let qty   = decimal.from(3)     # from an int
```

`dec(...)` / `decimal.new(...)` accept a numeric string, an int, or a float.
Strings are preferred for literals because they are exact (`dec("0.1")` is
exactly 1/10; `dec(0.1)` goes through the float's text form).

## Operators

`+`, `-`, `*` are exact. `==`, `!=`, `<`, `<=`, `>`, `>=` compare by value
(`dec("1.50") == dec("1.5")` is `true`). Mixed `decimal`/`int` arithmetic
promotes the int losslessly.

```ran
let total = dec("19.99") * dec("3") + dec("0.03")   # 60.00 exactly
```

`/` on decimals uses a **default** of half-up rounding at `max(scale, 2)`
places. For money, prefer explicit division (below) so the scale and rounding
are never a surprise.

## Methods & module functions

| Call | Description |
|------|-------------|
| `decimal.new(x)` / `dec(x)` | Construct a decimal |
| `decimal.add(a, b)` / `a.add(b)` | Exact addition |
| `decimal.sub(a, b)` / `a.sub(b)` | Exact subtraction |
| `decimal.mul(a, b)` / `a.mul(b)` | Exact multiplication |
| `decimal.div(a, b, scale, mode)` / `a.div(b, scale, mode)` | Division with explicit scale & rounding |
| `decimal.round(a, scale, mode)` / `a.round(scale, mode)` | Round to `scale` places |
| `decimal.cmp(a, b)` / `a.cmp(b)` | `-1`, `0`, or `1` |
| `decimal.abs(a)` / `a.abs()` | Absolute value |
| `decimal.neg(a)` / `a.neg()` | Negate |
| `decimal.is_zero(a)` / `a.is_zero()` | Zero test |
| `a.scale()` | Number of fractional digits |
| `a.to_str()` | String form |
| `a.to_int()` | Truncate to int |
| `a.to_float()` | Convert to float (lossy — avoid for money) |

## Rounding modes

Pass the mode as a string (case-insensitive). Default is `half_up`.

| Mode | Behavior | `2.5` → | `-2.5` → |
|------|----------|---------|----------|
| `half_up` | Half away from zero (common for invoices) | `3` | `-3` |
| `half_even` | Banker's rounding (reduces bias) | `2` | `-2` |
| `down` | Truncate toward zero | `2` | `-2` |
| `up` | Away from zero | `3` | `-3` |
| `floor` | Toward −∞ | `2` | `-3` |
| `ceiling` | Toward +∞ | `3` | `-2` |

Use `half_even` for large aggregations (statistically unbiased); use `half_up`
for per-invoice rounding where local accounting rules require it.

## Worked example: invoice with tax

```ran
import "std::decimal" as decimal

fn main() {
    let price    = dec("19.99")
    let qty      = dec("3")
    let subtotal = price * qty                                  # 59.97 exact
    let tax      = decimal.round(subtotal * dec("0.11"), 2, "half_up")  # 6.60
    let total    = subtotal + tax                               # 66.57

    echo "subtotal: " + subtotal
    echo "tax:      " + tax
    echo "total:    " + total

    # Split the bill three ways, rounded to cents.
    let share = decimal.div(total, dec("3"), 2, "half_up")      # 22.19
    echo "per person: " + share
}
```

## Safety guarantees

- **Exactness:** `+`, `-`, `*` never lose precision.
- **No silent overflow:** if a result exceeds 128-bit precision, the program
  aborts with `E1003` instead of producing a wrong number.
- **No divide-by-zero:** decimal `/` by zero aborts with `E1002`.
- **Parse errors are loud:** `dec("abc")` aborts with `E1004`.

## JSON

`json.encode` / `json.pretty` emit decimals as **unquoted JSON numbers** with
their exact digits (e.g. `66.57`), preserving precision on the wire.

```ran
import "std::json" as json
let m = map()
set(m, "total", dec("66.57"))
echo json.encode(m)        # {"total":66.57}
```

## When to use what

| Need | Type |
|------|------|
| Money, prices, tax, balances, rates | `decimal` |
| Counts, indexes, IDs | `int` |
| Scientific / approximate, trigonometry | `float` |
