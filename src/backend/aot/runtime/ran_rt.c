/* ran_rt.c — minimal C runtime for Ran native AOT codegen (Phase D, iteration D2).
 *
 * Precompiled once and linked into each native binary (it is NOT re-emitted per
 * program). The generated program calls into these helpers for echo, string
 * ops, the tagged `RanValue` data layer (decimal/array/object), checked
 * arithmetic, and value formatting.
 *
 * See ran_rt.h for the value model and safety contract.
 */
/* Enable POSIX/BSD APIs (clock_gettime, nanosleep, setenv, getcwd, readdir)
 * under -std=c11, which otherwise hides them. Must precede every #include. */
#ifndef _POSIX_C_SOURCE
#define _POSIX_C_SOURCE 200809L
#endif
#ifndef _DEFAULT_SOURCE
#define _DEFAULT_SOURCE 1
#endif
#include "ran_rt.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdatomic.h>

/* Abort with a Ran-style diagnostic and exit code 70 (the interpreter's
 * top-level fault exit code). */
static void ran_fault(const char *code, const char *message, const char *help) {
    /* Flush buffered stdout first so any already-echoed output appears before
     * the diagnostic, matching the interpreter's ordering when both streams are
     * captured together. */
    fflush(stdout);
    fprintf(stderr, "error[%s]: %s\n", code, message);
    fprintf(stderr, "  = help: %s\n", help);
    exit(70);
}

static void *ran_xalloc(size_t n) {
    void *p = malloc(n);
    if (!p) {
        ran_fault("E1006", "out of memory", "reduce memory pressure");
    }
    return p;
}

/* ====================================================================== */
/* D1 unboxed scalar helpers.                                             */
/* ====================================================================== */

void ran_echo(const char *s) {
    fputs(s ? s : "", stdout);
    fputc('\n', stdout);
}

const char *ran_concat(const char *a, const char *b) {
    if (!a) a = "";
    if (!b) b = "";
    size_t la = strlen(a);
    size_t lb = strlen(b);
    char *out = (char *)ran_xalloc(la + lb + 1);
    memcpy(out, a, la);
    memcpy(out + la, b, lb);
    out[la + lb] = '\0';
    return out;
}

const char *ran_int_to_str(int64_t n) {
    char buf[24];
    int len = snprintf(buf, sizeof(buf), "%lld", (long long)n);
    char *out = (char *)ran_xalloc((size_t)len + 1);
    memcpy(out, buf, (size_t)len + 1);
    return out;
}

const char *ran_bool_to_str(bool b) {
    return b ? "true" : "false";
}

/* Shortest round-trippable rendering matching Rust's f64 `Display`:
 *   - never uses scientific notation (always fixed decimal),
 *   - no trailing zeros, integral values have no ".0" (10.0 -> "10"),
 *   - special values: "NaN", "inf", "-inf".
 * Strategy: find the smallest number of significant digits whose `%.*e`
 * rendering round-trips through `strtod`, then reassemble fixed-notation. */
const char *ran_float_to_str(double x) {
    if (isnan(x)) { char *o = (char *)ran_xalloc(4); memcpy(o, "NaN", 4); return o; }
    if (isinf(x)) {
        const char *t = x < 0 ? "-inf" : "inf";
        size_t n = strlen(t) + 1; char *o = (char *)ran_xalloc(n); memcpy(o, t, n); return o;
    }
    if (x == 0.0) {
        /* Preserve sign of zero like Rust ("-0" for negative zero). */
        const char *t = signbit(x) ? "-0" : "0";
        size_t n = strlen(t) + 1; char *o = (char *)ran_xalloc(n); memcpy(o, t, n); return o;
    }

    char ebuf[64];
    int prec = 17;
    for (int p = 0; p <= 17; p++) {
        snprintf(ebuf, sizeof(ebuf), "%.*e", p, x);
        if (strtod(ebuf, NULL) == x) { prec = p; break; }
    }
    /* ebuf looks like "-1.2345e+02" or "5e-01". Split mantissa digits + exp. */
    const char *s = ebuf;
    bool neg = false;
    if (*s == '-') { neg = true; s++; }
    char digits[32];
    size_t nd = 0;
    /* first digit */
    if (*s >= '0' && *s <= '9') digits[nd++] = *s++;
    if (*s == '.') {
        s++;
        while (*s >= '0' && *s <= '9') digits[nd++] = *s++;
    }
    int exp = 0;
    if (*s == 'e' || *s == 'E') {
        s++;
        exp = (int)strtol(s, NULL, 10);
    }
    digits[nd] = '\0';
    /* Value = digits (as integer with nd digits) * 10^(exp - (nd-1)).
     * The decimal point sits after position (exp+1) counting from the first
     * significant digit. Build a fixed-notation string. */
    int point = exp + 1; /* number of digits before the decimal point */

    char out[512];
    size_t oi = 0;
    if (neg) out[oi++] = '-';

    if (point <= 0) {
        /* 0.00ddd */
        out[oi++] = '0';
        out[oi++] = '.';
        for (int z = 0; z < -point; z++) out[oi++] = '0';
        for (size_t k = 0; k < nd; k++) out[oi++] = digits[k];
    } else if ((size_t)point >= nd) {
        /* ddd000 (integral) */
        for (size_t k = 0; k < nd; k++) out[oi++] = digits[k];
        for (int z = 0; z < point - (int)nd; z++) out[oi++] = '0';
    } else {
        /* dd.ddd */
        for (int k = 0; k < point; k++) out[oi++] = digits[k];
        out[oi++] = '.';
        for (size_t k = (size_t)point; k < nd; k++) out[oi++] = digits[k];
    }
    out[oi] = '\0';

    char *res = (char *)ran_xalloc(oi + 1);
    memcpy(res, out, oi + 1);
    return res;
}

const char *ran_apply_escapes(const char *s) {
    if (!s) return "";
    size_t n = strlen(s);
    char *out = (char *)ran_xalloc(n + 1);
    size_t j = 0;
    for (size_t i = 0; i < n; i++) {
        if (s[i] == '\\' && i + 1 < n) {
            char nx = s[i + 1];
            if (nx == 'n') { out[j++] = '\n'; i++; continue; }
            if (nx == 't') { out[j++] = '\t'; i++; continue; }
            if (nx == 'r') { out[j++] = '\r'; i++; continue; }
        }
        out[j++] = s[i];
    }
    out[j] = '\0';
    return out;
}

int64_t ran_checked_add(int64_t a, int64_t b) {
    int64_t r;
    if (__builtin_add_overflow(a, b, &r)) {
        char msg[96];
        snprintf(msg, sizeof(msg), "integer overflow: %lld + %lld", (long long)a, (long long)b);
        ran_fault("E1010", msg,
                  "Operasi integer melebihi rentang (overflow). Gunakan nilai lebih kecil atau tipe decimal untuk perhitungan besar.");
    }
    return r;
}

int64_t ran_checked_sub(int64_t a, int64_t b) {
    int64_t r;
    if (__builtin_sub_overflow(a, b, &r)) {
        char msg[96];
        snprintf(msg, sizeof(msg), "integer overflow: %lld - %lld", (long long)a, (long long)b);
        ran_fault("E1010", msg,
                  "Operasi integer melebihi rentang (overflow). Gunakan nilai lebih kecil atau tipe decimal untuk perhitungan besar.");
    }
    return r;
}

int64_t ran_checked_mul(int64_t a, int64_t b) {
    int64_t r;
    if (__builtin_mul_overflow(a, b, &r)) {
        char msg[96];
        snprintf(msg, sizeof(msg), "integer overflow: %lld * %lld", (long long)a, (long long)b);
        ran_fault("E1010", msg,
                  "Operasi integer melebihi rentang (overflow). Gunakan nilai lebih kecil atau tipe decimal untuk perhitungan besar.");
    }
    return r;
}

int64_t ran_checked_div(int64_t a, int64_t b) {
    if (b == 0) {
        ran_fault("E1011", "division by zero",
                  "Pembagian/modulo dengan nol. Pastikan pembagi bukan nol sebelum operasi.");
    }
    if (a == INT64_MIN && b == -1) {
        ran_fault("E1010", "integer overflow: division overflow",
                  "Operasi integer melebihi rentang (overflow). Gunakan nilai lebih kecil atau tipe decimal untuk perhitungan besar.");
    }
    return a / b;
}

int64_t ran_checked_mod(int64_t a, int64_t b) {
    if (b == 0) {
        ran_fault("E1011", "modulo by zero",
                  "Pembagian/modulo dengan nol. Pastikan pembagi bukan nol sebelum operasi.");
    }
    if (a == INT64_MIN && b == -1) {
        return 0;
    }
    return a % b;
}

/* ====================================================================== */
/* D2 tagged value model — heap payloads + reference counting.            */
/* ====================================================================== */

struct RanStr {
    _Atomic long rc;
    size_t len;     /* byte length (excludes NUL) */
    char  *data;    /* NUL-terminated */
};

struct RanArray {
    _Atomic long rc;
    size_t len;
    size_t cap;
    RanValue *items;
};

struct RanObject {
    _Atomic long rc;
    const char *type_name;        /* static, codegen-owned */
    size_t n;
    const char *const *names;     /* static, codegen-owned (field names) */
    RanValue *vals;               /* owned (n slots) */
};

/* String-keyed dictionary. Entries are kept in INSERTION order (a parallel
 * keys/vals array); the interpreter's HashMap leaves order unspecified, so
 * whole-map traversal here is order-divergent but per-key access matches. */
struct RanMap {
    _Atomic long rc;
    size_t len;
    size_t cap;
    char    **keys;   /* owned, NUL-terminated copies */
    RanValue *vals;   /* owned */
};

void ran_retain(RanValue v) {
    switch (v.tag) {
        case RAN_STR:    atomic_fetch_add(&v.u.s->rc, 1); break;
        case RAN_ARRAY:  atomic_fetch_add(&v.u.a->rc, 1); break;
        case RAN_OBJECT: atomic_fetch_add(&v.u.o->rc, 1); break;
        case RAN_MAP:    atomic_fetch_add(&v.u.m->rc, 1); break;
        default: break;
    }
}

void ran_release(RanValue v) {
    switch (v.tag) {
        case RAN_STR:
            if (atomic_fetch_sub(&v.u.s->rc, 1) == 1) {
                free(v.u.s->data);
                free(v.u.s);
            }
            break;
        case RAN_ARRAY:
            if (atomic_fetch_sub(&v.u.a->rc, 1) == 1) {
                for (size_t i = 0; i < v.u.a->len; i++) ran_release(v.u.a->items[i]);
                free(v.u.a->items);
                free(v.u.a);
            }
            break;
        case RAN_OBJECT:
            if (atomic_fetch_sub(&v.u.o->rc, 1) == 1) {
                for (size_t i = 0; i < v.u.o->n; i++) ran_release(v.u.o->vals[i]);
                free(v.u.o->vals);
                free(v.u.o);
            }
            break;
        case RAN_MAP:
            if (atomic_fetch_sub(&v.u.m->rc, 1) == 1) {
                for (size_t i = 0; i < v.u.m->len; i++) {
                    free(v.u.m->keys[i]);
                    ran_release(v.u.m->vals[i]);
                }
                free(v.u.m->keys);
                free(v.u.m->vals);
                free(v.u.m);
            }
            break;
        default:
            break;
    }
}

RanValue ran_clone(RanValue v) {
    ran_retain(v);
    return v;
}

/* ---- Scalar boxing. --------------------------------------------------- */

RanValue ran_from_int(int64_t n)  { RanValue v; v.tag = RAN_INT;   v.u.i = n; return v; }
RanValue ran_from_float(double f) { RanValue v; v.tag = RAN_FLOAT; v.u.f = f; return v; }
RanValue ran_from_bool(bool b)    { RanValue v; v.tag = RAN_BOOL;  v.u.b = b; return v; }
RanValue ran_void(void)           { RanValue v; v.tag = RAN_VOID;  v.u.i = 0; return v; }

RanValue ran_from_str(const char *s) {
    if (!s) s = "";
    RanStr *p = (RanStr *)ran_xalloc(sizeof(RanStr));
    atomic_store(&p->rc, 1);
    p->len = strlen(s);
    p->data = (char *)ran_xalloc(p->len + 1);
    memcpy(p->data, s, p->len + 1);
    RanValue v; v.tag = RAN_STR; v.u.s = p;
    return v;
}

/* ====================================================================== */
/* Decimal — exact base-10 fixed point, ported from support/decimal.rs.   */
/* ====================================================================== */

typedef struct { __int128 mant; int32_t scale; } Dec;

static RanValue dec_value(__int128 mant, int32_t scale) {
    RanValue v; v.tag = RAN_DEC; v.u.dec.mant = mant; v.u.dec.scale = scale; return v;
}

static __int128 dec_pow10(int32_t n) {
    __int128 acc = 1;
    for (int32_t k = 0; k < n; k++) {
        __int128 next = acc * 10;
        if (next / 10 != acc) {
            ran_fault("E1003", "decimal overflow (scale too large)",
                      "decimal values exceed 128-bit precision");
        }
        acc = next;
    }
    return acc;
}

/* Round a non-negative magnitude q (remainder r over den) per `mode`. */
static __int128 round_magnitude(__int128 q, __int128 r, __int128 den,
                                RanRounding mode, bool negative) {
    if (r == 0) return q;
    /* twice = r*2, saturating semantics are irrelevant within our domain. */
    __int128 twice = r * 2;
    switch (mode) {
        case RAN_ROUND_DOWN:    return q;
        case RAN_ROUND_UP:      return q + 1;
        case RAN_ROUND_HALF_UP: return (twice >= den) ? q + 1 : q;
        case RAN_ROUND_HALF_EVEN:
            if (twice > den) return q + 1;
            if (twice < den) return q;
            return (q % 2 == 0) ? q : q + 1;
        case RAN_ROUND_FLOOR:   return negative ? q + 1 : q;
        case RAN_ROUND_CEILING: return negative ? q : q + 1;
    }
    return q;
}

static __int128 i128_abs(__int128 x) { return x < 0 ? -x : x; }

/* Rescale mantissa to `target` scale, rounding if reducing precision. */
static Dec dec_rescale(Dec d, int32_t target, RanRounding mode) {
    Dec out;
    if (target == d.scale) return d;
    if (target > d.scale) {
        __int128 factor = dec_pow10(target - d.scale);
        __int128 m = d.mant * factor;
        if (factor != 0 && m / factor != d.mant) {
            ran_fault("E1003", "decimal overflow", "decimal values exceed 128-bit precision");
        }
        out.mant = m; out.scale = target; return out;
    }
    __int128 factor = dec_pow10(d.scale - target);
    __int128 q = d.mant / factor;
    __int128 r = d.mant % factor;
    bool negative = r < 0;
    __int128 mag = round_magnitude(i128_abs(q), i128_abs(r), i128_abs(factor), mode, negative);
    out.mant = negative ? -mag : mag;
    out.scale = target;
    return out;
}

/* Align two decimals to a common (max) scale. */
static void dec_align(Dec a, Dec b, __int128 *am, __int128 *bm, int32_t *scale) {
    int32_t s = a.scale > b.scale ? a.scale : b.scale;
    *am = dec_rescale(a, s, RAN_ROUND_DOWN).mant;
    *bm = dec_rescale(b, s, RAN_ROUND_DOWN).mant;
    *scale = s;
}

static Dec dec_add(Dec a, Dec b) {
    __int128 am, bm; int32_t s;
    dec_align(a, b, &am, &bm, &s);
    __int128 m = am + bm;
    Dec out; out.mant = m; out.scale = s; return out;
}

static Dec dec_sub(Dec a, Dec b) {
    __int128 am, bm; int32_t s;
    dec_align(a, b, &am, &bm, &s);
    __int128 m = am - bm;
    Dec out; out.mant = m; out.scale = s; return out;
}

static Dec dec_mul(Dec a, Dec b) {
    __int128 m = a.mant * b.mant;
    if (a.mant != 0 && m / a.mant != b.mant) {
        ran_fault("E1003", "decimal overflow in multiplication",
                  "decimal values exceed 128-bit precision");
    }
    Dec out; out.mant = m; out.scale = a.scale + b.scale; return out;
}

static bool dec_is_zero(Dec a) { return a.mant == 0; }

static Dec dec_div(Dec a, Dec b, int32_t result_scale, RanRounding mode) {
    /* Caller guarantees b != 0. */
    long long e = (long long)result_scale + (long long)b.scale - (long long)a.scale;
    __int128 num, den;
    if (e >= 0) {
        __int128 factor = dec_pow10((int32_t)e);
        num = a.mant * factor;
        den = b.mant;
    } else {
        __int128 factor = dec_pow10((int32_t)(-e));
        num = a.mant;
        den = b.mant * factor;
    }
    int sign = ((num > 0) - (num < 0)) * ((den > 0) - (den < 0));
    __int128 num_abs = i128_abs(num);
    __int128 den_abs = i128_abs(den);
    __int128 q = num_abs / den_abs;
    __int128 r = num_abs % den_abs;
    __int128 mag = round_magnitude(q, r, den_abs, mode, sign < 0);
    Dec out; out.mant = (__int128)sign * mag; out.scale = result_scale; return out;
}

/* Compare two decimals: -1 / 0 / 1. */
static int dec_cmp(Dec a, Dec b) {
    __int128 am, bm; int32_t s;
    dec_align(a, b, &am, &bm, &s);
    if (am < bm) return -1;
    if (am > bm) return 1;
    return 0;
}

static double dec_to_f64(Dec a) {
    double divisor = pow(10.0, (double)a.scale);
    return (double)a.mant / divisor;
}

/* Render a decimal exactly like support/decimal.rs `Display`. */
static const char *dec_to_str(Dec d) {
    char tmp[64];
    /* Build the absolute mantissa decimal digits manually (no %lld for i128). */
    bool neg = d.mant < 0;
    __int128 m = i128_abs(d.mant);
    char digs[48];
    int nd = 0;
    if (m == 0) {
        digs[nd++] = '0';
    } else {
        while (m > 0) { digs[nd++] = (char)('0' + (int)(m % 10)); m /= 10; }
    }
    /* digs is reversed; reverse into `dstr`. */
    char dstr[48];
    for (int k = 0; k < nd; k++) dstr[k] = digs[nd - 1 - k];
    dstr[nd] = '\0';

    char out[80];
    size_t oi = 0;
    if (d.scale == 0) {
        if (neg) out[oi++] = '-';
        memcpy(out + oi, dstr, (size_t)nd); oi += nd;
        out[oi] = '\0';
    } else {
        int scale = d.scale;
        if (neg) out[oi++] = '-';
        if (nd <= scale) {
            out[oi++] = '0';
            out[oi++] = '.';
            for (int z = 0; z < scale - nd; z++) out[oi++] = '0';
            memcpy(out + oi, dstr, (size_t)nd); oi += nd;
        } else {
            int split = nd - scale;
            memcpy(out + oi, dstr, (size_t)split); oi += split;
            out[oi++] = '.';
            memcpy(out + oi, dstr + split, (size_t)scale); oi += scale;
        }
        out[oi] = '\0';
    }
    (void)tmp;
    char *res = (char *)ran_xalloc(oi + 1);
    memcpy(res, out, oi + 1);
    return res;
}

/* Parse a decimal string (mirrors Decimal::parse). On error -> E1004. */
static bool dec_parse_str(const char *s, Dec *out) {
    if (!s) return false;
    /* trim leading/trailing ASCII whitespace */
    while (*s == ' ' || *s == '\t' || *s == '\n' || *s == '\r') s++;
    size_t end = strlen(s);
    while (end > 0 && (s[end-1] == ' ' || s[end-1] == '\t' || s[end-1] == '\n' || s[end-1] == '\r')) end--;
    if (end == 0) return false;
    size_t i = 0;
    bool neg = false;
    if (s[i] == '-') { neg = true; i++; }
    else if (s[i] == '+') { i++; }
    __int128 mant = 0;
    int32_t scale = 0;
    bool seen_dot = false;
    bool any_digit = false;
    for (; i < end; i++) {
        char ch = s[i];
        if (ch >= '0' && ch <= '9') {
            mant = mant * 10 + (ch - '0');
            any_digit = true;
            if (seen_dot) scale++;
        } else if (ch == '.') {
            if (seen_dot) return false;
            seen_dot = true;
        } else if (ch == '_') {
            /* separator, ignore */
        } else {
            return false;
        }
    }
    if (!any_digit) return false;
    out->mant = neg ? -mant : mant;
    out->scale = scale;
    return true;
}

RanValue ran_dec_parse(const char *s) {
    Dec d;
    if (!dec_parse_str(s, &d)) {
        char msg[128];
        snprintf(msg, sizeof(msg), "invalid decimal `%s`", s ? s : "");
        ran_fault("E1004", msg, "use a numeric string like \"19.99\"");
    }
    return dec_value(d.mant, d.scale);
}

RanValue ran_dec_from_int(int64_t n) {
    return dec_value((__int128)n, 0);
}

RanValue ran_dec_make(const char *mantissa_digits, int32_t scale) {
    /* mantissa_digits is a plain signed integer string produced by codegen. */
    const char *s = mantissa_digits ? mantissa_digits : "0";
    bool neg = false;
    if (*s == '-') { neg = true; s++; }
    __int128 m = 0;
    for (; *s >= '0' && *s <= '9'; s++) m = m * 10 + (*s - '0');
    return dec_value(neg ? -m : m, scale);
}

/* Coerce any RanValue to a decimal (Int lossless; Float/Str via text). */
static bool to_decimal(RanValue v, Dec *out) {
    switch (v.tag) {
        case RAN_DEC:   out->mant = v.u.dec.mant; out->scale = v.u.dec.scale; return true;
        case RAN_INT:   out->mant = (__int128)v.u.i; out->scale = 0; return true;
        case RAN_FLOAT: {
            const char *t = ran_float_to_str(v.u.f);
            return dec_parse_str(t, out);
        }
        case RAN_STR:   return dec_parse_str(v.u.s->data, out);
        default:        return false;
    }
}

/* ====================================================================== */
/* Arrays.                                                                */
/* ====================================================================== */

RanValue ran_array_new(size_t cap) {
    RanArray *p = (RanArray *)ran_xalloc(sizeof(RanArray));
    atomic_store(&p->rc, 1);
    p->len = 0;
    p->cap = cap;
    p->items = cap ? (RanValue *)ran_xalloc(cap * sizeof(RanValue)) : NULL;
    RanValue v; v.tag = RAN_ARRAY; v.u.a = p;
    return v;
}

void ran_array_push(RanValue arr, RanValue elem) {
    RanArray *p = arr.u.a;
    if (p->len == p->cap) {
        size_t ncap = p->cap ? p->cap * 2 : 4;
        RanValue *ni = (RanValue *)ran_xalloc(ncap * sizeof(RanValue));
        if (p->items) {
            memcpy(ni, p->items, p->len * sizeof(RanValue));
            free(p->items);
        }
        p->items = ni;
        p->cap = ncap;
    }
    p->items[p->len++] = elem; /* takes ownership of elem's reference */
}

RanValue ran_index(RanValue arr, int64_t i) {
    if (arr.tag != RAN_ARRAY) {
        ran_fault("E1012", "indexing a non-array value",
                  "only arrays support [index] in native codegen");
    }
    RanArray *p = arr.u.a;
    if (i < 0 || (size_t)i >= p->len) {
        char msg[96];
        snprintf(msg, sizeof(msg), "index out of bounds: index %lld, length %zu",
                 (long long)i, p->len);
        ran_fault("E1012", msg, "ensure 0 <= index < length before indexing");
    }
    return ran_clone(p->items[i]);
}

/* ====================================================================== */
/* Objects (structs).                                                     */
/* ====================================================================== */

RanValue ran_object_new(const char *type_name, size_t n, const char *const *names) {
    RanObject *p = (RanObject *)ran_xalloc(sizeof(RanObject));
    atomic_store(&p->rc, 1);
    p->type_name = type_name;
    p->n = n;
    p->names = names;
    p->vals = n ? (RanValue *)ran_xalloc(n * sizeof(RanValue)) : NULL;
    for (size_t k = 0; k < n; k++) p->vals[k] = ran_void();
    RanValue v; v.tag = RAN_OBJECT; v.u.o = p;
    return v;
}

void ran_object_set(RanValue obj, size_t idx, RanValue val) {
    RanObject *p = obj.u.o;
    if (idx < p->n) {
        ran_release(p->vals[idx]);
        p->vals[idx] = val; /* takes ownership */
    } else {
        ran_release(val);
    }
}

RanValue ran_field(RanValue obj, const char *name) {
    if (obj.tag != RAN_OBJECT) return ran_void();
    RanObject *p = obj.u.o;
    for (size_t k = 0; k < p->n; k++) {
        if (strcmp(p->names[k], name) == 0) return ran_clone(p->vals[k]);
    }
    return ran_void();
}

/* ====================================================================== */
/* Maps (string-keyed dictionary; mirrors the interpreter's Value::Map).  */
/* Entries are kept in insertion order; the interpreter uses a HashMap, so */
/* iteration/display order is unspecified in BOTH engines — only per-key   */
/* access is deterministic (see ran_rt.h note).                            */
/* ====================================================================== */

RanValue ran_map_new(void) {
    RanMap *p = (RanMap *)ran_xalloc(sizeof(RanMap));
    atomic_store(&p->rc, 1);
    p->len = 0;
    p->cap = 0;
    p->keys = NULL;
    p->vals = NULL;
    RanValue v; v.tag = RAN_MAP; v.u.m = p;
    return v;
}

/* Find the slot index of `key`, or -1 if absent. */
static long ran_map_find(RanMap *p, const char *key) {
    for (size_t i = 0; i < p->len; i++) {
        if (strcmp(p->keys[i], key) == 0) return (long)i;
    }
    return -1;
}

static char *ran_strdup(const char *s) {
    if (!s) s = "";
    size_t n = strlen(s);
    char *out = (char *)ran_xalloc(n + 1);
    memcpy(out, s, n + 1);
    return out;
}

void ran_map_set(RanValue map, const char *key, RanValue val) {
    if (map.tag != RAN_MAP) { ran_release(val); return; }
    RanMap *p = map.u.m;
    long idx = ran_map_find(p, key);
    if (idx >= 0) {
        /* Overwrite: release the previous value, take ownership of the new. */
        ran_release(p->vals[idx]);
        p->vals[idx] = val;
        return;
    }
    if (p->len == p->cap) {
        size_t ncap = p->cap ? p->cap * 2 : 4;
        char **nk = (char **)ran_xalloc(ncap * sizeof(char *));
        RanValue *nv = (RanValue *)ran_xalloc(ncap * sizeof(RanValue));
        if (p->keys) { memcpy(nk, p->keys, p->len * sizeof(char *)); free(p->keys); }
        if (p->vals) { memcpy(nv, p->vals, p->len * sizeof(RanValue)); free(p->vals); }
        p->keys = nk;
        p->vals = nv;
        p->cap = ncap;
    }
    p->keys[p->len] = ran_strdup(key); /* key copied */
    p->vals[p->len] = val;             /* ownership taken */
    p->len++;
}

RanValue ran_map_get(RanValue map, const char *key) {
    if (map.tag != RAN_MAP) return ran_void();
    RanMap *p = map.u.m;
    long idx = ran_map_find(p, key);
    if (idx < 0) return ran_void();
    return ran_clone(p->vals[idx]);
}

RanValue ran_map_keys(RanValue map) {
    RanValue arr = ran_array_new(map.tag == RAN_MAP ? map.u.m->len : 0);
    if (map.tag == RAN_MAP) {
        for (size_t i = 0; i < map.u.m->len; i++) {
            ran_array_push(arr, ran_from_str(map.u.m->keys[i]));
        }
    }
    return arr;
}

RanValue ran_map_values(RanValue map) {
    RanValue arr = ran_array_new(map.tag == RAN_MAP ? map.u.m->len : 0);
    if (map.tag == RAN_MAP) {
        for (size_t i = 0; i < map.u.m->len; i++) {
            ran_array_push(arr, ran_clone(map.u.m->vals[i]));
        }
    }
    return arr;
}

/* String-interpolation dotted-path resolution (mirrors the interpreter's
 * `lookup_path` + the "unknown name left literal" fallback in
 * `interpolate_string`). `base` is borrowed (never released here). `fields` is
 * the dot-separated remainder after the base variable (e.g. "owner",
 * "address.city"). Each component is walked with `ran_field`, releasing the
 * intermediate value. If the full path resolves to a non-void value, its
 * display string (heap) is returned; if any field is missing or a non-object is
 * encountered, `fallback` — the literal "$path" text — is returned unchanged. */
const char *ran_interp_path(RanValue base, const char *fields, const char *fallback) {
    RanValue cur = ran_clone(base);
    const char *p = fields;
    while (*p) {
        const char *start = p;
        while (*p && *p != '.') p++;
        size_t len = (size_t)(p - start);
        char *fname = (char *)ran_xalloc(len + 1);
        memcpy(fname, start, len);
        fname[len] = '\0';
        RanValue next = ran_field(cur, fname);
        free(fname);
        ran_release(cur);
        cur = next;
        if (*p == '.') p++;
        if (cur.tag == RAN_VOID) {
            ran_release(cur);
            return fallback;
        }
    }
    const char *s = ran_value_to_str(cur);
    ran_release(cur);
    return s;
}

/* ====================================================================== */
/* Generic operations — replicate the interpreter's eval_binary_op.       */
/* ====================================================================== */

static double rv_as_f64(RanValue v) {
    switch (v.tag) {
        case RAN_INT:   return (double)v.u.i;
        case RAN_FLOAT: return v.u.f;
        case RAN_DEC:   return dec_to_f64((Dec){v.u.dec.mant, v.u.dec.scale});
        default:        return 0.0;
    }
}

static bool rv_is_num(RanValue v) {
    return v.tag == RAN_INT || v.tag == RAN_FLOAT;
}

/* Decimal binop helper for the operator path; `op` in {'+','-','*','/','%'}. */
static RanValue dec_binop(Dec l, char op, Dec r) {
    switch (op) {
        case '+': { Dec d = dec_add(l, r); return dec_value(d.mant, d.scale); }
        case '-': { Dec d = dec_sub(l, r); return dec_value(d.mant, d.scale); }
        case '*': { Dec d = dec_mul(l, r); return dec_value(d.mant, d.scale); }
        case '/': {
            if (dec_is_zero(r)) {
                ran_fault("E1002", "decimal division by zero", "guard the divisor before dividing");
            }
            int32_t scale = l.scale > r.scale ? l.scale : r.scale;
            if (scale < 2) scale = 2;
            Dec d = dec_div(l, r, scale, RAN_ROUND_HALF_UP);
            return dec_value(d.mant, d.scale);
        }
        case '%': {
            if (dec_is_zero(r)) {
                ran_fault("E1002", "decimal modulo by zero", "guard the divisor");
            }
            int32_t scale = l.scale > r.scale ? l.scale : r.scale;
            Dec q = dec_div(l, r, 0, RAN_ROUND_DOWN);
            Dec prod = dec_mul(q, r);
            Dec diff = dec_sub(l, prod);
            Dec d = dec_rescale(diff, scale, RAN_ROUND_HALF_UP);
            return dec_value(d.mant, d.scale);
        }
    }
    return ran_void();
}

/* Arithmetic dispatch shared by +,-,*,/,%. */
static RanValue arith(RanValue a, RanValue b, char op) {
    /* Exact decimal path first (matches interpreter). */
    if (a.tag == RAN_DEC || b.tag == RAN_DEC) {
        Dec l, r;
        if (to_decimal(a, &l) && to_decimal(b, &r)) {
            return dec_binop(l, op, r);
        }
    }
    /* Int op Int -> checked integer arithmetic. */
    if (a.tag == RAN_INT && b.tag == RAN_INT) {
        switch (op) {
            case '+': return ran_from_int(ran_checked_add(a.u.i, b.u.i));
            case '-': return ran_from_int(ran_checked_sub(a.u.i, b.u.i));
            case '*': return ran_from_int(ran_checked_mul(a.u.i, b.u.i));
            case '/': return ran_from_int(ran_checked_div(a.u.i, b.u.i));
            case '%': return ran_from_int(ran_checked_mod(a.u.i, b.u.i));
        }
    }
    /* Float / mixed int-float arithmetic. */
    if (rv_is_num(a) && rv_is_num(b)) {
        double l = rv_as_f64(a), r = rv_as_f64(b);
        switch (op) {
            case '+': return ran_from_float(l + r);
            case '-': return ran_from_float(l - r);
            case '*': return ran_from_float(l * r);
            case '/': return ran_from_float(l / r);
            case '%': return ran_from_float(fmod(l, r));
        }
    }
    /* String concatenation (only '+'). */
    if (op == '+') {
        if (a.tag == RAN_STR && b.tag == RAN_STR)
            return ran_from_str(ran_concat(a.u.s->data, b.u.s->data));
        if (a.tag == RAN_STR)
            return ran_from_str(ran_concat(a.u.s->data, ran_value_to_str(b)));
        if (b.tag == RAN_STR)
            return ran_from_str(ran_concat(ran_value_to_str(a), b.u.s->data));
    }
    return ran_void();
}

RanValue ran_add(RanValue a, RanValue b) { return arith(a, b, '+'); }
RanValue ran_sub(RanValue a, RanValue b) { return arith(a, b, '-'); }
RanValue ran_mul(RanValue a, RanValue b) { return arith(a, b, '*'); }
RanValue ran_div(RanValue a, RanValue b) { return arith(a, b, '/'); }
RanValue ran_mod(RanValue a, RanValue b) { return arith(a, b, '%'); }

/* Comparison dispatch. `kind`: 0=lt 1=lte 2=gt 3=gte. */
static bool cmp_order(RanValue a, RanValue b, int kind) {
    /* Decimal path. */
    if (a.tag == RAN_DEC || b.tag == RAN_DEC) {
        Dec l, r;
        if (to_decimal(a, &l) && to_decimal(b, &r)) {
            int c = dec_cmp(l, r);
            switch (kind) { case 0: return c < 0; case 1: return c <= 0;
                            case 2: return c > 0; default: return c >= 0; }
        }
    }
    if (a.tag == RAN_INT && b.tag == RAN_INT) {
        switch (kind) { case 0: return a.u.i < b.u.i; case 1: return a.u.i <= b.u.i;
                        case 2: return a.u.i > b.u.i; default: return a.u.i >= b.u.i; }
    }
    if (rv_is_num(a) && rv_is_num(b)) {
        double l = rv_as_f64(a), r = rv_as_f64(b);
        switch (kind) { case 0: return l < r; case 1: return l <= r;
                        case 2: return l > r; default: return l >= r; }
    }
    if (a.tag == RAN_STR && b.tag == RAN_STR) {
        int c = strcmp(a.u.s->data, b.u.s->data);
        switch (kind) { case 0: return c < 0; case 1: return c <= 0;
                        case 2: return c > 0; default: return c >= 0; }
    }
    return false;
}

bool ran_lt(RanValue a, RanValue b)  { return cmp_order(a, b, 0); }
bool ran_lte(RanValue a, RanValue b) { return cmp_order(a, b, 1); }
bool ran_gt(RanValue a, RanValue b)  { return cmp_order(a, b, 2); }
bool ran_gte(RanValue a, RanValue b) { return cmp_order(a, b, 3); }

bool ran_eq(RanValue a, RanValue b) {
    if (a.tag == RAN_DEC || b.tag == RAN_DEC) {
        Dec l, r;
        if (to_decimal(a, &l) && to_decimal(b, &r)) return dec_cmp(l, r) == 0;
    }
    if (a.tag == RAN_INT && b.tag == RAN_INT)   return a.u.i == b.u.i;
    if (rv_is_num(a) && rv_is_num(b))           return rv_as_f64(a) == rv_as_f64(b);
    if (a.tag == RAN_STR && b.tag == RAN_STR)   return strcmp(a.u.s->data, b.u.s->data) == 0;
    if (a.tag == RAN_BOOL && b.tag == RAN_BOOL) return a.u.b == b.u.b;
    return false;
}

bool ran_neq(RanValue a, RanValue b) {
    /* Mirror the interpreter: only defined for like types; otherwise false. */
    if (a.tag == RAN_DEC || b.tag == RAN_DEC) {
        Dec l, r;
        if (to_decimal(a, &l) && to_decimal(b, &r)) return dec_cmp(l, r) != 0;
    }
    if (a.tag == RAN_INT && b.tag == RAN_INT)   return a.u.i != b.u.i;
    if (rv_is_num(a) && rv_is_num(b))           return rv_as_f64(a) != rv_as_f64(b);
    if (a.tag == RAN_STR && b.tag == RAN_STR)   return strcmp(a.u.s->data, b.u.s->data) != 0;
    if (a.tag == RAN_BOOL && b.tag == RAN_BOOL) return a.u.b != b.u.b;
    return false;
}

bool ran_truthy(RanValue v) {
    switch (v.tag) {
        case RAN_BOOL:  return v.u.b;
        case RAN_INT:   return v.u.i != 0;
        case RAN_FLOAT: return v.u.f != 0.0;
        case RAN_DEC:   return v.u.dec.mant != 0;
        case RAN_STR:   return v.u.s->len != 0;
        case RAN_ARRAY: return v.u.a->len != 0;
        case RAN_VOID:  return false;
        /* RAN_MAP / RAN_OBJECT fall through to the default: the interpreter's
         * `is_truthy_val` returns `true` for a map/object regardless of size
         * (its `_ => true` arm), so an empty map is STILL truthy. */
        default:        return true;
    }
}

int64_t ran_len(RanValue v) {
    switch (v.tag) {
        case RAN_STR:   return (int64_t)v.u.s->len;
        case RAN_ARRAY: return (int64_t)v.u.a->len;
        case RAN_MAP:   return (int64_t)v.u.m->len;
        default:        return 0;
    }
}

/* ---- Display. --------------------------------------------------------- */

/* A small growable byte buffer for composing array/object display strings. */
typedef struct { char *buf; size_t len; size_t cap; } SB;

static void sb_init(SB *b) { b->cap = 32; b->len = 0; b->buf = (char *)ran_xalloc(b->cap); b->buf[0] = '\0'; }
static void sb_push(SB *b, const char *s) {
    size_t n = strlen(s);
    if (b->len + n + 1 > b->cap) {
        while (b->len + n + 1 > b->cap) b->cap *= 2;
        char *nb = (char *)ran_xalloc(b->cap);
        memcpy(nb, b->buf, b->len + 1);
        free(b->buf);
        b->buf = nb;
    }
    memcpy(b->buf + b->len, s, n + 1);
    b->len += n;
}

const char *ran_value_to_str(RanValue v) {
    switch (v.tag) {
        case RAN_INT:   return ran_int_to_str(v.u.i);
        case RAN_FLOAT: return ran_float_to_str(v.u.f);
        case RAN_BOOL:  return v.u.b ? "true" : "false";
        case RAN_DEC:   return dec_to_str((Dec){v.u.dec.mant, v.u.dec.scale});
        case RAN_STR:   return v.u.s->data;
        case RAN_VOID:  return "()";
        case RAN_ARRAY: {
            SB b; sb_init(&b);
            sb_push(&b, "[");
            for (size_t k = 0; k < v.u.a->len; k++) {
                if (k) sb_push(&b, ", ");
                sb_push(&b, ran_value_to_str(v.u.a->items[k]));
            }
            sb_push(&b, "]");
            return b.buf;
        }
        case RAN_OBJECT: {
            SB b; sb_init(&b);
            sb_push(&b, v.u.o->type_name);
            sb_push(&b, " {");
            for (size_t k = 0; k < v.u.o->n; k++) {
                if (k) sb_push(&b, ", ");
                sb_push(&b, v.u.o->names[k]);
                sb_push(&b, ": ");
                sb_push(&b, ran_value_to_str(v.u.o->vals[k]));
            }
            sb_push(&b, "}");
            return b.buf;
        }
        case RAN_MAP: {
            /* Mirrors `Value::Map` Display: {"k": v, ...}. ORDER is insertion
             * order here vs HashMap order in the interpreter — not byte-for-byte
             * comparable on the whole-map form (verify via per-key access). */
            SB b; sb_init(&b);
            sb_push(&b, "{");
            for (size_t k = 0; k < v.u.m->len; k++) {
                if (k) sb_push(&b, ", ");
                sb_push(&b, "\"");
                sb_push(&b, v.u.m->keys[k]);
                sb_push(&b, "\": ");
                sb_push(&b, ran_value_to_str(v.u.m->vals[k]));
            }
            sb_push(&b, "}");
            return b.buf;
        }
    }
    return "";
}

/* ====================================================================== */
/* D4a stdlib bridge — common modules in C (libc/libm only).              */
/*                                                                        */
/* Honesty contract (see ran_rt.h):                                       */
/*   * Deterministic functions (math.*, str.*, os.platform/arch/cwd,      */
/*     fs.* success paths, json.encode/pretty) reproduce the interpreter  */
/*     byte-for-byte.                                                     */
/*   * Nondeterministic functions (time.now/now_ms/now_iso, rand.*, the   */
/*     log timestamp, os.getpid/hostname/args) cannot match exact bytes;  */
/*     they match the FORMAT/shape/type and the same algorithm/seed.      */
/*                                                                        */
/* Documented unavoidable divergences:                                    */
/*   * str.upper/lower/trim* operate on ASCII (libc), whereas the          */
/*     interpreter uses Unicode-aware Rust casing/whitespace. Identical    */
/*     for ASCII text (the overwhelming common case).                      */
/*   * log args are joined with their display form but NOT re-scanned for  */
/*     `$name` interpolation (the interpreter re-interpolates the rendered */
/*     value against the live scope, which has no native runtime analog).  */
/*   * fs error MESSAGES (stderr) use strerror(); the success/return        */
/*     values are identical to the interpreter.                            */
/*   * os.args[0] is this binary's path (not the `ran` launcher path).      */
/* ====================================================================== */

#include <time.h>
#include <unistd.h>
#include <errno.h>
#include <ctype.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <dirent.h>

/* ---- Argument-vector accessors (mirror the interpreter's eval_arg_*). - */

static RanValue marg_val(const RanValue *argv, int64_t argc, int64_t i) {
    if (i >= 0 && i < argc) return argv[i];
    return ran_void();
}

/* eval_arg_str: a present string is returned as-is; any other present value
 * is rendered via Display; an absent argument yields `def`. */
static const char *marg_str(const RanValue *argv, int64_t argc, int64_t i, const char *def) {
    if (i < 0 || i >= argc) return def ? def : "";
    RanValue v = argv[i];
    if (v.tag == RAN_STR) return v.u.s->data;
    return ran_value_to_str(v);
}

/* eval_arg_int: Int as-is; Float truncates; Str parses (else default). */
static int64_t marg_int(const RanValue *argv, int64_t argc, int64_t i, int64_t def) {
    if (i < 0 || i >= argc) return def;
    RanValue v = argv[i];
    switch (v.tag) {
        case RAN_INT:   return v.u.i;
        case RAN_FLOAT: return (int64_t)v.u.f;
        case RAN_STR: {
            errno = 0;
            char *end = NULL;
            long long n = strtoll(v.u.s->data, &end, 10);
            if (errno != 0 || end == v.u.s->data || (end && *end != '\0')) return def;
            return (int64_t)n;
        }
        default: return def;
    }
}

/* as_f64 over an argument (Int/Float/Dec → double; else 0.0). */
static double rv_as_f64_pub(RanValue v); /* fwd */
static double marg_f64(const RanValue *argv, int64_t argc, int64_t i) {
    return rv_as_f64_pub(marg_val(argv, argc, i));
}

/* Value::as_f64 / as_i64 equivalents (used by math.*). */
static double rv_as_f64_pub(RanValue v) {
    switch (v.tag) {
        case RAN_INT:   return (double)v.u.i;
        case RAN_FLOAT: return v.u.f;
        case RAN_DEC:   return dec_to_f64((Dec){v.u.dec.mant, v.u.dec.scale});
        default:        return 0.0;
    }
}
static int64_t rv_as_i64_pub(RanValue v) {
    switch (v.tag) {
        case RAN_INT:   return v.u.i;
        case RAN_FLOAT: return (int64_t)v.u.f;
        case RAN_DEC:   return (int64_t)dec_to_f64((Dec){v.u.dec.mant, v.u.dec.scale});
        default:        return 0;
    }
}

/* ---- UTF-8 helpers (the interpreter counts/iterates Unicode scalars). - */

/* Count Unicode code points in a UTF-8 string (lead bytes only). */
static size_t utf8_count(const char *s) {
    size_t n = 0;
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        if ((*p & 0xC0) != 0x80) n++;
    }
    return n;
}

/* Byte length of the UTF-8 sequence starting at `p` (1..4). */
static size_t utf8_seq_len(const unsigned char *p) {
    unsigned char c = *p;
    if (c < 0x80) return 1;
    if ((c & 0xE0) == 0xC0) return 2;
    if ((c & 0xF0) == 0xE0) return 3;
    if ((c & 0xF8) == 0xF0) return 4;
    return 1;
}

/* Heap-dup a byte range [start,end) as a NUL-terminated C string. */
static const char *dup_range(const char *start, const char *end) {
    size_t n = (size_t)(end - start);
    char *out = (char *)ran_xalloc(n + 1);
    memcpy(out, start, n);
    out[n] = '\0';
    return out;
}
static const char *dup_str(const char *s) {
    if (!s) s = "";
    return dup_range(s, s + strlen(s));
}

/* ====================================================================== */
/* time — wall-clock helpers (nondeterministic; type/format-matched).      */
/* ====================================================================== */

int64_t ran_mod_time_now(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
    return (int64_t)time(NULL); /* seconds since the Unix epoch */
}

int64_t ran_mod_time_now_ms(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000 + (int64_t)(ts.tv_nsec / 1000000);
}

/* Civil-from-days (Howard Hinnant) — identical to the interpreter's
 * unix_to_iso, so the FORMAT and the date math match exactly (only the
 * captured instant differs). */
static const char *unix_to_iso(int64_t secs) {
    int64_t days = secs >= 0 ? secs / 86400 : -((-secs + 86399) / 86400);
    int64_t rem = secs - days * 86400;
    int hh = (int)(rem / 3600), mm = (int)((rem % 3600) / 60), ss = (int)(rem % 60);

    int64_t z = days + 719468;
    int64_t era = (z >= 0 ? z : z - 146096) / 146097;
    int64_t doe = z - era * 146097;
    int64_t yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    int64_t y = yoe + era * 400;
    int64_t doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    int64_t mp = (5 * doy + 2) / 153;
    int64_t d = doy - (153 * mp + 2) / 5 + 1;
    int64_t m = mp < 10 ? mp + 3 : mp - 9;
    int64_t year = m <= 2 ? y + 1 : y;

    char buf[40];
    snprintf(buf, sizeof(buf), "%04lld-%02lld-%02lldT%02d:%02d:%02dZ",
             (long long)year, (long long)m, (long long)d, hh, mm, ss);
    return dup_str(buf);
}

const char *ran_mod_time_now_iso(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
    return unix_to_iso((int64_t)time(NULL));
}

void ran_mod_time_sleep(const RanValue *argv, int64_t argc) {
    int64_t ms = marg_int(argv, argc, 0, 0);
    if (ms <= 0) return;
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (long)(ms % 1000) * 1000000L;
    nanosleep(&ts, NULL);
}

/* ====================================================================== */
/* log — leveled lines to stderr. Format identical to the interpreter:    */
/*   "<color><LEVEL:5-left><reset> [<ISO-8601>] <args joined by space>"   */
/* (the timestamp is the only nondeterministic part).                      */
/* ====================================================================== */

static void log_at(const char *level, const char *color, const RanValue *argv, int64_t argc) {
    const char *iso = unix_to_iso((int64_t)time(NULL));
    /* Join args with a single space using their display form. */
    SB b; sb_init(&b);
    for (int64_t i = 0; i < argc; i++) {
        if (i) sb_push(&b, " ");
        sb_push(&b, ran_value_to_str(argv[i]));
    }
    fprintf(stderr, "%s%-5s\x1b[0m [%s] %s\n", color, level, iso, b.buf);
}

void ran_mod_log_debug(const RanValue *argv, int64_t argc) { log_at("DEBUG", "\x1b[36m",   argv, argc); }
void ran_mod_log_info(const RanValue *argv, int64_t argc)  { log_at("INFO",  "\x1b[32m",   argv, argc); }
void ran_mod_log_warn(const RanValue *argv, int64_t argc)  { log_at("WARN",  "\x1b[33m",   argv, argc); }
void ran_mod_log_error(const RanValue *argv, int64_t argc) { log_at("ERROR", "\x1b[31m",   argv, argc); }
void ran_mod_log_fatal(const RanValue *argv, int64_t argc) {
    log_at("FATAL", "\x1b[31;1m", argv, argc);
    exit(1);
}

/* ====================================================================== */
/* math — <math.h>. abs/max/min preserve int-vs-float like the interpreter.*/
/* ====================================================================== */

RanValue ran_mod_math_abs(const RanValue *argv, int64_t argc) {
    RanValue a = marg_val(argv, argc, 0);
    if (a.tag == RAN_FLOAT) return ran_from_float(fabs(a.u.f));
    if (a.tag == RAN_INT)   return ran_from_int(a.u.i < 0 ? -a.u.i : a.u.i);
    return ran_from_int(0);
}
RanValue ran_mod_math_max(const RanValue *argv, int64_t argc) {
    RanValue a = marg_val(argv, argc, 0), b = marg_val(argv, argc, 1);
    if (a.tag == RAN_FLOAT || b.tag == RAN_FLOAT) {
        double x = rv_as_f64_pub(a), y = rv_as_f64_pub(b);
        return ran_from_float(x > y ? x : y);
    }
    int64_t x = rv_as_i64_pub(a), y = rv_as_i64_pub(b);
    return ran_from_int(x > y ? x : y);
}
RanValue ran_mod_math_min(const RanValue *argv, int64_t argc) {
    RanValue a = marg_val(argv, argc, 0), b = marg_val(argv, argc, 1);
    if (a.tag == RAN_FLOAT || b.tag == RAN_FLOAT) {
        double x = rv_as_f64_pub(a), y = rv_as_f64_pub(b);
        return ran_from_float(x < y ? x : y);
    }
    int64_t x = rv_as_i64_pub(a), y = rv_as_i64_pub(b);
    return ran_from_int(x < y ? x : y);
}
double ran_mod_math_sqrt(const RanValue *argv, int64_t argc)  { return sqrt(marg_f64(argv, argc, 0)); }
double ran_mod_math_pow(const RanValue *argv, int64_t argc)   { return pow(marg_f64(argv, argc, 0), marg_f64(argv, argc, 1)); }
int64_t ran_mod_math_floor(const RanValue *argv, int64_t argc){ return (int64_t)floor(marg_f64(argv, argc, 0)); }
int64_t ran_mod_math_ceil(const RanValue *argv, int64_t argc) { return (int64_t)ceil(marg_f64(argv, argc, 0)); }
int64_t ran_mod_math_round(const RanValue *argv, int64_t argc){ return (int64_t)round(marg_f64(argv, argc, 0)); }
double ran_mod_math_sin(const RanValue *argv, int64_t argc)   { return sin(marg_f64(argv, argc, 0)); }
double ran_mod_math_cos(const RanValue *argv, int64_t argc)   { return cos(marg_f64(argv, argc, 0)); }
double ran_mod_math_tan(const RanValue *argv, int64_t argc)   { return tan(marg_f64(argv, argc, 0)); }
double ran_mod_math_log(const RanValue *argv, int64_t argc)   { return log(marg_f64(argv, argc, 0)); }
double ran_mod_math_log10(const RanValue *argv, int64_t argc) { return log10(marg_f64(argv, argc, 0)); }
double ran_mod_math_pi(const RanValue *argv, int64_t argc)    { (void)argv; (void)argc; return 3.14159265358979311600; }
double ran_mod_math_e(const RanValue *argv, int64_t argc)     { (void)argv; (void)argc; return 2.71828182845904509080; }

/* ====================================================================== */
/* str — string utilities. ASCII case/whitespace (see header note).        */
/* ====================================================================== */

const char *ran_mod_str_from(const RanValue *argv, int64_t argc) {
    /* Display form, matching `format!("{}", v)`; dup so it is owned/stable. */
    return dup_str(ran_value_to_str(marg_val(argv, argc, 0)));
}

const char *ran_mod_str_upper(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    size_t n = strlen(s);
    char *out = (char *)ran_xalloc(n + 1);
    for (size_t i = 0; i < n; i++) out[i] = (char)toupper((unsigned char)s[i]);
    out[n] = '\0';
    return out;
}
const char *ran_mod_str_lower(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    size_t n = strlen(s);
    char *out = (char *)ran_xalloc(n + 1);
    for (size_t i = 0; i < n; i++) out[i] = (char)tolower((unsigned char)s[i]);
    out[n] = '\0';
    return out;
}

static bool is_ascii_ws(char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v'; }

const char *ran_mod_str_trim(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    const char *a = s;
    const char *b = s + strlen(s);
    while (a < b && is_ascii_ws(*a)) a++;
    while (b > a && is_ascii_ws(b[-1])) b--;
    return dup_range(a, b);
}
const char *ran_mod_str_trim_start(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    const char *a = s, *b = s + strlen(s);
    while (a < b && is_ascii_ws(*a)) a++;
    return dup_range(a, b);
}
const char *ran_mod_str_trim_end(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    const char *a = s, *b = s + strlen(s);
    while (b > a && is_ascii_ws(b[-1])) b--;
    return dup_range(a, b);
}

int64_t ran_mod_str_len(const RanValue *argv, int64_t argc) {
    return (int64_t)utf8_count(marg_str(argv, argc, 0, ""));
}

bool ran_mod_str_contains(const RanValue *argv, int64_t argc) {
    const char *hay = marg_str(argv, argc, 0, "");
    const char *needle = marg_str(argv, argc, 1, "");
    return strstr(hay, needle) != NULL;
}
bool ran_mod_str_starts_with(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    const char *p = marg_str(argv, argc, 1, "");
    size_t lp = strlen(p);
    return strncmp(s, p, lp) == 0;
}
bool ran_mod_str_ends_with(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    const char *p = marg_str(argv, argc, 1, "");
    size_t ls = strlen(s), lp = strlen(p);
    if (lp > ls) return false;
    return memcmp(s + (ls - lp), p, lp) == 0;
}

/* Codepoint index of the first byte offset; matches `s[..i].chars().count()`. */
int64_t ran_mod_str_index_of(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    const char *needle = marg_str(argv, argc, 1, "");
    if (*needle == '\0') return 0;            /* Rust find("") == Some(0) */
    const char *hit = strstr(s, needle);
    if (!hit) return -1;
    size_t bytes = (size_t)(hit - s);
    /* count codepoints in s[0..bytes] */
    int64_t cp = 0;
    for (size_t i = 0; i < bytes; ) { i += utf8_seq_len((const unsigned char *)s + i); cp++; }
    return cp;
}

const char *ran_mod_str_replace(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    const char *from = marg_str(argv, argc, 1, "");
    const char *to = marg_str(argv, argc, 2, "");
    SB b; sb_init(&b);
    size_t lf = strlen(from);
    if (lf == 0) {
        /* Rust: "".replace inserts `to` around every char: -a-b- form. */
        sb_push(&b, to);
        for (const char *p = s; *p; ) {
            size_t l = utf8_seq_len((const unsigned char *)p);
            char *tmp = (char *)ran_xalloc(l + 1);
            memcpy(tmp, p, l); tmp[l] = '\0';
            sb_push(&b, tmp); free(tmp);
            sb_push(&b, to);
            p += l;
        }
        return b.buf;
    }
    const char *p = s;
    while (1) {
        const char *hit = strstr(p, from);
        if (!hit) { sb_push(&b, p); break; }
        char *pre = (char *)ran_xalloc((size_t)(hit - p) + 1);
        memcpy(pre, p, (size_t)(hit - p)); pre[hit - p] = '\0';
        sb_push(&b, pre); free(pre);
        sb_push(&b, to);
        p = hit + lf;
    }
    return b.buf;
}

RanValue ran_mod_str_split(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    const char *delim = marg_str(argv, argc, 1, " ");
    RanValue arr = ran_array_new(4);
    size_t ld = strlen(delim);
    if (ld == 0) {
        /* Rust split(""): "" + each char + "" */
        ran_array_push(arr, ran_from_str(""));
        for (const char *p = s; *p; ) {
            size_t l = utf8_seq_len((const unsigned char *)p);
            const char *piece = dup_range(p, p + l);
            ran_array_push(arr, ran_from_str(piece));
            p += l;
        }
        ran_array_push(arr, ran_from_str(""));
        return arr;
    }
    const char *p = s;
    while (1) {
        const char *hit = strstr(p, delim);
        if (!hit) {
            ran_array_push(arr, ran_from_str(dup_str(p)));
            break;
        }
        ran_array_push(arr, ran_from_str(dup_range(p, hit)));
        p = hit + ld;
    }
    return arr;
}

const char *ran_mod_str_join(const RanValue *argv, int64_t argc) {
    RanValue arr = marg_val(argv, argc, 0);
    const char *sep = marg_str(argv, argc, 1, "");
    SB b; sb_init(&b);
    if (arr.tag == RAN_ARRAY) {
        for (size_t i = 0; i < arr.u.a->len; i++) {
            if (i) sb_push(&b, sep);
            sb_push(&b, ran_value_to_str(arr.u.a->items[i]));
        }
    }
    return b.buf;
}

const char *ran_mod_str_repeat(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    int64_t n = marg_int(argv, argc, 1, 0);
    if (n < 0) n = 0;
    size_t ls = strlen(s);
    SB b; sb_init(&b);
    for (int64_t i = 0; i < n; i++) {
        if (ls) sb_push(&b, s);
    }
    return b.buf;
}

const char *ran_mod_str_reverse(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    size_t n = strlen(s);
    char *out = (char *)ran_xalloc(n + 1);
    size_t oi = n;
    out[n] = '\0';
    /* Reverse by codepoint so multibyte sequences stay intact. */
    for (const char *p = s; *p; ) {
        size_t l = utf8_seq_len((const unsigned char *)p);
        oi -= l;
        memcpy(out + oi, p, l);
        p += l;
    }
    return out;
}

/* Take the first codepoint of `pad` (default ' '), as a small heap string. */
static const char *first_codepoint(const char *pad) {
    if (!pad || !*pad) return " ";
    size_t l = utf8_seq_len((const unsigned char *)pad);
    return dup_range(pad, pad + l);
}

const char *ran_mod_str_pad_left(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    int64_t width = marg_int(argv, argc, 1, 0);
    if (width < 0) width = 0;
    const char *padc = first_codepoint(marg_str(argv, argc, 2, " "));
    size_t len = utf8_count(s);
    if ((int64_t)len >= width) return dup_str(s);
    SB b; sb_init(&b);
    for (int64_t i = 0; i < width - (int64_t)len; i++) sb_push(&b, padc);
    sb_push(&b, s);
    return b.buf;
}
const char *ran_mod_str_pad_right(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    int64_t width = marg_int(argv, argc, 1, 0);
    if (width < 0) width = 0;
    const char *padc = first_codepoint(marg_str(argv, argc, 2, " "));
    size_t len = utf8_count(s);
    if ((int64_t)len >= width) return dup_str(s);
    SB b; sb_init(&b);
    sb_push(&b, s);
    for (int64_t i = 0; i < width - (int64_t)len; i++) sb_push(&b, padc);
    return b.buf;
}

int64_t ran_mod_str_to_int(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    /* trim, then parse i64; on failure -> 0 (matches `.parse().unwrap_or(0)`). */
    while (is_ascii_ws(*s)) s++;
    const char *end = s + strlen(s);
    while (end > s && is_ascii_ws(end[-1])) end--;
    char buf[32];
    size_t n = (size_t)(end - s);
    if (n == 0 || n >= sizeof(buf)) return 0;
    memcpy(buf, s, n); buf[n] = '\0';
    errno = 0;
    char *pe = NULL;
    long long v = strtoll(buf, &pe, 10);
    if (errno != 0 || pe == buf || *pe != '\0') return 0;
    return (int64_t)v;
}
double ran_mod_str_to_float(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    while (is_ascii_ws(*s)) s++;
    const char *end = s + strlen(s);
    while (end > s && is_ascii_ws(end[-1])) end--;
    char buf[64];
    size_t n = (size_t)(end - s);
    if (n == 0 || n >= sizeof(buf)) return 0.0;
    memcpy(buf, s, n); buf[n] = '\0';
    errno = 0;
    char *pe = NULL;
    double v = strtod(buf, &pe);
    if (pe == buf || *pe != '\0') return 0.0;
    return v;
}

/* ====================================================================== */
/* os — platform/arch are compile-time constants matching Rust's            */
/* std::env::consts; the rest use libc/POSIX.                               */
/* ====================================================================== */

const char *ran_mod_os_platform(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
#if defined(__linux__)
    return "linux";
#elif defined(__APPLE__)
    return "macos";
#elif defined(_WIN32)
    return "windows";
#elif defined(__FreeBSD__)
    return "freebsd";
#else
    return "unknown";
#endif
}

const char *ran_mod_os_arch(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
#if defined(__x86_64__) || defined(_M_X64)
    return "x86_64";
#elif defined(__aarch64__)
    return "aarch64";
#elif defined(__arm__)
    return "arm";
#elif defined(__i386__)
    return "x86";
#else
    return "unknown";
#endif
}

const char *ran_mod_os_cwd(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
    char buf[4096];
    if (getcwd(buf, sizeof(buf))) return dup_str(buf);
    return "";
}

const char *ran_mod_os_hostname(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
    /* Mirror the interpreter: read /etc/hostname (trimmed) if readable, else
     * $HOSTNAME, else "localhost". */
    FILE *f = fopen("/etc/hostname", "rb");
    if (f) {
        SB b; sb_init(&b);
        char chunk[256];
        size_t r;
        while ((r = fread(chunk, 1, sizeof(chunk) - 1, f)) > 0) { chunk[r] = '\0'; sb_push(&b, chunk); }
        fclose(f);
        /* trim trailing/leading ASCII whitespace */
        char *a = b.buf;
        char *e = b.buf + strlen(b.buf);
        while (a < e && is_ascii_ws(*a)) a++;
        while (e > a && is_ascii_ws(e[-1])) e--;
        return dup_range(a, e);
    }
    const char *h = getenv("HOSTNAME");
    if (h) return dup_str(h);
    return "localhost";
}

const char *ran_mod_os_env_or(const RanValue *argv, int64_t argc) {
    const char *key = marg_str(argv, argc, 0, "");
    const char *def = marg_str(argv, argc, 1, "");
    const char *v = getenv(key);
    return v ? dup_str(v) : dup_str(def);
}

int64_t ran_mod_os_getpid(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
    return (int64_t)getpid();
}

int64_t ran_mod_os_cpu_count(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
    long n = sysconf(_SC_NPROCESSORS_ONLN);
    return n > 0 ? (int64_t)n : 1;
}

/* os.env(key): Str if set, else Void (matches the interpreter). */
RanValue ran_mod_os_env(const RanValue *argv, int64_t argc) {
    const char *key = marg_str(argv, argc, 0, "");
    const char *v = getenv(key);
    return v ? ran_from_str(v) : ran_void();
}

bool ran_mod_os_setenv(const RanValue *argv, int64_t argc) {
    const char *key = marg_str(argv, argc, 0, "");
    const char *val = marg_str(argv, argc, 1, "");
    setenv(key, val, 1);
    return true;
}

void ran_mod_os_exit(const RanValue *argv, int64_t argc) {
    exit((int)marg_int(argv, argc, 0, 0));
}

/* os.args via /proc/self/cmdline (Linux). argv[0] is THIS binary's path, so
 * it differs from the interpreter's launcher path — type/shape-matched only. */
RanValue ran_mod_os_args(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
    RanValue arr = ran_array_new(4);
    FILE *f = fopen("/proc/self/cmdline", "rb");
    if (!f) return arr;
    SB b; sb_init(&b);
    int ch;
    while ((ch = fgetc(f)) != EOF) {
        if (ch == '\0') {
            ran_array_push(arr, ran_from_str(b.buf));
            b.len = 0; b.buf[0] = '\0';
        } else {
            char one[2] = { (char)ch, '\0' };
            sb_push(&b, one);
        }
    }
    if (b.len > 0) ran_array_push(arr, ran_from_str(b.buf));
    fclose(f);
    return arr;
}

/* ====================================================================== */
/* fs — file system over stdio/POSIX. Return values match the interpreter; */
/* error MESSAGES use strerror (see header note).                          */
/* ====================================================================== */

RanValue ran_mod_fs_read(const RanValue *argv, int64_t argc) {
    const char *path = marg_str(argv, argc, 0, "");
    FILE *f = fopen(path, "rb");
    if (!f) { fprintf(stderr, "ran: fs.read error: %s\n", strerror(errno)); return ran_void(); }
    SB b; sb_init(&b);
    char chunk[4096];
    size_t r;
    while ((r = fread(chunk, 1, sizeof(chunk), f)) > 0) {
        /* Preserve embedded NULs poorly (text-oriented), but typical text is
         * fine; matches the interpreter's read_to_string for text files. */
        char *tmp = (char *)ran_xalloc(r + 1);
        memcpy(tmp, chunk, r); tmp[r] = '\0';
        sb_push(&b, tmp); free(tmp);
    }
    fclose(f);
    return ran_from_str(b.buf);
}

bool ran_mod_fs_write(const RanValue *argv, int64_t argc) {
    const char *path = marg_str(argv, argc, 0, "");
    const char *content = marg_str(argv, argc, 1, "");
    FILE *f = fopen(path, "wb");
    if (!f) { fprintf(stderr, "ran: fs.write error: %s\n", strerror(errno)); return false; }
    size_t n = strlen(content);
    bool ok = fwrite(content, 1, n, f) == n;
    fclose(f);
    if (!ok) fprintf(stderr, "ran: fs.write error: %s\n", strerror(errno));
    return ok;
}

bool ran_mod_fs_exists(const RanValue *argv, int64_t argc) {
    const char *path = marg_str(argv, argc, 0, "");
    struct stat st;
    return stat(path, &st) == 0;
}

bool ran_mod_fs_append(const RanValue *argv, int64_t argc) {
    const char *path = marg_str(argv, argc, 0, "");
    const char *content = marg_str(argv, argc, 1, "");
    FILE *f = fopen(path, "ab");
    if (!f) return false;
    size_t n = strlen(content);
    bool ok = fwrite(content, 1, n, f) == n;
    fclose(f);
    return ok;
}

bool ran_mod_fs_remove(const RanValue *argv, int64_t argc) {
    const char *path = marg_str(argv, argc, 0, "");
    return remove(path) == 0;
}

/* Recursive mkdir (like std::fs::create_dir_all). */
static bool mkdir_p(const char *path) {
    if (!path || !*path) return false;
    size_t len = strlen(path);
    char *tmp = (char *)ran_xalloc(len + 1);
    memcpy(tmp, path, len + 1);
    for (char *p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = '\0';
            if (mkdir(tmp, 0777) != 0 && errno != EEXIST) { free(tmp); return false; }
            *p = '/';
        }
    }
    bool ok = (mkdir(tmp, 0777) == 0 || errno == EEXIST);
    free(tmp);
    return ok;
}
bool ran_mod_fs_mkdir(const RanValue *argv, int64_t argc) {
    return mkdir_p(marg_str(argv, argc, 0, ""));
}

bool ran_mod_fs_is_file(const RanValue *argv, int64_t argc) {
    const char *path = marg_str(argv, argc, 0, "");
    struct stat st;
    return stat(path, &st) == 0 && S_ISREG(st.st_mode);
}
bool ran_mod_fs_is_dir(const RanValue *argv, int64_t argc) {
    const char *path = marg_str(argv, argc, 0, "");
    struct stat st;
    return stat(path, &st) == 0 && S_ISDIR(st.st_mode);
}

RanValue ran_mod_fs_readdir(const RanValue *argv, int64_t argc) {
    const char *path = marg_str(argv, argc, 0, ".");
    RanValue arr = ran_array_new(8);
    DIR *d = opendir(path);
    if (!d) return arr;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        ran_array_push(arr, ran_from_str(e->d_name));
    }
    closedir(d);
    return arr;
}

int64_t ran_mod_fs_size(const RanValue *argv, int64_t argc) {
    const char *path = marg_str(argv, argc, 0, "");
    struct stat st;
    if (stat(path, &st) == 0) return (int64_t)st.st_size;
    return -1;
}

bool ran_mod_fs_copy(const RanValue *argv, int64_t argc) {
    const char *from = marg_str(argv, argc, 0, "");
    const char *to = marg_str(argv, argc, 1, "");
    FILE *in = fopen(from, "rb");
    if (!in) return false;
    FILE *out = fopen(to, "wb");
    if (!out) { fclose(in); return false; }
    char chunk[8192];
    size_t r;
    bool ok = true;
    while ((r = fread(chunk, 1, sizeof(chunk), in)) > 0) {
        if (fwrite(chunk, 1, r, out) != r) { ok = false; break; }
    }
    fclose(in);
    fclose(out);
    return ok;
}

bool ran_mod_fs_rename(const RanValue *argv, int64_t argc) {
    const char *from = marg_str(argv, argc, 0, "");
    const char *to = marg_str(argv, argc, 1, "");
    return rename(from, to) == 0;
}

/* ====================================================================== */
/* rand — xorshift64, lazily seeded from the wall clock like the           */
/* interpreter (nondeterministic; only the value distribution is matched). */
/* ====================================================================== */

static uint64_t g_rand_state = 0;

static uint64_t rand_u64(void) {
    uint64_t x = g_rand_state;
    if (x == 0) {
        struct timespec ts;
        clock_gettime(CLOCK_REALTIME, &ts);
        x = ((uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec) | 1ull;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    g_rand_state = x;
    return x;
}

int64_t ran_mod_rand_int(const RanValue *argv, int64_t argc) {
    int64_t lo = marg_int(argv, argc, 0, 0);
    int64_t hi = marg_int(argv, argc, 1, INT64_MAX);
    int64_t range = hi - lo;
    if (range < 1) range = 1;
    return lo + (int64_t)(rand_u64() % (uint64_t)range);
}
double ran_mod_rand_float(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
    return (double)rand_u64() / (double)UINT64_MAX;
}
bool ran_mod_rand_bool(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
    return (rand_u64() % 2) == 0;
}

/* ====================================================================== */
/* json — encode/stringify (compact) and pretty (2-space indent).         */
/* Matches the interpreter's to_json / to_json_pretty byte-for-byte for    */
/* the native value model (arrays of scalars are fully deterministic;      */
/* object FIELD ORDER follows declaration order here).                     */
/* ====================================================================== */

/* RFC 8259 string escaping into an SB (mirrors json_escape_str). */
static void json_escape_into(SB *b, const char *s) {
    sb_push(b, "\"");
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        unsigned char c = *p;
        switch (c) {
            case '"':  sb_push(b, "\\\""); break;
            case '\\': sb_push(b, "\\\\"); break;
            case '\n': sb_push(b, "\\n"); break;
            case '\r': sb_push(b, "\\r"); break;
            case '\t': sb_push(b, "\\t"); break;
            case '\b': sb_push(b, "\\b"); break;
            case '\f': sb_push(b, "\\f"); break;
            default:
                if (c < 0x20) {
                    char u[8];
                    snprintf(u, sizeof(u), "\\u%04x", (unsigned)c);
                    sb_push(b, u);
                } else {
                    char one[2] = { (char)c, '\0' };
                    sb_push(b, one);
                }
        }
    }
    sb_push(b, "\"");
}

static void json_encode_into(SB *b, RanValue v) {
    switch (v.tag) {
        case RAN_VOID:  sb_push(b, "null"); break;
        case RAN_INT:   sb_push(b, ran_int_to_str(v.u.i)); break;
        case RAN_FLOAT: sb_push(b, ran_float_to_str(v.u.f)); break;
        case RAN_DEC:   sb_push(b, dec_to_str((Dec){v.u.dec.mant, v.u.dec.scale})); break;
        case RAN_BOOL:  sb_push(b, v.u.b ? "true" : "false"); break;
        case RAN_STR:   json_escape_into(b, v.u.s->data); break;
        case RAN_ARRAY: {
            sb_push(b, "[");
            for (size_t i = 0; i < v.u.a->len; i++) {
                if (i) sb_push(b, ",");
                json_encode_into(b, v.u.a->items[i]);
            }
            sb_push(b, "]");
            break;
        }
        case RAN_OBJECT: {
            sb_push(b, "{");
            for (size_t i = 0; i < v.u.o->n; i++) {
                if (i) sb_push(b, ",");
                json_escape_into(b, v.u.o->names[i]);
                sb_push(b, ":");
                json_encode_into(b, v.u.o->vals[i]);
            }
            sb_push(b, "}");
            break;
        }
        case RAN_MAP: {
            /* Matches Value::Map to_json: {"k":v,...}. Key ORDER is insertion
             * order (interpreter: HashMap order) — see ran_rt.h note. */
            sb_push(b, "{");
            for (size_t i = 0; i < v.u.m->len; i++) {
                if (i) sb_push(b, ",");
                json_escape_into(b, v.u.m->keys[i]);
                sb_push(b, ":");
                json_encode_into(b, v.u.m->vals[i]);
            }
            sb_push(b, "}");
            break;
        }
    }
}

const char *ran_mod_json_encode(const RanValue *argv, int64_t argc) {
    SB b; sb_init(&b);
    json_encode_into(&b, marg_val(argv, argc, 0));
    return b.buf;
}
const char *ran_mod_json_stringify(const RanValue *argv, int64_t argc) {
    return ran_mod_json_encode(argv, argc);
}

static void sb_indent(SB *b, int n) { for (int i = 0; i < n; i++) sb_push(b, "  "); }

/* Mirrors to_json_pretty: arrays/objects expand with 2-space indentation;
 * scalars fall back to compact json. */
static void json_pretty_into(SB *b, RanValue v, int indent) {
    if (v.tag == RAN_ARRAY) {
        if (v.u.a->len == 0) { sb_push(b, "[]"); return; }
        sb_push(b, "[\n");
        for (size_t i = 0; i < v.u.a->len; i++) {
            if (i) sb_push(b, ",\n");
            sb_indent(b, indent + 1);
            json_pretty_into(b, v.u.a->items[i], indent + 1);
        }
        sb_push(b, "\n");
        sb_indent(b, indent);
        sb_push(b, "]");
        return;
    }
    if (v.tag == RAN_OBJECT) {
        if (v.u.o->n == 0) { sb_push(b, "{}"); return; }
        sb_push(b, "{\n");
        for (size_t i = 0; i < v.u.o->n; i++) {
            if (i) sb_push(b, ",\n");
            sb_indent(b, indent + 1);
            json_escape_into(b, v.u.o->names[i]);
            sb_push(b, ": ");
            json_pretty_into(b, v.u.o->vals[i], indent + 1);
        }
        sb_push(b, "\n");
        sb_indent(b, indent);
        sb_push(b, "}");
        return;
    }
    if (v.tag == RAN_MAP) {
        if (v.u.m->len == 0) { sb_push(b, "{}"); return; }
        sb_push(b, "{\n");
        for (size_t i = 0; i < v.u.m->len; i++) {
            if (i) sb_push(b, ",\n");
            sb_indent(b, indent + 1);
            json_escape_into(b, v.u.m->keys[i]);
            sb_push(b, ": ");
            json_pretty_into(b, v.u.m->vals[i], indent + 1);
        }
        sb_push(b, "\n");
        sb_indent(b, indent);
        sb_push(b, "}");
        return;
    }
    json_encode_into(b, v);
}

const char *ran_mod_json_pretty(const RanValue *argv, int64_t argc) {
    SB b; sb_init(&b);
    json_pretty_into(&b, marg_val(argv, argc, 0), 0);
    return b.buf;
}

/* ====================================================================== */
/* json.decode / parse / get / valid — byte-faithful mirror of the        */
/* interpreter's recursive-descent parser in runtime/json.rs.             */
/*                                                                        */
/* The interpreter scans a Vec<char> (Unicode scalars); this scans bytes. */
/* For ASCII JSON the two are identical; string CONTENT bytes (incl.      */
/* multi-byte UTF-8) are copied verbatim, and `\uXXXX` escapes are        */
/* re-encoded to UTF-8 (with surrogate-pair joining) so decoded strings   */
/* match the interpreter byte-for-byte for deterministic inputs. Objects  */
/* decode to RAN_MAP, arrays to RAN_ARRAY, numbers to RAN_INT|RAN_FLOAT,  */
/* booleans to RAN_BOOL, strings to RAN_STR, and null to RAN_VOID — the   */
/* exact value shapes `parse_json` produces.                              */
/* ====================================================================== */

static bool jp_is_ws(unsigned char c) {
    /* Rust char::is_whitespace for the bytes JSON can contain: the ASCII
     * whitespace set (space, tab, LF, VT, FF, CR). */
    return c == ' ' || c == '\t' || c == '\n' || c == '\v' || c == '\f' || c == '\r';
}

static void jp_skip_ws(const char *s, size_t len, size_t *pos) {
    while (*pos < len && jp_is_ws((unsigned char)s[*pos])) (*pos)++;
}

/* Append a Unicode code point to `b` as UTF-8 (mirrors pushing a `char`). */
static void utf8_encode_into(SB *b, unsigned int cp) {
    char tmp[5];
    if (cp < 0x80) {
        tmp[0] = (char)cp; tmp[1] = '\0';
    } else if (cp < 0x800) {
        tmp[0] = (char)(0xC0 | (cp >> 6));
        tmp[1] = (char)(0x80 | (cp & 0x3F));
        tmp[2] = '\0';
    } else if (cp < 0x10000) {
        tmp[0] = (char)(0xE0 | (cp >> 12));
        tmp[1] = (char)(0x80 | ((cp >> 6) & 0x3F));
        tmp[2] = (char)(0x80 | (cp & 0x3F));
        tmp[3] = '\0';
    } else {
        tmp[0] = (char)(0xF0 | (cp >> 18));
        tmp[1] = (char)(0x80 | ((cp >> 12) & 0x3F));
        tmp[2] = (char)(0x80 | ((cp >> 6) & 0x3F));
        tmp[3] = (char)(0x80 | (cp & 0x3F));
        tmp[4] = '\0';
    }
    sb_push(b, tmp);
}

/* Read 4 hex digits following a `\u` (pos is ON the 'u'); on success advances
 * pos to the last hex digit and returns the code unit, else returns -1. */
static long jp_read_hex4(const char *s, size_t len, size_t *pos) {
    if (*pos + 4 >= len) return -1;
    unsigned int val = 0;
    for (int k = 1; k <= 4; k++) {
        char c = s[*pos + k];
        unsigned int d;
        if (c >= '0' && c <= '9') d = (unsigned int)(c - '0');
        else if (c >= 'a' && c <= 'f') d = (unsigned int)(c - 'a' + 10);
        else if (c >= 'A' && c <= 'F') d = (unsigned int)(c - 'A' + 10);
        else return -1;
        val = val * 16 + d;
    }
    *pos += 4;
    return (long)val;
}

/* Parse a JSON string starting at the opening quote; returns a heap copy. */
static char *jp_string(const char *s, size_t len, size_t *pos) {
    SB b; sb_init(&b);
    (*pos)++; /* opening quote */
    while (*pos < len && s[*pos] != '"') {
        if (s[*pos] == '\\' && *pos + 1 < len) {
            (*pos)++;
            char e = s[*pos];
            switch (e) {
                case 'n': sb_push(&b, "\n"); break;
                case 't': sb_push(&b, "\t"); break;
                case 'r': sb_push(&b, "\r"); break;
                case 'b': sb_push(&b, "\b"); break;
                case 'f': sb_push(&b, "\f"); break;
                case '"': sb_push(&b, "\""); break;
                case '\\': sb_push(&b, "\\"); break;
                case '/': sb_push(&b, "/"); break;
                case 'u': {
                    long cp = jp_read_hex4(s, len, pos);
                    if (cp >= 0) {
                        if (cp >= 0xD800 && cp <= 0xDBFF
                            && *pos + 2 < len
                            && s[*pos + 1] == '\\' && s[*pos + 2] == 'u') {
                            *pos += 2; /* onto the second 'u' */
                            long lo = jp_read_hex4(s, len, pos);
                            if (lo >= 0) {
                                unsigned int c = 0x10000u
                                    + (((unsigned int)cp - 0xD800u) << 10)
                                    + ((unsigned int)lo - 0xDC00u);
                                utf8_encode_into(&b, c);
                            }
                        } else {
                            utf8_encode_into(&b, (unsigned int)cp);
                        }
                    }
                    break;
                }
                default: {
                    char one[2] = { e, '\0' };
                    sb_push(&b, one);
                    break;
                }
            }
        } else {
            char one[2] = { s[*pos], '\0' };
            sb_push(&b, one);
        }
        (*pos)++;
    }
    (*pos)++; /* closing quote */
    return b.buf;
}

static RanValue jp_value(const char *s, size_t len, size_t *pos);

static RanValue jp_number(const char *s, size_t len, size_t *pos) {
    size_t start = *pos;
    bool is_float = false;
    while (*pos < len) {
        char c = s[*pos];
        if ((c >= '0' && c <= '9') || c == '-' || c == '+') {
            (*pos)++;
        } else if (c == '.' || c == 'e' || c == 'E') {
            is_float = true;
            (*pos)++;
        } else {
            break;
        }
    }
    size_t n = *pos - start;
    char *buf = (char *)ran_xalloc(n + 1);
    memcpy(buf, s + start, n);
    buf[n] = '\0';
    RanValue v;
    if (is_float) {
        /* `num.parse::<f64>().unwrap_or(0.0)`: the whole text must parse. */
        char *end = NULL;
        double d = strtod(buf, &end);
        v = ran_from_float((end && *end == '\0' && end != buf) ? d : 0.0);
    } else {
        /* `num.parse::<i64>().unwrap_or(0)`: the whole text must parse. */
        char *end = NULL;
        long long ll = strtoll(buf, &end, 10);
        v = ran_from_int((end && *end == '\0' && end != buf) ? (int64_t)ll : 0);
    }
    free(buf);
    return v;
}

static RanValue jp_bool(const char *s, size_t len, size_t *pos) {
    if (s[*pos] == 't') {
        if (*pos + 4 <= len && strncmp(s + *pos, "true", 4) == 0) { *pos += 4; return ran_from_bool(true); }
        (*pos)++; return ran_from_bool(true);
    } else {
        if (*pos + 5 <= len && strncmp(s + *pos, "false", 5) == 0) { *pos += 5; return ran_from_bool(false); }
        (*pos)++; return ran_from_bool(false);
    }
}

static RanValue jp_array(const char *s, size_t len, size_t *pos) {
    RanValue arr = ran_array_new(4);
    (*pos)++; /* '[' */
    for (;;) {
        jp_skip_ws(s, len, pos);
        if (*pos >= len || s[*pos] == ']') { (*pos)++; break; }
        ran_array_push(arr, jp_value(s, len, pos));
        jp_skip_ws(s, len, pos);
        if (*pos < len && s[*pos] == ',') (*pos)++;
    }
    return arr;
}

static RanValue jp_object(const char *s, size_t len, size_t *pos) {
    RanValue map = ran_map_new();
    (*pos)++; /* '{' */
    for (;;) {
        jp_skip_ws(s, len, pos);
        if (*pos >= len || s[*pos] == '}') { (*pos)++; break; }
        if (s[*pos] != '"') break; /* malformed key -> stop (matches interpreter) */
        char *key = jp_string(s, len, pos);
        jp_skip_ws(s, len, pos);
        if (*pos < len && s[*pos] == ':') (*pos)++;
        RanValue val = jp_value(s, len, pos);
        ran_map_set(map, key, val); /* key copied; val ownership taken */
        free(key);
        jp_skip_ws(s, len, pos);
        if (*pos < len && s[*pos] == ',') (*pos)++;
    }
    return map;
}

static RanValue jp_value(const char *s, size_t len, size_t *pos) {
    jp_skip_ws(s, len, pos);
    if (*pos >= len) return ran_void();
    switch (s[*pos]) {
        case '{': return jp_object(s, len, pos);
        case '[': return jp_array(s, len, pos);
        case '"': { char *str = jp_string(s, len, pos); RanValue v = ran_from_str(str); free(str); return v; }
        case 't':
        case 'f': return jp_bool(s, len, pos);
        case 'n':
            if (*pos + 4 <= len && strncmp(s + *pos, "null", 4) == 0) { *pos += 4; return ran_void(); }
            (*pos)++; return ran_void();
        default: return jp_number(s, len, pos);
    }
}

static RanValue ran_parse_json(const char *s) {
    if (!s) s = "";
    size_t len = strlen(s);
    size_t pos = 0;
    return jp_value(s, len, &pos);
}

RanValue ran_mod_json_decode(const RanValue *argv, int64_t argc) {
    return ran_parse_json(marg_str(argv, argc, 0, ""));
}
RanValue ran_mod_json_parse(const RanValue *argv, int64_t argc) {
    return ran_parse_json(marg_str(argv, argc, 0, ""));
}

bool ran_mod_json_valid(const RanValue *argv, int64_t argc) {
    const char *s = marg_str(argv, argc, 0, "");
    if (!s) s = "";
    /* trim() empty check (matches `s.trim().is_empty()`). */
    const char *a = s;
    const char *z = s + strlen(s);
    while (a < z && jp_is_ws((unsigned char)*a)) a++;
    while (z > a && jp_is_ws((unsigned char)z[-1])) z--;
    if (a == z) return false;

    size_t len = strlen(s);
    size_t pos = 0;
    jp_skip_ws(s, len, &pos);
    size_t start = pos;
    RanValue v = jp_value(s, len, &pos);
    ran_release(v);
    if (pos <= start) return false;
    jp_skip_ws(s, len, &pos);
    return pos >= len;
}

/* json.get(value_or_json_string, "a.b.0"): a Str base is parsed first, then a
 * dotted path is walked (named segments index map/object, numeric segments
 * index arrays). Mirrors json_path_get; returns RAN_VOID for any missing step.
 * BORROWS argv; returns an owned value. */
RanValue ran_mod_json_get(const RanValue *argv, int64_t argc) {
    RanValue base = marg_val(argv, argc, 0);
    RanValue cur;
    if (base.tag == RAN_STR) {
        cur = ran_parse_json(base.u.s->data); /* owned */
    } else {
        cur = ran_clone(base);                /* owned copy of the borrowed arg */
    }
    const char *path = marg_str(argv, argc, 1, "");
    if (!path || path[0] == '\0') return cur;

    const char *p = path;
    while (*p) {
        const char *seg_start = p;
        while (*p && *p != '.') p++;
        size_t seg_len = (size_t)(p - seg_start);
        char *seg = (char *)ran_xalloc(seg_len + 1);
        memcpy(seg, seg_start, seg_len);
        seg[seg_len] = '\0';

        RanValue next;
        if (cur.tag == RAN_MAP) {
            next = ran_map_get(cur, seg);
        } else if (cur.tag == RAN_OBJECT) {
            next = ran_field(cur, seg);
        } else if (cur.tag == RAN_ARRAY) {
            /* numeric segment only (Rust `seg.parse::<usize>()`); else void. */
            char *end = NULL;
            unsigned long long idx = strtoull(seg, &end, 10);
            if (end && *end == '\0' && end != seg) {
                next = (idx < cur.u.a->len) ? ran_clone(cur.u.a->items[idx]) : ran_void();
            } else {
                free(seg);
                ran_release(cur);
                return ran_void();
            }
        } else {
            free(seg);
            ran_release(cur);
            return ran_void();
        }
        free(seg);
        ran_release(cur);
        cur = next;
        if (*p == '.') p++;
    }
    return cur;
}

/* ====================================================================== */
/* env — environment + dotenv (D4b-1). Deterministic given the process    */
/* environment. Mirrors the interpreter's `env` module in module_dispatch */
/* (env.get/get_or/require/has/set/unset/int/float/bool/decimal/all) and  */
/* the dotenv loader (env.load/load_override/load_default).               */
/* ====================================================================== */

extern char **environ;

/* parse_env_bool: true/1/yes/on/y -> true; false/0/no/off/n/"" -> false;
 * anything else -> "no decision" (returns false, sets *ok=false). */
static bool env_parse_bool(const char *s, bool *ok) {
    /* trim + lowercase into a small stack buffer. */
    char buf[32];
    const char *a = s ? s : "";
    const char *z = a + strlen(a);
    while (a < z && (*a==' '||*a=='\t'||*a=='\n'||*a=='\r'||*a=='\f'||*a=='\v')) a++;
    while (z > a && (z[-1]==' '||z[-1]=='\t'||z[-1]=='\n'||z[-1]=='\r'||z[-1]=='\f'||z[-1]=='\v')) z--;
    size_t n = (size_t)(z - a);
    if (n >= sizeof(buf)) n = sizeof(buf) - 1;
    for (size_t i = 0; i < n; i++) {
        char c = a[i];
        if (c >= 'A' && c <= 'Z') c = (char)(c - 'A' + 'a');
        buf[i] = c;
    }
    buf[n] = '\0';
    *ok = true;
    if (!strcmp(buf,"true")||!strcmp(buf,"1")||!strcmp(buf,"yes")||!strcmp(buf,"on")||!strcmp(buf,"y")) return true;
    if (!strcmp(buf,"false")||!strcmp(buf,"0")||!strcmp(buf,"no")||!strcmp(buf,"off")||!strcmp(buf,"n")||buf[0]=='\0') return false;
    *ok = false;
    return false;
}

RanValue ran_mod_env_get(const RanValue *argv, int64_t argc) {
    const char *v = getenv(marg_str(argv, argc, 0, ""));
    return v ? ran_from_str(v) : ran_void();
}

const char *ran_mod_env_get_or(const RanValue *argv, int64_t argc) {
    const char *key = marg_str(argv, argc, 0, "");
    const char *def = marg_str(argv, argc, 1, "");
    const char *v = getenv(key);
    return dup_str(v ? v : def);
}

const char *ran_mod_env_require(const RanValue *argv, int64_t argc) {
    const char *key = marg_str(argv, argc, 0, "");
    const char *v = getenv(key);
    if (!v) {
        char msg[256];
        snprintf(msg, sizeof(msg), "required environment variable `%s` is not set", key);
        ran_fault("E1005", msg, "set it in the environment or in a .env file (env.load)");
    }
    return dup_str(v);
}

bool ran_mod_env_has(const RanValue *argv, int64_t argc) {
    return getenv(marg_str(argv, argc, 0, "")) != NULL;
}

bool ran_mod_env_set(const RanValue *argv, int64_t argc) {
    setenv(marg_str(argv, argc, 0, ""), marg_str(argv, argc, 1, ""), 1);
    return true;
}

bool ran_mod_env_unset(const RanValue *argv, int64_t argc) {
    unsetenv(marg_str(argv, argc, 0, ""));
    return true;
}

int64_t ran_mod_env_int(const RanValue *argv, int64_t argc) {
    const char *key = marg_str(argv, argc, 0, "");
    int64_t def = marg_int(argv, argc, 1, 0);
    const char *v = getenv(key);
    if (!v) return def;
    /* `v.trim().parse::<i64>().unwrap_or(def)` — whole trimmed text must parse. */
    const char *a = v; const char *z = v + strlen(v);
    while (a < z && (*a==' '||*a=='\t'||*a=='\n'||*a=='\r'||*a=='\f'||*a=='\v')) a++;
    while (z > a && (z[-1]==' '||z[-1]=='\t'||z[-1]=='\n'||z[-1]=='\r'||z[-1]=='\f'||z[-1]=='\v')) z--;
    size_t n = (size_t)(z - a);
    char *buf = (char *)ran_xalloc(n + 1);
    memcpy(buf, a, n); buf[n] = '\0';
    char *end = NULL;
    long long ll = strtoll(buf, &end, 10);
    int64_t out = (end && *end == '\0' && end != buf) ? (int64_t)ll : def;
    free(buf);
    return out;
}

double ran_mod_env_float(const RanValue *argv, int64_t argc) {
    const char *key = marg_str(argv, argc, 0, "");
    double def = marg_f64(argv, argc, 1);
    const char *v = getenv(key);
    if (!v) return def;
    const char *a = v; const char *z = v + strlen(v);
    while (a < z && (*a==' '||*a=='\t'||*a=='\n'||*a=='\r'||*a=='\f'||*a=='\v')) a++;
    while (z > a && (z[-1]==' '||z[-1]=='\t'||z[-1]=='\n'||z[-1]=='\r'||z[-1]=='\f'||z[-1]=='\v')) z--;
    size_t n = (size_t)(z - a);
    char *buf = (char *)ran_xalloc(n + 1);
    memcpy(buf, a, n); buf[n] = '\0';
    char *end = NULL;
    double d = strtod(buf, &end);
    double out = (end && *end == '\0' && end != buf) ? d : def;
    free(buf);
    return out;
}

bool ran_mod_env_bool(const RanValue *argv, int64_t argc) {
    const char *key = marg_str(argv, argc, 0, "");
    bool def = ran_truthy(marg_val(argv, argc, 1));
    const char *v = getenv(key);
    if (!v) return def;
    bool ok = false;
    bool parsed = env_parse_bool(v, &ok);
    return ok ? parsed : def;
}

RanValue ran_mod_env_decimal(const RanValue *argv, int64_t argc) {
    const char *key = marg_str(argv, argc, 0, "");
    const char *def = marg_str(argv, argc, 1, "0");
    const char *v = getenv(key);
    const char *raw = v ? v : def;
    Dec d;
    if (dec_parse_str(raw, &d)) return dec_value(d.mant, d.scale);
    if (dec_parse_str(def, &d)) return dec_value(d.mant, d.scale);
    return dec_value(0, 0);
}

RanValue ran_mod_env_all(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
    RanValue map = ran_map_new();
    if (environ) {
        for (char **e = environ; *e; e++) {
            const char *entry = *e;
            const char *eq = strchr(entry, '=');
            if (!eq) continue;
            size_t klen = (size_t)(eq - entry);
            char *key = (char *)ran_xalloc(klen + 1);
            memcpy(key, entry, klen); key[klen] = '\0';
            ran_map_set(map, key, ran_from_str(eq + 1));
            free(key);
        }
    }
    return map;
}

/* dotenv loader: KEY=value / export KEY=value, '#' comments, blank lines, and
 * single/double-quoted values; trailing " #" inline comments stripped on
 * unquoted values. Returns the count of variables set. When !override, existing
 * process variables are left untouched. Mirrors `load_dotenv`. */
static int64_t env_load_dotenv(const char *path, bool override_existing) {
    FILE *f = fopen(path, "rb");
    if (!f) return 0; /* missing file is not an error */
    /* Slurp the whole file. */
    if (fseek(f, 0, SEEK_END) != 0) { fclose(f); return 0; }
    long size = ftell(f);
    if (size < 0) { fclose(f); return 0; }
    rewind(f);
    char *text = (char *)ran_xalloc((size_t)size + 1);
    size_t got = fread(text, 1, (size_t)size, f);
    text[got] = '\0';
    fclose(f);

    int64_t count = 0;
    char *save = text;
    char *line;
    /* Iterate lines (split on '\n'; tolerate '\r'). */
    while ((line = save) != NULL) {
        char *nl = strchr(save, '\n');
        if (nl) { *nl = '\0'; save = nl + 1; } else { save = NULL; }
        /* trim line */
        char *a = line;
        char *z = line + strlen(line);
        while (a < z && (*a==' '||*a=='\t'||*a=='\r'||*a=='\f'||*a=='\v')) a++;
        while (z > a && (z[-1]==' '||z[-1]=='\t'||z[-1]=='\r'||z[-1]=='\f'||z[-1]=='\v')) z--;
        *z = '\0';
        if (*a == '\0' || *a == '#') continue;
        if (strncmp(a, "export ", 7) == 0) {
            a += 7;
            while (*a==' '||*a=='\t') a++;
        }
        char *eq = strchr(a, '=');
        if (!eq) continue;
        /* key = trim(a..eq), val = trim(eq+1..) */
        char *ka = a, *kz = eq;
        while (ka < kz && (*ka==' '||*ka=='\t')) ka++;
        while (kz > ka && (kz[-1]==' '||kz[-1]=='\t')) kz--;
        if (ka == kz) continue;
        char *va = eq + 1, *vz = a + strlen(a);
        while (va < vz && (*va==' '||*va=='\t')) va++;
        while (vz > va && (vz[-1]==' '||vz[-1]=='\t')) vz--;

        size_t klen = (size_t)(kz - ka);
        char *key = (char *)ran_xalloc(klen + 1);
        memcpy(key, ka, klen); key[klen] = '\0';

        size_t vlen = (size_t)(vz - va);
        char *val;
        if (vlen >= 2 && ((va[0]=='"' && vz[-1]=='"') || (va[0]=='\'' && vz[-1]=='\''))) {
            /* strip surrounding matching quotes */
            size_t inner = vlen - 2;
            val = (char *)ran_xalloc(inner + 1);
            memcpy(val, va + 1, inner); val[inner] = '\0';
        } else {
            /* trim trailing inline comment " #" on unquoted values */
            char *cut = NULL;
            for (char *p = va; p + 1 < vz; p++) {
                if (p[0] == ' ' && p[1] == '#') { cut = p; break; }
            }
            char *vend = cut ? cut : vz;
            while (vend > va && (vend[-1]==' '||vend[-1]=='\t')) vend--;
            size_t n = (size_t)(vend - va);
            val = (char *)ran_xalloc(n + 1);
            memcpy(val, va, n); val[n] = '\0';
        }

        if (!override_existing && getenv(key) != NULL) {
            free(key); free(val);
            continue;
        }
        setenv(key, val, 1);
        count++;
        free(key);
        free(val);
    }
    free(text);
    return count;
}

int64_t ran_mod_env_load(const RanValue *argv, int64_t argc) {
    return env_load_dotenv(marg_str(argv, argc, 0, ".env"), false);
}
int64_t ran_mod_env_load_override(const RanValue *argv, int64_t argc) {
    return env_load_dotenv(marg_str(argv, argc, 0, ".env"), true);
}
int64_t ran_mod_env_load_default(const RanValue *argv, int64_t argc) {
    (void)argv; (void)argc;
    return env_load_dotenv(".env", false);
}
