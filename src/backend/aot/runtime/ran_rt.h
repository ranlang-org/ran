/* ran_rt.h — minimal C runtime for Ran native AOT codegen (Phase D, iteration D2).
 *
 * D1 was deliberately monomorphic: the supported subset (int, bool, str,
 * control flow, recursion, echo) was unboxed to plain C types. D2 keeps that
 * unboxed fast path — a variable the analyzer proves is a plain `int`/`bool`/
 * `float`/`str` is still a raw `int64_t`/`bool`/`double`/`const char*`, with no
 * tag and no refcount — and adds a tagged `RanValue` model for the data-type
 * layer that needs it: exact `decimal`, arrays, and struct/object values.
 *
 * Value mapping (D2):
 *     Ran int     -> int64_t        (unboxed)
 *     Ran bool    -> bool           (unboxed)
 *     Ran float   -> double         (unboxed)
 *     Ran str     -> const char*    (unboxed; concat/format results are heap)
 *     Ran decimal -> RanValue (RAN_DEC, inline i128 mantissa+scale, POD)
 *     Ran array   -> RanValue (RAN_ARRAY, reference-counted heap payload)
 *     Ran struct  -> RanValue (RAN_OBJECT, reference-counted heap payload)
 *
 * Memory model: heap payloads (string/array/object) carry an *atomic* reference
 * count. `ran_retain`/`ran_release` adjust it; releasing the last reference
 * frees the payload, recursively releasing array elements and object fields.
 * `RAN_INT`/`RAN_FLOAT`/`RAN_BOOL`/`RAN_DEC`/`RAN_VOID` are inline (POD) and
 * their retain/release are no-ops. Generated code follows a simple discipline:
 * a variable owns one reference (retained on store, released on reassign and at
 * scope/function exit); operations *borrow* their operands; per-statement
 * temporaries are released at the end of the statement. This keeps the model
 * free of double-free / use-after-free.
 *
 * Safety parity with the interpreter is carried over: checked integer
 * arithmetic raises E1010 on overflow and E1011 on divide/modulo-by-zero;
 * exact decimal overflow raises E1003 and decimal divide-by-zero E1002; an
 * out-of-bounds index raises E1012. All exit with code 70, the same code the
 * interpreter uses when a runtime fault reaches the top-level catch boundary.
 *
 * Portability: ISO C11 plus the compiler builtins/`__int128` already relied on
 * by D1 (`__builtin_*_overflow`); standard library only, no third-party code.
 */
#ifndef RAN_RT_H
#define RAN_RT_H

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>

/* ====================================================================== */
/* D1 unboxed scalar helpers (unchanged contract).                        */
/* ====================================================================== */

/* Print `s` followed by a newline (mirrors the interpreter's `println!`). */
void ran_echo(const char *s);

/* Concatenate two C strings into a freshly heap-allocated string. */
const char *ran_concat(const char *a, const char *b);

/* Convert a scalar to its Ran display string (matches `format!("{}", v)`). */
const char *ran_int_to_str(int64_t n);
const char *ran_bool_to_str(bool b);
/* Shortest round-trippable, non-scientific rendering matching Rust's f64
 * `Display` (e.g. 10.0 -> "10", 4.75 -> "4.75", 0.1+0.2 -> "0.30000000000000004"). */
const char *ran_float_to_str(double x);

/* Apply bash-style `echo -e` whitespace escapes (\n \t \r) to a string. */
const char *ran_apply_escapes(const char *s);

/* Checked integer arithmetic. On overflow -> E1010; on /0 or %0 -> E1011. */
int64_t ran_checked_add(int64_t a, int64_t b);
int64_t ran_checked_sub(int64_t a, int64_t b);
int64_t ran_checked_mul(int64_t a, int64_t b);
int64_t ran_checked_div(int64_t a, int64_t b);
int64_t ran_checked_mod(int64_t a, int64_t b);

/* ====================================================================== */
/* D2 tagged value model.                                                 */
/* ====================================================================== */

typedef enum {
    RAN_VOID = 0,
    RAN_INT,
    RAN_FLOAT,
    RAN_BOOL,
    RAN_DEC,
    RAN_STR,
    RAN_ARRAY,
    RAN_OBJECT
} RanTag;

typedef struct RanValue  RanValue;
typedef struct RanStr    RanStr;
typedef struct RanArray  RanArray;
typedef struct RanObject RanObject;

struct RanValue {
    RanTag tag;
    union {
        int64_t i;
        double  f;
        bool    b;
        struct { __int128 mant; int32_t scale; } dec; /* RAN_DEC: inline POD */
        RanStr   *s;   /* RAN_STR    */
        RanArray *a;   /* RAN_ARRAY  */
        RanObject *o;  /* RAN_OBJECT */
    } u;
};

/* Rounding modes — order matches support/decimal.rs `Rounding`. */
typedef enum {
    RAN_ROUND_HALF_UP = 0,
    RAN_ROUND_HALF_EVEN,
    RAN_ROUND_DOWN,
    RAN_ROUND_UP,
    RAN_ROUND_FLOOR,
    RAN_ROUND_CEILING
} RanRounding;

/* ---- Reference counting (heap payloads only; scalars are no-ops). ----- */
void     ran_retain(RanValue v);
void     ran_release(RanValue v);
/* Retain `v` and return it (an owned copy, for storing into a variable). */
RanValue ran_clone(RanValue v);

/* ---- Scalar boxing into RanValue (inline; no allocation except str). -- */
RanValue ran_from_int(int64_t n);
RanValue ran_from_float(double f);
RanValue ran_from_bool(bool b);
RanValue ran_from_str(const char *s); /* copies into a refcounted RanStr   */
RanValue ran_void(void);

/* ---- Decimal (exact money / business math). --------------------------- */
/* Parse a decimal string ("-1234.56", "0.001", "42", underscores ignored).
 * On a malformed string -> E1004 (matching the interpreter's `make_decimal`). */
RanValue ran_dec_parse(const char *s);
RanValue ran_dec_from_int(int64_t n);
/* Construct directly from a pre-parsed signed mantissa string + scale (used by
 * codegen for `dec("literal")`, where Rust already validated the text). */
RanValue ran_dec_make(const char *mantissa_digits, int32_t scale);

/* ---- Arrays. ---------------------------------------------------------- */
RanValue ran_array_new(size_t cap);
/* Append `elem`, taking ownership of its reference (does NOT retain). */
void     ran_array_push(RanValue arr, RanValue elem);
/* Bounds-checked index (E1012 out of range). Returns an owned (+1) copy. */
RanValue ran_index(RanValue arr, int64_t i);

/* ---- Objects (structs). `names` is a static, codegen-owned array. ----- */
RanValue ran_object_new(const char *type_name, size_t n, const char *const *names);
/* Set field slot `idx`, taking ownership of `val` (does NOT retain). */
void     ran_object_set(RanValue obj, size_t idx, RanValue val);
/* Field access by name. Returns an owned (+1) copy; missing field -> void. */
RanValue ran_field(RanValue obj, const char *name);

/* String-interpolation dotted-path resolution. Walks the dot-separated `fields`
 * remainder (e.g. "owner", "address.city") starting from `base` (borrowed). On
 * full resolution returns the display string (heap) of the resolved value; if
 * any field is missing or `base` is not an object, returns `fallback` (the
 * literal "$path" text the interpreter leaves in place). Releases intermediates. */
const char *ran_interp_path(RanValue base, const char *fields, const char *fallback);

/* ---- Generic operations (replicate interpreter `eval_binary_op`). ----- */
/* These BORROW their operands (never release them) and return an owned value. */
RanValue ran_add(RanValue a, RanValue b);
RanValue ran_sub(RanValue a, RanValue b);
RanValue ran_mul(RanValue a, RanValue b);
RanValue ran_div(RanValue a, RanValue b);
RanValue ran_mod(RanValue a, RanValue b);
bool     ran_eq(RanValue a, RanValue b);
bool     ran_neq(RanValue a, RanValue b);
bool     ran_lt(RanValue a, RanValue b);
bool     ran_lte(RanValue a, RanValue b);
bool     ran_gt(RanValue a, RanValue b);
bool     ran_gte(RanValue a, RanValue b);
bool     ran_truthy(RanValue v);

/* `len(x)`: bytes for a string, element count for an array, else 0. */
int64_t  ran_len(RanValue v);

/* Display form matching `format!("{}", value)`. Returns a heap C string. */
const char *ran_value_to_str(RanValue v);

/* ====================================================================== */
/* D4a stdlib bridge — common modules implemented in C (libc/libm only).   */
/*                                                                        */
/* Every bridged function takes an argument vector `argv` of `argc`        */
/* already-evaluated RanValue arguments (the codegen boxes scalars), and   */
/* returns the module method's result in its native C type. The functions  */
/* BORROW `argv` (they never release it; the caller owns the temporaries). */
/* Deterministic functions match the interpreter byte-for-byte; the few    */
/* nondeterministic ones (time.*, rand.*, log timestamp, os.getpid/        */
/* hostname/args) match shape/format/type only — see ran_rt.c for notes.   */
/* ====================================================================== */

/* time */
int64_t     ran_mod_time_now(const RanValue *argv, int64_t argc);
int64_t     ran_mod_time_now_ms(const RanValue *argv, int64_t argc);
const char *ran_mod_time_now_iso(const RanValue *argv, int64_t argc);
void        ran_mod_time_sleep(const RanValue *argv, int64_t argc);

/* log (variadic; all void; fatal exits with code 1) */
void ran_mod_log_debug(const RanValue *argv, int64_t argc);
void ran_mod_log_info(const RanValue *argv, int64_t argc);
void ran_mod_log_warn(const RanValue *argv, int64_t argc);
void ran_mod_log_error(const RanValue *argv, int64_t argc);
void ran_mod_log_fatal(const RanValue *argv, int64_t argc);

/* math */
RanValue ran_mod_math_abs(const RanValue *argv, int64_t argc);
RanValue ran_mod_math_max(const RanValue *argv, int64_t argc);
RanValue ran_mod_math_min(const RanValue *argv, int64_t argc);
double   ran_mod_math_sqrt(const RanValue *argv, int64_t argc);
double   ran_mod_math_pow(const RanValue *argv, int64_t argc);
int64_t  ran_mod_math_floor(const RanValue *argv, int64_t argc);
int64_t  ran_mod_math_ceil(const RanValue *argv, int64_t argc);
int64_t  ran_mod_math_round(const RanValue *argv, int64_t argc);
double   ran_mod_math_sin(const RanValue *argv, int64_t argc);
double   ran_mod_math_cos(const RanValue *argv, int64_t argc);
double   ran_mod_math_tan(const RanValue *argv, int64_t argc);
double   ran_mod_math_log(const RanValue *argv, int64_t argc);
double   ran_mod_math_log10(const RanValue *argv, int64_t argc);
double   ran_mod_math_pi(const RanValue *argv, int64_t argc);
double   ran_mod_math_e(const RanValue *argv, int64_t argc);

/* str */
const char *ran_mod_str_from(const RanValue *argv, int64_t argc);
const char *ran_mod_str_upper(const RanValue *argv, int64_t argc);
const char *ran_mod_str_lower(const RanValue *argv, int64_t argc);
const char *ran_mod_str_trim(const RanValue *argv, int64_t argc);
int64_t     ran_mod_str_len(const RanValue *argv, int64_t argc);
bool        ran_mod_str_contains(const RanValue *argv, int64_t argc);
const char *ran_mod_str_replace(const RanValue *argv, int64_t argc);
RanValue    ran_mod_str_split(const RanValue *argv, int64_t argc);
const char *ran_mod_str_join(const RanValue *argv, int64_t argc);
bool        ran_mod_str_starts_with(const RanValue *argv, int64_t argc);
bool        ran_mod_str_ends_with(const RanValue *argv, int64_t argc);
int64_t     ran_mod_str_index_of(const RanValue *argv, int64_t argc);
const char *ran_mod_str_repeat(const RanValue *argv, int64_t argc);
const char *ran_mod_str_reverse(const RanValue *argv, int64_t argc);
const char *ran_mod_str_trim_start(const RanValue *argv, int64_t argc);
const char *ran_mod_str_trim_end(const RanValue *argv, int64_t argc);
const char *ran_mod_str_pad_left(const RanValue *argv, int64_t argc);
const char *ran_mod_str_pad_right(const RanValue *argv, int64_t argc);
int64_t     ran_mod_str_to_int(const RanValue *argv, int64_t argc);
double      ran_mod_str_to_float(const RanValue *argv, int64_t argc);

/* os */
const char *ran_mod_os_platform(const RanValue *argv, int64_t argc);
const char *ran_mod_os_arch(const RanValue *argv, int64_t argc);
const char *ran_mod_os_cwd(const RanValue *argv, int64_t argc);
const char *ran_mod_os_hostname(const RanValue *argv, int64_t argc);
const char *ran_mod_os_env_or(const RanValue *argv, int64_t argc);
int64_t     ran_mod_os_getpid(const RanValue *argv, int64_t argc);
int64_t     ran_mod_os_cpu_count(const RanValue *argv, int64_t argc);
RanValue    ran_mod_os_env(const RanValue *argv, int64_t argc);
bool        ran_mod_os_setenv(const RanValue *argv, int64_t argc);
void        ran_mod_os_exit(const RanValue *argv, int64_t argc);
RanValue    ran_mod_os_args(const RanValue *argv, int64_t argc);

/* fs */
RanValue ran_mod_fs_read(const RanValue *argv, int64_t argc);
bool     ran_mod_fs_write(const RanValue *argv, int64_t argc);
bool     ran_mod_fs_exists(const RanValue *argv, int64_t argc);
bool     ran_mod_fs_append(const RanValue *argv, int64_t argc);
bool     ran_mod_fs_remove(const RanValue *argv, int64_t argc);
bool     ran_mod_fs_mkdir(const RanValue *argv, int64_t argc);
bool     ran_mod_fs_is_file(const RanValue *argv, int64_t argc);
bool     ran_mod_fs_is_dir(const RanValue *argv, int64_t argc);
RanValue ran_mod_fs_readdir(const RanValue *argv, int64_t argc);
int64_t  ran_mod_fs_size(const RanValue *argv, int64_t argc);
bool     ran_mod_fs_copy(const RanValue *argv, int64_t argc);
bool     ran_mod_fs_rename(const RanValue *argv, int64_t argc);

/* rand (nondeterministic; xorshift64 seeded like the interpreter) */
int64_t ran_mod_rand_int(const RanValue *argv, int64_t argc);
double  ran_mod_rand_float(const RanValue *argv, int64_t argc);
bool    ran_mod_rand_bool(const RanValue *argv, int64_t argc);

/* json (encode/stringify identical; pretty = indented) */
const char *ran_mod_json_encode(const RanValue *argv, int64_t argc);
const char *ran_mod_json_stringify(const RanValue *argv, int64_t argc);
const char *ran_mod_json_pretty(const RanValue *argv, int64_t argc);

#endif /* RAN_RT_H */
