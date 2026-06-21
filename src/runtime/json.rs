//! JSON parse/validate engine for the runtime (extracted from mod.rs).
//! Part of the `Environment` inherent impl; child module so it can use the
//! parent\x27s private types and helpers via `use super::*`.

use super::*;
use std::collections::HashMap;

impl Environment {
    // --- Simple JSON parser ---

    pub(super) fn parse_json(&self, s: &str) -> Value {
        let chars: Vec<char> = s.chars().collect();
        let mut pos = 0;
        let val = Self::json_value(&chars, &mut pos);
        val
    }

    /// Validate that `s` is a single well-formed JSON value with only
    /// whitespace trailing. Stricter than `decode` (which is tolerant).
    pub(super) fn json_is_valid(s: &str) -> bool {
        let t = s.trim();
        if t.is_empty() {
            return false;
        }
        let chars: Vec<char> = s.chars().collect();
        let mut pos = 0;
        Self::json_skip_ws(&chars, &mut pos);
        let start = pos;
        let _ = Self::json_value(&chars, &mut pos);
        if pos <= start {
            return false;
        }
        Self::json_skip_ws(&chars, &mut pos);
        pos >= chars.len()
    }

    /// Traverse a decoded value by a dotted path. Numeric segments index
    /// arrays; named segments index maps/objects. Returns Void if absent.
    pub(super) fn json_path_get(base: &Value, path: &str) -> Value {
        let mut current = base.clone();
        if path.is_empty() {
            return current;
        }
        for seg in path.split('.') {
            current = match current {
                Value::Map(ref m) => m.get(seg).cloned().unwrap_or(Value::Void),
                Value::Object(_, ref f) => f.get(seg).cloned().unwrap_or(Value::Void),
                Value::Array(ref a) => match seg.parse::<usize>() {
                    Ok(i) => a.get(i).cloned().unwrap_or(Value::Void),
                    Err(_) => return Value::Void,
                },
                _ => return Value::Void,
            };
        }
        current
    }

    fn json_skip_ws(chars: &[char], pos: &mut usize) {
        while *pos < chars.len() && chars[*pos].is_whitespace() {
            *pos += 1;
        }
    }

    fn json_value(chars: &[char], pos: &mut usize) -> Value {
        Self::json_skip_ws(chars, pos);
        if *pos >= chars.len() {
            return Value::Void;
        }
        match chars[*pos] {
            '{' => Self::json_object(chars, pos),
            '[' => Self::json_array(chars, pos),
            '"' => Value::Str(Self::json_string(chars, pos)),
            't' | 'f' => Self::json_bool(chars, pos),
            'n' => {
                // Consume "null" if present; otherwise advance one char safely.
                if Self::json_match_literal(chars, pos, "null") {
                    Value::Void
                } else {
                    *pos += 1;
                    Value::Void
                }
            }
            _ => Self::json_number(chars, pos),
        }
    }

    /// If `chars[pos..]` starts with `lit`, advance past it and return true.
    fn json_match_literal(chars: &[char], pos: &mut usize, lit: &str) -> bool {
        let lit_chars: Vec<char> = lit.chars().collect();
        if *pos + lit_chars.len() > chars.len() {
            return false;
        }
        for (k, &lc) in lit_chars.iter().enumerate() {
            if chars[*pos + k] != lc {
                return false;
            }
        }
        *pos += lit_chars.len();
        true
    }

    fn json_string(chars: &[char], pos: &mut usize) -> String {
        let mut out = String::new();
        *pos += 1; // opening quote
        while *pos < chars.len() && chars[*pos] != '"' {
            if chars[*pos] == '\\' && *pos + 1 < chars.len() {
                *pos += 1;
                match chars[*pos] {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000C}'),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'u' => {
                        // \uXXXX, with UTF-16 surrogate-pair support.
                        if let Some(cp) = Self::json_read_hex4(chars, pos) {
                            if (0xD800..=0xDBFF).contains(&cp)
                                && *pos + 2 < chars.len()
                                && chars[*pos + 1] == '\\'
                                && chars[*pos + 2] == 'u'
                            {
                                *pos += 2; // move onto the second 'u'
                                if let Some(lo) = Self::json_read_hex4(chars, pos) {
                                    let c = 0x10000
                                        + ((cp - 0xD800) << 10)
                                        + (lo - 0xDC00);
                                    if let Some(ch) = char::from_u32(c) {
                                        out.push(ch);
                                    }
                                }
                            } else if let Some(ch) = char::from_u32(cp) {
                                out.push(ch);
                            }
                        }
                    }
                    other => out.push(other),
                }
            } else {
                out.push(chars[*pos]);
            }
            *pos += 1;
        }
        *pos += 1; // closing quote
        out
    }

    /// Read exactly 4 hex digits following `\u` (pos is on the 'u'); returns the
    /// code unit and leaves pos on the last hex digit.
    fn json_read_hex4(chars: &[char], pos: &mut usize) -> Option<u32> {
        if *pos + 4 >= chars.len() {
            return None;
        }
        let mut val: u32 = 0;
        for k in 1..=4 {
            let d = chars[*pos + k].to_digit(16)?;
            val = val * 16 + d;
        }
        *pos += 4;
        Some(val)
    }

    fn json_number(chars: &[char], pos: &mut usize) -> Value {
        let start = *pos;
        let mut is_float = false;
        while *pos < chars.len() {
            let c = chars[*pos];
            if c.is_ascii_digit() || c == '-' || c == '+' {
                *pos += 1;
            } else if c == '.' || c == 'e' || c == 'E' {
                is_float = true;
                *pos += 1;
            } else {
                break;
            }
        }
        let num: String = chars[start..*pos].iter().collect();
        if is_float {
            Value::Float(num.parse().unwrap_or(0.0))
        } else {
            Value::Int(num.parse().unwrap_or(0))
        }
    }

    fn json_bool(chars: &[char], pos: &mut usize) -> Value {
        if chars[*pos] == 't' {
            if Self::json_match_literal(chars, pos, "true") {
                return Value::Bool(true);
            }
            *pos += 1;
            Value::Bool(true)
        } else {
            if Self::json_match_literal(chars, pos, "false") {
                return Value::Bool(false);
            }
            *pos += 1;
            Value::Bool(false)
        }
    }

    fn json_array(chars: &[char], pos: &mut usize) -> Value {
        let mut items = Vec::new();
        *pos += 1; // '['
        loop {
            Self::json_skip_ws(chars, pos);
            if *pos >= chars.len() || chars[*pos] == ']' {
                *pos += 1;
                break;
            }
            items.push(Self::json_value(chars, pos));
            Self::json_skip_ws(chars, pos);
            if *pos < chars.len() && chars[*pos] == ',' {
                *pos += 1;
            }
        }
        Value::Array(items)
    }

    fn json_object(chars: &[char], pos: &mut usize) -> Value {
        let mut map = HashMap::new();
        *pos += 1; // '{'
        loop {
            Self::json_skip_ws(chars, pos);
            if *pos >= chars.len() || chars[*pos] == '}' {
                *pos += 1;
                break;
            }
            // key
            let key = if chars[*pos] == '"' {
                Self::json_string(chars, pos)
            } else {
                break;
            };
            Self::json_skip_ws(chars, pos);
            if *pos < chars.len() && chars[*pos] == ':' {
                *pos += 1;
            }
            let val = Self::json_value(chars, pos);
            map.insert(key, val);
            Self::json_skip_ws(chars, pos);
            if *pos < chars.len() && chars[*pos] == ',' {
                *pos += 1;
            }
        }
        Value::Map(map)
    }
}
