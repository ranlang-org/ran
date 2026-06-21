//! VM runtime values - compact, cache-friendly representation.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// VM value - designed for performance.
/// Small values (int, float, bool) are stored inline (no heap allocation).
/// Heap values (String, Array, Map, Struct) use reference counting.
#[derive(Debug, Clone)]
pub enum VMValue {
    Void,
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Arc<String>),
    Array(Arc<Vec<VMValue>>),
    Map(Arc<HashMap<String, VMValue>>),
    Struct(Arc<StructInstance>),
    Function(FnRef),
    Channel(ChannelRef),
}

/// Struct instance
#[derive(Debug, Clone)]
pub struct StructInstance {
    pub type_name: String,
    pub fields: HashMap<String, VMValue>,
}

/// Function reference (for closures and first-class functions)
#[derive(Debug, Clone)]
pub struct FnRef {
    pub name: String,
    pub chunk_idx: usize,
    pub arity: u8,
}

/// Channel reference
#[derive(Debug, Clone)]
pub struct ChannelRef {
    pub id: usize,
}

impl VMValue {
    pub fn is_truthy(&self) -> bool {
        match self {
            VMValue::Void => false,
            VMValue::Bool(b) => *b,
            VMValue::Int(n) => *n != 0,
            VMValue::Float(f) => *f != 0.0,
            VMValue::Str(s) => !s.is_empty(),
            VMValue::Array(a) => !a.is_empty(),
            _ => true,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            VMValue::Void => "void",
            VMValue::Int(_) => "int",
            VMValue::Float(_) => "float",
            VMValue::Bool(_) => "bool",
            VMValue::Str(_) => "string",
            VMValue::Array(_) => "array",
            VMValue::Map(_) => "map",
            VMValue::Struct(_) => "struct",
            VMValue::Function(_) => "function",
            VMValue::Channel(_) => "channel",
        }
    }

    pub fn to_json(&self) -> String {
        match self {
            VMValue::Void => "null".to_string(),
            VMValue::Int(n) => n.to_string(),
            VMValue::Float(f) => f.to_string(),
            VMValue::Bool(b) => b.to_string(),
            VMValue::Str(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
            VMValue::Array(arr) => {
                let items: Vec<String> = arr.iter().map(|v| v.to_json()).collect();
                format!("[{}]", items.join(","))
            }
            VMValue::Map(map) => {
                let items: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("\"{}\":{}", k, v.to_json()))
                    .collect();
                format!("{{{}}}", items.join(","))
            }
            VMValue::Struct(s) => {
                let items: Vec<String> = s
                    .fields
                    .iter()
                    .map(|(k, v)| format!("\"{}\":{}", k, v.to_json()))
                    .collect();
                format!("{{{}}}", items.join(","))
            }
            _ => "null".to_string(),
        }
    }
}

impl fmt::Display for VMValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VMValue::Void => write!(f, "()"),
            VMValue::Int(n) => write!(f, "{}", n),
            VMValue::Float(n) => write!(f, "{}", n),
            VMValue::Bool(b) => write!(f, "{}", b),
            VMValue::Str(s) => write!(f, "{}", s),
            VMValue::Array(arr) => {
                let items: Vec<String> = arr.iter().map(|v| format!("{}", v)).collect();
                write!(f, "[{}]", items.join(", "))
            }
            VMValue::Map(map) => {
                // Match the interpreter's `Value::Map` Display exactly: keys are
                // quoted, `"k": v`, joined by ", ". (Iteration order is
                // HashMap-defined in both engines.)
                let items: Vec<String> =
                    map.iter().map(|(k, v)| format!("\"{}\": {}", k, v)).collect();
                write!(f, "{{{}}}", items.join(", "))
            }
            VMValue::Struct(s) => {
                // Match the interpreter's `Value::Object` Display: `Type {k: v, ...}`.
                let items: Vec<String> = s
                    .fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, v))
                    .collect();
                write!(f, "{} {{{}}}", s.type_name, items.join(", "))
            }
            VMValue::Function(r) => write!(f, "<fn {}>", r.name),
            VMValue::Channel(c) => write!(f, "<chan #{}>", c.id),
        }
    }
}

impl PartialEq for VMValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (VMValue::Void, VMValue::Void) => true,
            (VMValue::Int(a), VMValue::Int(b)) => a == b,
            (VMValue::Float(a), VMValue::Float(b)) => a == b,
            (VMValue::Bool(a), VMValue::Bool(b)) => a == b,
            (VMValue::Str(a), VMValue::Str(b)) => a == b,
            _ => false,
        }
    }
}
