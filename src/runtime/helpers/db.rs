//! SQLite (`db`) value mapping + handleable error constructors.
//! Part of the `Environment` inherent impl (microkernel-style split).

use super::super::*;
use std::collections::HashMap;

impl Environment {
    // --- DB (SQLite) value mapping + handleable errors ----------------------

    /// Render a [`DbError`] as a handleable Ran error value while also emitting
    /// the registered diagnostic to stderr (mirrors the concurrency helpers).
    /// The runtime keeps running; the program can inspect the returned
    /// `Map{error, code, message}` (R6.3/R6.4/R6.6/R7.5/R9.2/R9.5, etc.).
    pub(crate) fn db_error_value(err: &crate::support::sqlite_ffi::DbError) -> Value {
        let code = err.code;
        let msg = err.message.clone();
        let hint = crate::support::diagnostics::code_severity_hint(code)
            .map(|(_, h)| h)
            .unwrap_or("");
        eprintln!("\x1b[31;1merror\x1b[0m[{}]: {}", code, msg);
        if !hint.is_empty() {
            eprintln!("  \x1b[36m= help\x1b[0m: {}", hint);
        }
        let mut m = HashMap::new();
        m.insert("error".to_string(), Value::Bool(true));
        m.insert("code".to_string(), Value::Str(code.to_string()));
        m.insert("message".to_string(), Value::Str(msg));
        Value::Map(m)
    }

    /// Map a single Ran [`Value`] to a SQLite bind value ([`DbValue`]):
    /// Int→Int, Float→Float, Str→Str, Decimal→Decimal, Void→Null. Any other
    /// kind (Bool/Array/Map/Object) is unsupported as a bind parameter and
    /// yields `None` so the caller can raise a clear error.
    pub(crate) fn ran_value_to_db(v: &Value) -> Option<crate::support::sqlite_ffi::DbValue> {
        use crate::support::sqlite_ffi::DbValue;
        Some(match v {
            Value::Int(n) => DbValue::Int(*n),
            Value::Float(f) => DbValue::Float(*f),
            Value::Str(s) => DbValue::Str(s.clone()),
            Value::Decimal(d) => DbValue::Decimal(*d),
            Value::Void => DbValue::Null,
            _ => return None,
        })
    }

    /// Map a SQLite read value ([`DbValue`]) back to a Ran [`Value`]:
    /// Int→Int, Float→Float, Str→Str, Null→Void, Decimal→Decimal.
    pub(crate) fn db_value_to_ran(v: crate::support::sqlite_ffi::DbValue) -> Value {
        use crate::support::sqlite_ffi::DbValue;
        match v {
            DbValue::Int(n) => Value::Int(n),
            DbValue::Float(f) => Value::Float(f),
            DbValue::Str(s) => Value::Str(s),
            DbValue::Null => Value::Void,
            DbValue::Decimal(d) => Value::Decimal(d),
        }
    }

    /// Convert query result rows into a Ran `Array` of `Map` (column→value).
    pub(crate) fn db_rows_to_value(rows: Vec<crate::support::sqlite_ffi::DbRow>) -> Value {
        let arr: Vec<Value> = rows
            .into_iter()
            .map(|row| {
                let mut m = HashMap::new();
                for (name, val) in row {
                    m.insert(name, Self::db_value_to_ran(val));
                }
                Value::Map(m)
            })
            .collect();
        Value::Array(arr)
    }

    /// Evaluate the params argument (an array) at `idx` and convert each element
    /// to a [`DbValue`]. A missing/`Void` argument is treated as no parameters.
    /// On a non-array argument or an unsupported element type, returns a
    /// handleable error value (`Err`) the caller should return as-is.
    pub(crate) fn eval_db_params(
        &mut self,
        args: &[Expression],
        idx: usize,
    ) -> Result<Vec<crate::support::sqlite_ffi::DbValue>, Value> {
        let v = self.eval_arg_val(args, idx);
        let items = match v {
            Value::Array(a) => a,
            Value::Void => Vec::new(),
            other => return Err(Self::db_param_type_error(&other)),
        };
        let mut out = Vec::with_capacity(items.len());
        for it in &items {
            match Self::ran_value_to_db(it) {
                Some(dv) => out.push(dv),
                None => return Err(Self::db_param_type_error(it)),
            }
        }
        Ok(out)
    }

    /// Build a handleable error value for an unsupported bind-parameter type.
    /// Surfaced under `E0507` (unsupported type) with a parameter-specific
    /// message, and emitted as a diagnostic.
    pub(crate) fn db_param_type_error(v: &Value) -> Value {
        let kind = match v {
            Value::Bool(_) => "bool",
            Value::Array(_) => "array",
            Value::Map(_) => "map",
            Value::Object(name, _) => return Self::db_param_type_error_named(name),
            _ => "nilai",
        };
        Self::db_param_type_error_named(kind)
    }

    /// Helper for [`db_param_type_error`] given a type label.
    pub(crate) fn db_param_type_error_named(kind: &str) -> Value {
        let code = "E0507";
        let msg = format!(
            "tipe parameter `{}` tidak didukung sebagai bind value; gunakan int/float/str/decimal/void",
            kind
        );
        let hint = crate::support::diagnostics::code_severity_hint(code)
            .map(|(_, h)| h)
            .unwrap_or("Gunakan int/float/str/decimal/void sebagai parameter.");
        eprintln!("\x1b[31;1merror\x1b[0m[{}]: {}", code, msg);
        eprintln!("  \x1b[36m= help\x1b[0m: {}", hint);
        let mut m = HashMap::new();
        m.insert("error".to_string(), Value::Bool(true));
        m.insert("code".to_string(), Value::Str(code.to_string()));
        m.insert("message".to_string(), Value::Str(msg));
        Value::Map(m)
    }
}
