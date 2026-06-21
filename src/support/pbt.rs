//! Property-Based Testing harness — std-only, zero external crates.
//!
//! Konsisten dengan filosofi proyek (zero external crates / std-only), modul ini
//! menyediakan harness PBT internal kecil: RNG deterministik seedable, abstraksi
//! generator + shrinker minimal, dan property runner. Tidak ada crate eksternal.
//!
//! Dipakai oleh property test (`prop_*`) yang memetakan Correctness Properties
//! P1–P11 dari `design.md` (lihat Testing Strategy → Konfigurasi PBT).
//!
//! ## Konvensi tag komentar
//!
//! Setiap property test WAJIB diawali komentar tag yang merujuk property desain:
//!
//! ```text
//! // Feature: enterprise-runtime-capabilities, Property {n}: {teks property}
//! ```
//!
//! Gunakan [`property_tag`] untuk membentuk string tag tersebut secara konsisten,
//! atau salin manual di atas fungsi test.
//!
//! ## Konfigurasi (env)
//!
//! - `RAN_PBT_CASES` — override jumlah kasus per property (default [`DEFAULT_CASES`] = 100, minimum 100).
//! - `RAN_PBT_SEED`  — paksa seed master tertentu untuk reproduksi kegagalan.
//!
//! ## Availability-skip (test FFI)
//!
//! Test FFI yang butuh pustaka sistem (mis. `libsqlite3`) memakai
//! [`skip_if_unavailable`] agar di-skip dengan pesan jelas (bukan gagal) bila
//! pustaka absen — CI tetap hijau.
//!
//! ## Berkas sementara
//!
//! Artefak uji ditulis ke `.tmp_tests/` (gitignored). Gunakan [`unique_tmp_path`].
//!
//! Modul ini hanya dikompilasi pada konfigurasi test (`#[cfg(test)]`).

#![allow(dead_code)]

use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Nama fitur untuk konvensi tag komentar property test.
pub const FEATURE_NAME: &str = "enterprise-runtime-capabilities";

/// Jumlah kasus default per property test (minimum sesuai desain).
pub const DEFAULT_CASES: usize = 100;

/// Bentuk string tag komentar konvensi untuk Property `n`.
///
/// ```text
/// // Feature: enterprise-runtime-capabilities, Property {n}: {text}
/// ```
pub fn property_tag(n: u32, text: &str) -> String {
    format!("// Feature: {}, Property {}: {}", FEATURE_NAME, n, text)
}

// ============================================================================
// RNG deterministik (SplitMix64) — std-only.
// ============================================================================

/// RNG deterministik seedable berbasis SplitMix64.
///
/// Cepat, kualitas cukup untuk pengujian, dan sepenuhnya reproducible dari seed
/// `u64`. Mencetak seed saat gagal memungkinkan reproduksi pasti.
#[derive(Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Buat RNG baru dari seed.
    pub fn new(seed: u64) -> Self {
        Rng { state: seed }
    }

    /// Hasilkan `u64` acak berikutnya (SplitMix64).
    pub fn next_u64(&mut self) -> u64 {
        // SplitMix64: konstanta standar.
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Hasilkan `u32` acak.
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Bilangan acak dalam `[0, n)`; mengembalikan 0 bila `n == 0`.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        // Rejection sampling untuk distribusi seragam tanpa modulo-bias.
        let zone = u64::MAX - (u64::MAX % n);
        loop {
            let v = self.next_u64();
            if v < zone {
                return v % n;
            }
        }
    }

    /// Bilangan acak dalam `[lo, hi]` (inklusif). Bila `lo > hi`, kembalikan `lo`.
    pub fn range_i64(&mut self, lo: i64, hi: i64) -> i64 {
        if lo >= hi {
            return lo;
        }
        // Lebar bisa melebihi u64::MAX hanya bila lo=i64::MIN & hi=i64::MAX → tangani.
        let span = (hi as i128 - lo as i128 + 1) as u128;
        if span >= u64::MAX as u128 {
            return self.next_u64() as i64;
        }
        let off = self.below(span as u64);
        (lo as i128 + off as i128) as i64
    }

    /// `usize` dalam `[0, n]` inklusif.
    pub fn upto(&mut self, n: usize) -> usize {
        self.below(n as u64 + 1) as usize
    }

    /// Boolean acak.
    pub fn boolean(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }

    /// `f64` seragam dalam `[0, 1)`.
    pub fn unit_f64(&mut self) -> f64 {
        // 53 bit mantissa.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Pilih indeks acak ke dalam slice; panik bila slice kosong.
    pub fn choose<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let i = self.below(items.len() as u64) as usize;
        &items[i]
    }
}

/// Hasilkan seed master non-deterministik (waktu + counter proses).
pub fn random_seed() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    // Campur dengan SplitMix64 agar bit tersebar baik.
    let mut r = Rng::new(nanos ^ c.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    r.next_u64()
}

/// Turunkan seed iterasi dari seed master + indeks (deterministik & reproducible).
fn derive_seed(master: u64, iter: usize) -> u64 {
    let mut r = Rng::new(master ^ (iter as u64).wrapping_mul(0xD1B5_4A32_D192_ED03));
    r.next_u64()
}

// ============================================================================
// Generator + shrinker.
// ============================================================================

/// Generator nilai bertipe `T` dengan kemampuan shrink (memperkecil counterexample).
///
/// Direpresentasikan sebagai pasangan closure (`generate`, `shrink`) agar dapat
/// dikomposisi tanpa boilerplate trait. `size` adalah petunjuk ukuran (0..=max_size)
/// yang tumbuh seiring iterasi sehingga kasus awal kecil dan makin besar.
#[derive(Clone)]
pub struct Gen<T> {
    generate: Rc<dyn Fn(&mut Rng, usize) -> T>,
    shrink: Rc<dyn Fn(&T) -> Vec<T>>,
}

impl<T: Clone + 'static> Gen<T> {
    /// Buat generator dari closure generate + shrink.
    pub fn new(
        generate: impl Fn(&mut Rng, usize) -> T + 'static,
        shrink: impl Fn(&T) -> Vec<T> + 'static,
    ) -> Self {
        Gen {
            generate: Rc::new(generate),
            shrink: Rc::new(shrink),
        }
    }

    /// Hasilkan satu nilai.
    pub fn generate(&self, rng: &mut Rng, size: usize) -> T {
        (self.generate)(rng, size)
    }

    /// Kandidat shrink (lebih sederhana) dari sebuah nilai.
    pub fn shrink_candidates(&self, value: &T) -> Vec<T> {
        (self.shrink)(value)
    }

    /// Konstanta (tanpa shrink).
    pub fn just(value: T) -> Self {
        Gen::new(move |_, _| value.clone(), |_| Vec::new())
    }
}

// ---- Konstruktor primitif --------------------------------------------------

/// Boolean. Shrink `true` → `false`.
pub fn bool_gen() -> Gen<bool> {
    Gen::new(
        |rng, _| rng.boolean(),
        |b| if *b { vec![false] } else { Vec::new() },
    )
}

/// Shrink integer ke arah nol: kandidat 0, separuh, dan ±1 mendekat.
fn shrink_i64(v: i64) -> Vec<i64> {
    if v == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    out.push(0);
    // Bagi dua berulang menuju nol.
    let mut half = v / 2;
    if half != 0 && half != v {
        out.push(half);
    }
    half /= 2;
    if half != 0 && !out.contains(&half) {
        out.push(half);
    }
    // Satu langkah mendekat ke nol.
    let step = if v > 0 { v - 1 } else { v + 1 };
    if step != 0 && !out.contains(&step) {
        out.push(step);
    }
    out
}

/// `i64` dalam rentang inklusif `[lo, hi]`. Shrink menuju nol (di-clamp ke rentang).
pub fn i64_range(lo: i64, hi: i64) -> Gen<i64> {
    Gen::new(
        move |rng, _| rng.range_i64(lo, hi),
        move |v| {
            shrink_i64(*v)
                .into_iter()
                .map(|c| c.clamp(lo, hi))
                .filter(|c| *c != *v)
                .collect()
        },
    )
}

/// `i64` penuh termasuk edge case (`0, 1, -1, MIN, MAX`, dsb). Shrink menuju nol.
pub fn i64_any() -> Gen<i64> {
    const EDGES: &[i64] = &[
        0,
        1,
        -1,
        2,
        -2,
        i64::MAX,
        i64::MIN,
        i64::MAX - 1,
        i64::MIN + 1,
        i32::MAX as i64,
        i32::MIN as i64,
        1 << 53,        // batas presisi f64 (relevan konversi Ran↔JS)
        -(1 << 53),
        (1i64 << 53) + 1,
    ];
    Gen::new(
        |rng, _| {
            // 25% pilih edge case, sisanya acak penuh.
            if rng.below(4) == 0 {
                *rng.choose(EDGES)
            } else {
                rng.next_u64() as i64
            }
        },
        |v| shrink_i64(*v),
    )
}

/// `f64` termasuk nilai khusus (`NaN`, `±inf`, `-0.0`). Shrink menuju nilai sederhana.
pub fn f64_any() -> Gen<f64> {
    const SPECIAL: &[f64] = &[
        0.0,
        -0.0,
        1.0,
        -1.0,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        f64::EPSILON,
    ];
    Gen::new(
        |rng, _| {
            match rng.below(3) {
                0 => *rng.choose(SPECIAL),
                1 => {
                    // Bit pattern acak (mencakup subnormal/NaN beragam).
                    f64::from_bits(rng.next_u64())
                }
                _ => {
                    // Nilai "wajar" berskala.
                    let mant = rng.unit_f64() * 2.0 - 1.0;
                    let exp = rng.range_i64(-12, 12) as f64;
                    mant * 10f64.powf(exp)
                }
            }
        },
        |v| {
            // Jangan shrink NaN/inf (tidak ada urutan bermakna); selain itu menuju 0.
            if v.is_nan() || v.is_infinite() {
                return Vec::new();
            }
            let mut out = Vec::new();
            if *v != 0.0 {
                out.push(0.0);
            }
            let t = v.trunc();
            if t != *v {
                out.push(t);
            }
            let half = v / 2.0;
            if half != *v && half.is_finite() {
                out.push(half);
            }
            out
        },
    )
}

/// `char` dalam rentang Unicode valid (termasuk multi-byte UTF-8).
pub fn char_unicode() -> Gen<char> {
    Gen::new(
        |rng, _| loop {
            // Sebar di berbagai blok: ASCII, Latin-1, BMP, astral.
            let cp = match rng.below(4) {
                0 => rng.below(0x80),
                1 => rng.below(0x100),
                2 => rng.below(0x1_0000),
                _ => rng.below(0x11_0000),
            } as u32;
            if let Some(c) = char::from_u32(cp) {
                return c;
            }
        },
        |c| {
            // Shrink menuju 'a', lalu '\0'.
            if *c == 'a' {
                Vec::new()
            } else if *c == '\0' {
                Vec::new()
            } else {
                vec!['a', '\0']
            }
        },
    )
}

/// String UTF-8 dengan panjang hingga `max_len` (digabung petunjuk `size`).
/// Shrink: perpendek (buang karakter) lalu sederhanakan karakter.
pub fn string(max_len: usize) -> Gen<String> {
    let ch = char_unicode();
    Gen::new(
        move |rng, size| {
            let cap = max_len.min(size.max(1) + 1).max(1);
            let len = rng.upto(cap);
            let mut s = String::new();
            for _ in 0..len {
                s.push(ch.generate(rng, size));
            }
            s
        },
        |s| {
            let chars: Vec<char> = s.chars().collect();
            let mut out = Vec::new();
            if chars.is_empty() {
                return out;
            }
            // Kandidat string kosong.
            out.push(String::new());
            // Buang separuh awal / separuh akhir.
            let half = chars.len() / 2;
            if half > 0 {
                out.push(chars[..half].iter().collect());
                out.push(chars[half..].iter().collect());
            }
            // Buang satu karakter (dari depan).
            if chars.len() > 1 {
                out.push(chars[1..].iter().collect());
            }
            out
        },
    )
}

/// `Vec<T>` dengan panjang hingga `max_len`. Shrink: kurangi elemen, lalu shrink isi.
pub fn vec_of<T: Clone + 'static>(inner: Gen<T>, max_len: usize) -> Gen<Vec<T>> {
    let inner_gen = inner.clone();
    let inner_shrink = inner;
    Gen::new(
        move |rng, size| {
            let cap = max_len.min(size.max(1) + 1).max(1);
            let len = rng.upto(cap);
            (0..len).map(|_| inner_gen.generate(rng, size)).collect()
        },
        move |v: &Vec<T>| {
            let mut out: Vec<Vec<T>> = Vec::new();
            if v.is_empty() {
                return out;
            }
            // Kandidat kosong.
            out.push(Vec::new());
            // Buang separuh.
            let half = v.len() / 2;
            if half > 0 {
                out.push(v[..half].to_vec());
                out.push(v[half..].to_vec());
            }
            // Buang satu elemen pada tiap posisi.
            for i in 0..v.len() {
                let mut c = v.clone();
                c.remove(i);
                out.push(c);
            }
            // Shrink satu elemen (elemen pertama yang punya kandidat).
            for i in 0..v.len() {
                let cands = inner_shrink.shrink_candidates(&v[i]);
                if let Some(first) = cands.into_iter().next() {
                    let mut c = v.clone();
                    c[i] = first;
                    out.push(c);
                    break;
                }
            }
            out
        },
    )
}

/// Pilih secara acak salah satu dari beberapa generator (uniform).
pub fn one_of<T: Clone + 'static>(gens: Vec<Gen<T>>) -> Gen<T> {
    assert!(!gens.is_empty(), "one_of membutuhkan minimal satu generator");
    let gens = Rc::new(gens);
    let gens_gen = gens.clone();
    Gen::new(
        move |rng, size| {
            let i = rng.below(gens_gen.len() as u64) as usize;
            gens_gen[i].generate(rng, size)
        },
        // Shrink generik tidak tahu generator asal; serahkan ke struktur nilai
        // (untuk tipe komposit, gunakan generator khusus seperti `value_tree`).
        move |_| Vec::new(),
    )
}

// ============================================================================
// Pohon nilai rekursif terbatas (untuk round-trip props P1/P2).
// ============================================================================

/// Nilai mirip-JSON yang merepresentasikan nilai Ran yang dapat ditukar dengan
/// JS/SQLite (`void↔null`, bool, int, float, str, array, map).
///
/// Dipakai property round-trip P1 (Ran↔JS) dan generator nilai P2 (SQLite).
#[derive(Clone, Debug, PartialEq)]
pub enum ValueTree {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<ValueTree>),
    Map(Vec<(String, ValueTree)>),
}

/// Generator pohon nilai rekursif dengan kedalaman maksimum `max_depth`
/// (mis. ≤64 untuk P1) dan fan-out terbatas.
pub fn value_tree(max_depth: usize) -> Gen<ValueTree> {
    Gen::new(
        move |rng, size| gen_value(rng, max_depth, size),
        shrink_value,
    )
}

fn gen_scalar(rng: &mut Rng) -> ValueTree {
    match rng.below(5) {
        0 => ValueTree::Null,
        1 => ValueTree::Bool(rng.boolean()),
        2 => ValueTree::Int(rng.next_u64() as i64),
        3 => {
            // f64 wajar + sesekali khusus.
            if rng.below(5) == 0 {
                ValueTree::Float(*rng.choose(&[0.0_f64, -0.0, f64::NAN, f64::INFINITY]))
            } else {
                ValueTree::Float(rng.unit_f64() * 1000.0 - 500.0)
            }
        }
        _ => {
            let len = rng.upto(6);
            let mut s = String::new();
            let ch = char_unicode();
            for _ in 0..len {
                s.push(ch.generate(rng, 4));
            }
            ValueTree::Str(s)
        }
    }
}

fn gen_value(rng: &mut Rng, depth_left: usize, size: usize) -> ValueTree {
    // Pada depth 0 atau dengan probabilitas, hasilkan scalar (basis rekursi).
    if depth_left == 0 || rng.below(3) == 0 {
        return gen_scalar(rng);
    }
    let fanout_cap = 4usize.min(size.max(1) + 1);
    if rng.boolean() {
        let len = rng.upto(fanout_cap);
        let items = (0..len)
            .map(|_| gen_value(rng, depth_left - 1, size))
            .collect();
        ValueTree::List(items)
    } else {
        let len = rng.upto(fanout_cap);
        let ch = char_unicode();
        let mut entries = Vec::new();
        for _ in 0..len {
            let klen = rng.upto(4);
            let mut k = String::new();
            for _ in 0..klen {
                k.push(ch.generate(rng, 4));
            }
            entries.push((k, gen_value(rng, depth_left - 1, size)));
        }
        ValueTree::Map(entries)
    }
}

fn shrink_value(v: &ValueTree) -> Vec<ValueTree> {
    match v {
        ValueTree::Null => Vec::new(),
        ValueTree::Bool(b) => {
            if *b {
                vec![ValueTree::Null, ValueTree::Bool(false)]
            } else {
                vec![ValueTree::Null]
            }
        }
        ValueTree::Int(i) => {
            let mut out = vec![ValueTree::Null];
            out.extend(shrink_i64(*i).into_iter().map(ValueTree::Int));
            out
        }
        ValueTree::Float(f) => {
            let mut out = vec![ValueTree::Null];
            if f.is_finite() && *f != 0.0 {
                out.push(ValueTree::Float(0.0));
                out.push(ValueTree::Float(f.trunc()));
            }
            out
        }
        ValueTree::Str(s) => {
            let mut out = vec![ValueTree::Null];
            if !s.is_empty() {
                out.push(ValueTree::Str(String::new()));
                let half: String = s.chars().take(s.chars().count() / 2).collect();
                out.push(ValueTree::Str(half));
            }
            out
        }
        ValueTree::List(items) => {
            let mut out = vec![ValueTree::Null, ValueTree::List(Vec::new())];
            // Buang satu elemen.
            for i in 0..items.len() {
                let mut c = items.clone();
                c.remove(i);
                out.push(ValueTree::List(c));
            }
            // Promosikan elemen tunggal (kurangi kedalaman).
            if items.len() == 1 {
                out.push(items[0].clone());
            }
            out
        }
        ValueTree::Map(entries) => {
            let mut out = vec![ValueTree::Null, ValueTree::Map(Vec::new())];
            for i in 0..entries.len() {
                let mut c = entries.clone();
                c.remove(i);
                out.push(ValueTree::Map(c));
            }
            if entries.len() == 1 {
                out.push(entries[0].1.clone());
            }
            out
        }
    }
}

// ============================================================================
// Property runner.
// ============================================================================

/// Konfigurasi runner PBT, dibaca dari environment.
#[derive(Clone, Debug)]
pub struct Config {
    /// Jumlah kasus per property (≥ [`DEFAULT_CASES`]).
    pub cases: usize,
    /// Petunjuk ukuran maksimum untuk generator.
    pub max_size: usize,
    /// Seed master eksplisit (untuk reproduksi), bila ada.
    pub seed: Option<u64>,
}

impl Config {
    /// Baca konfigurasi dari env (`RAN_PBT_CASES`, `RAN_PBT_SEED`).
    /// Jumlah kasus selalu di-clamp minimal ke [`DEFAULT_CASES`] (100).
    pub fn from_env() -> Self {
        let cases = std::env::var("RAN_PBT_CASES")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .map(|n| n.max(DEFAULT_CASES))
            .unwrap_or(DEFAULT_CASES);
        let seed = std::env::var("RAN_PBT_SEED")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok());
        Config {
            cases,
            max_size: 50,
            seed,
        }
    }
}

/// Statistik hasil sukses.
#[derive(Clone, Debug)]
pub struct RunStats {
    pub cases: usize,
    pub seed: u64,
}

/// Detail kegagalan property: cukup untuk reproduksi + counterexample terkecil.
#[derive(Clone)]
pub struct Failure<T> {
    /// Seed master (set `RAN_PBT_SEED=<seed>` untuk mereproduksi).
    pub seed: u64,
    /// Indeks iterasi yang gagal.
    pub iter: usize,
    /// Seed iterasi yang gagal.
    pub iter_seed: u64,
    /// Counterexample asli (sebelum shrink).
    pub original: T,
    /// Counterexample setelah shrink.
    pub shrunk: T,
    /// Jumlah langkah shrink yang berhasil.
    pub shrink_steps: usize,
}

impl<T: fmt::Debug> fmt::Display for Failure<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "property GAGAL\n  seed master : {seed}\n  iterasi     : {iter} (iter_seed={iter_seed})\n  counterexample (shrunk, {steps} langkah): {shrunk:?}\n  counterexample (asli)                    : {orig:?}\n  reproduksi  : set -x RAN_PBT_SEED {seed}  (fish)  lalu jalankan ulang test ini",
            seed = self.seed,
            iter = self.iter,
            iter_seed = self.iter_seed,
            steps = self.shrink_steps,
            shrunk = self.shrunk,
            orig = self.original,
        )
    }
}

impl<T: fmt::Debug> fmt::Debug for Failure<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Jalankan property dan tangkap panik sebagai kegagalan (mengembalikan `false`).
fn check<T, P: Fn(&T) -> bool>(prop: &P, value: &T) -> bool {
    // Bungkam panic hook sementara agar output tidak berisik saat probing/shrinking.
    let prev = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let result = panic::catch_unwind(AssertUnwindSafe(|| prop(value)));
    panic::set_hook(prev);
    result.unwrap_or(false)
}

/// Cari counterexample terkecil via shrinking greedy.
fn shrink_to_minimal<T: Clone + 'static, P: Fn(&T) -> bool>(
    gen: &Gen<T>,
    initial: &T,
    prop: &P,
) -> (T, usize) {
    let mut current = initial.clone();
    let mut steps = 0usize;
    // Batas keras agar tak berputar lama pada generator dengan banyak kandidat.
    const MAX_STEPS: usize = 5000;
    loop {
        let candidates = gen.shrink_candidates(&current);
        let mut progressed = false;
        for cand in candidates {
            // Kandidat masih gagal (property false) ⇒ lebih kecil tapi tetap counterexample.
            if !check(prop, &cand) {
                current = cand;
                steps += 1;
                progressed = true;
                break;
            }
        }
        if !progressed || steps >= MAX_STEPS {
            break;
        }
    }
    (current, steps)
}

/// Inti runner: kembalikan `Ok(stats)` bila semua kasus lolos, `Err(failure)` bila ada
/// counterexample (sudah di-shrink). Tidak panik.
pub fn run<T, P>(gen: &Gen<T>, prop: P) -> Result<RunStats, Failure<T>>
where
    T: Clone + 'static,
    P: Fn(&T) -> bool,
{
    let cfg = Config::from_env();
    let master = cfg.seed.unwrap_or_else(random_seed);
    for iter in 0..cfg.cases {
        let iter_seed = derive_seed(master, iter);
        let mut rng = Rng::new(iter_seed);
        // Ukuran tumbuh linear dari kecil → max_size.
        let size = if cfg.cases <= 1 {
            cfg.max_size
        } else {
            iter * cfg.max_size / cfg.cases
        };
        let value = gen.generate(&mut rng, size);
        if !check(&prop, &value) {
            let (shrunk, shrink_steps) = shrink_to_minimal(gen, &value, &prop);
            return Err(Failure {
                seed: master,
                iter,
                iter_seed,
                original: value,
                shrunk,
                shrink_steps,
            });
        }
    }
    Ok(RunStats {
        cases: cfg.cases,
        seed: master,
    })
}

/// Jalankan property; panik (gagalkan test) dengan pesan + seed bila ada counterexample.
///
/// `name` muncul pada pesan kegagalan. Property mengembalikan `true` bila invariant
/// dipertahankan untuk masukan tersebut.
pub fn for_all<T, P>(name: &str, gen: &Gen<T>, prop: P)
where
    T: Clone + fmt::Debug + 'static,
    P: Fn(&T) -> bool,
{
    match run(gen, prop) {
        Ok(_) => {}
        Err(failure) => panic!("[{}] {}", name, failure),
    }
}

// ============================================================================
// Availability-skip helper untuk test FFI.
// ============================================================================

/// Kandidat nama pustaka SQLite3 lintas-platform.
pub const SQLITE_LIBS: &[&str] = &[
    "libsqlite3.so",
    "libsqlite3.so.0",
    "libsqlite3.dylib",
    "sqlite3.dll",
];

#[cfg(unix)]
mod dl {
    #![allow(non_camel_case_types)]
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int, c_void};

    extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
    }

    // RTLD_NOW: resolve semua simbol saat load.
    const RTLD_NOW: c_int = 2;

    /// Coba `dlopen` nama pustaka; `true` bila pustaka dapat dimuat.
    pub fn can_load(name: &str) -> bool {
        let cname = match CString::new(name) {
            Ok(c) => c,
            Err(_) => return false,
        };
        unsafe {
            let h = dlopen(cname.as_ptr(), RTLD_NOW);
            if h.is_null() {
                false
            } else {
                dlclose(h);
                true
            }
        }
    }
}

#[cfg(not(unix))]
mod dl {
    /// Platform non-unix: probing dinamis tak didukung; konservatif `false`.
    pub fn can_load(_name: &str) -> bool {
        false
    }
}

/// `true` bila salah satu kandidat pustaka tersedia (dapat dimuat) di runtime.
pub fn library_available(candidates: &[&str]) -> bool {
    candidates.iter().any(|name| dl::can_load(name))
}

/// Periksa ketersediaan pustaka FFI untuk test. Bila absen, cetak pesan SKIP yang
/// jelas dan kembalikan `true` (penanda agar test `return` lebih awal — sukses,
/// bukan gagal). Bila tersedia, kembalikan `false`.
///
/// Pola pemakaian pada test FFI:
///
/// ```ignore
/// #[test]
/// fn prop_db_roundtrip() {
///     if pbt::skip_if_unavailable("libsqlite3", pbt::SQLITE_LIBS) {
///         return;
///     }
///     // ... jalankan property dengan FFI nyata ...
/// }
/// ```
pub fn skip_if_unavailable(lib_label: &str, candidates: &[&str]) -> bool {
    if library_available(candidates) {
        false
    } else {
        println!("SKIPPED: {} not available", lib_label);
        true
    }
}

// ============================================================================
// Berkas sementara (.tmp_tests/, gitignored).
// ============================================================================

/// Kembalikan (dan buat jika perlu) direktori `.tmp_tests/`.
pub fn tmp_dir() -> PathBuf {
    let dir = PathBuf::from(".tmp_tests");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Bentuk path unik di `.tmp_tests/` dengan `prefix` dan ekstensi `ext` (tanpa titik).
/// Unik per pid + nanos + counter atomic, jadi aman untuk test paralel.
pub fn unique_tmp_path(prefix: &str, ext: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = if ext.is_empty() {
        format!("{}_{}_{}_{}", prefix, pid, nanos, n)
    } else {
        format!("{}_{}_{}_{}.{}", prefix, pid, nanos, n, ext)
    };
    tmp_dir().join(name)
}

// ============================================================================
// Self-tests untuk harness (di-gate agar tidak menggagalkan CI).
// ============================================================================

#[cfg(test)]
mod self_tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_for_same_seed() {
        let mut a = Rng::new(12345);
        let mut b = Rng::new(12345);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn rng_below_is_in_range() {
        let mut r = Rng::new(7);
        for _ in 0..10_000 {
            let v = r.below(10);
            assert!(v < 10);
        }
        assert_eq!(r.below(0), 0);
    }

    #[test]
    fn rng_range_i64_respects_bounds() {
        let mut r = Rng::new(99);
        for _ in 0..10_000 {
            let v = r.range_i64(-5, 5);
            assert!((-5..=5).contains(&v));
        }
        // Rentang ekstrem tak panik / overflow.
        let _ = r.range_i64(i64::MIN, i64::MAX);
    }

    #[test]
    fn property_tag_format_matches_convention() {
        let tag = property_tag(1, "Integritas & keamanan penyajian aset web");
        assert_eq!(
            tag,
            "// Feature: enterprise-runtime-capabilities, Property 1: Integritas & keamanan penyajian aset web"
        );
    }

    #[test]
    fn config_default_is_at_least_100() {
        // Tanpa env override, minimal 100 (memvalidasi default; tak mengubah env global).
        let cfg = Config::from_env();
        assert!(cfg.cases >= DEFAULT_CASES);
    }

    #[test]
    fn passing_property_succeeds() {
        // Property selalu benar: abs(x) >= 0 untuk semua kecuali i64::MIN (tangani).
        let gen = i64_any();
        let stats = run(&gen, |x: &i64| x.wrapping_abs() >= 0 || *x == i64::MIN)
            .expect("property yang benar harus lolos");
        assert!(stats.cases >= DEFAULT_CASES);
    }

    #[test]
    fn value_tree_respects_max_depth() {
        fn depth(v: &ValueTree) -> usize {
            match v {
                ValueTree::List(items) => {
                    1 + items.iter().map(depth).max().unwrap_or(0)
                }
                ValueTree::Map(entries) => {
                    1 + entries.iter().map(|(_, v)| depth(v)).max().unwrap_or(0)
                }
                _ => 0,
            }
        }
        let gen = value_tree(4);
        let mut rng = Rng::new(2024);
        for _ in 0..2000 {
            let v = gen.generate(&mut rng, 20);
            assert!(depth(&v) <= 4, "kedalaman {} melebihi 4: {:?}", depth(&v), v);
        }
    }

    #[test]
    fn failing_property_reports_seed_and_shrinks() {
        // Property sengaja salah: "semua vektor punya panjang < 3".
        // Counterexample minimal adalah vektor panjang tepat 3.
        let gen = vec_of(i64_range(-100, 100), 12);
        let result = run(&gen, |v: &Vec<i64>| v.len() < 3);
        let failure = result.expect_err("property yang salah harus menghasilkan counterexample");

        // Seed tercatat untuk reproduksi.
        // (master seed acak; cukup pastikan field tersedia & pesan memuatnya.)
        let msg = format!("{}", failure);
        assert!(msg.contains("RAN_PBT_SEED"), "pesan harus berisi cara reproduksi seed: {msg}");
        assert!(msg.contains("counterexample"), "pesan harus menyebut counterexample");

        // Shrinking minimal: counterexample terkecil punya panjang tepat 3,
        // dan elemen-elemennya ter-shrink ke 0.
        assert_eq!(
            failure.shrunk.len(),
            3,
            "shrink harus menemukan counterexample minimal panjang 3, dapat: {:?}",
            failure.shrunk
        );
        assert!(
            failure.shrunk.iter().all(|x| *x == 0),
            "elemen counterexample minimal harus ter-shrink ke 0: {:?}",
            failure.shrunk
        );
        assert!(failure.shrink_steps > 0, "harus ada langkah shrink");
    }

    #[test]
    fn failing_property_is_reproducible_by_seed() {
        // Property salah; jalankan dengan seed tetap dua kali → counterexample asli identik.
        let gen = vec_of(i64_range(-50, 50), 10);
        let run_once = || {
            // Paksa seed master tertentu lewat Config manual (tanpa menyentuh env global).
            let master = 0xC0FFEE_u64;
            let cfg_cases = DEFAULT_CASES;
            for iter in 0..cfg_cases {
                let iter_seed = derive_seed(master, iter);
                let mut rng = Rng::new(iter_seed);
                let size = iter * 50 / cfg_cases;
                let v: Vec<i64> = gen.generate(&mut rng, size);
                if !(v.len() < 4) {
                    return Some((iter, v));
                }
            }
            None
        };
        let a = run_once();
        let b = run_once();
        assert_eq!(a, b, "seed tetap harus mereproduksi counterexample yang sama");
        assert!(a.is_some(), "harus menemukan counterexample untuk property salah");
    }

    #[test]
    fn library_available_handles_missing_lib() {
        // Nama pustaka yang pasti tidak ada → false (tanpa panik).
        assert!(!library_available(&["libdefinitely_not_a_real_lib_zzz.so"]));
    }

    #[test]
    fn skip_helper_returns_true_when_missing() {
        let skip = skip_if_unavailable("libnope_zzz", &["libnope_zzz.so"]);
        assert!(skip, "pustaka absen harus menghasilkan skip=true");
    }

    #[test]
    fn unique_tmp_path_is_under_tmp_tests_and_unique() {
        let a = unique_tmp_path("harness", "sqlite");
        let b = unique_tmp_path("harness", "sqlite");
        assert_ne!(a, b);
        assert!(a.starts_with(".tmp_tests"));
        assert!(a.to_string_lossy().ends_with(".sqlite"));
    }
}
