/* ran_rt.c — minimal C runtime for Ran native AOT codegen (Phase D, iteration D2).
 *
 * Precompiled once and linked into each native binary (it is NOT re-emitted per
 * program). The generated program calls into these helpers for echo, string
 * ops, the tagged `RanValue` data layer (decimal/array/object), checked
 * arithmetic, and value formatting.
 *
 * See ran_rt.h for the value model and safety contract.
 */
#include "ran_rt.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <stdatomic.h>

/* Abort with a Ran-style diagnostic and exit code 70 (the interpreter's
 * top-level fault exit code). */
static void ran_fault(const char *code, const char *message, const char *help) {
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

void ran_retain(RanValue v) {
    switch (v.tag) {
        case RAN_STR:    atomic_fetch_add(&v.u.s->rc, 1); break;
        case RAN_ARRAY:  atomic_fetch_add(&v.u.a->rc, 1); break;
        case RAN_OBJECT: atomic_fetch_add(&v.u.o->rc, 1); break;
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
        default:        return true;
    }
}

int64_t ran_len(RanValue v) {
    switch (v.tag) {
        case RAN_STR:   return (int64_t)v.u.s->len;
        case RAN_ARRAY: return (int64_t)v.u.a->len;
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
    }
    return "";
}
