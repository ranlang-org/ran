//! Built-in functions and value-method dispatch (extracted from mod.rs).
//! Part of the `Environment` inherent impl.

use super::*;

impl Environment {
    // ========================================================================
    // Built-in functions
    // ========================================================================

    pub(super) fn call_function(&mut self, name: &str, args: &[Expression]) -> Value {
        match name {
            "print" | "println" => {
                let vals: Vec<String> = args.iter().map(|a| {
                    let v = self.eval_expression(a);
                    let s = format!("{}", v);
                    self.interpolate_string(&s)
                }).collect();
                println!("{}", vals.join(" "));
                Value::Void
            }
            "len" => {
                if let Some(arg) = args.first() {
                    match self.eval_expression(arg) {
                        Value::Str(s) => Value::Int(s.len() as i64),
                        Value::Array(a) => Value::Int(a.len() as i64),
                        Value::Map(m) => Value::Int(m.len() as i64),
                        _ => Value::Int(0),
                    }
                } else { Value::Int(0) }
            }
            "typeof" => {
                if let Some(arg) = args.first() {
                    let val = self.eval_expression(arg);
                    Value::Str(match val {
                        Value::Int(_) => "int",
                        Value::Float(_) => "float",
                        Value::Decimal(_) => "decimal",
                        Value::Str(_) => "string",
                        Value::Bool(_) => "bool",
                        Value::Array(_) => "array",
                        Value::Map(_) => "map",
                        Value::Object(name, _) => return Value::Str(name.clone()),
                        Value::Closure { .. } => "closure",
                        Value::Void => "void",
                    }.to_string())
                } else { Value::Str("unknown".to_string()) }
            }
            "str" => {
                if let Some(arg) = args.first() {
                    let val = self.eval_expression(arg);
                    Value::Str(format!("{}", val))
                } else { Value::Str(String::new()) }
            }
            "int" => {
                if let Some(arg) = args.first() {
                    match self.eval_expression(arg) {
                        Value::Int(n) => Value::Int(n),
                        Value::Float(f) => Value::Int(f as i64),
                        Value::Decimal(d) => Value::Int(d.to_i64_trunc()),
                        Value::Str(s) => Value::Int(s.parse().unwrap_or(0)),
                        Value::Bool(b) => Value::Int(if b { 1 } else { 0 }),
                        _ => Value::Int(0),
                    }
                } else { Value::Int(0) }
            }
            "float" => {
                if let Some(arg) = args.first() {
                    match self.eval_expression(arg) {
                        Value::Int(n) => Value::Float(n as f64),
                        Value::Float(f) => Value::Float(f),
                        Value::Decimal(d) => Value::Float(d.to_f64()),
                        Value::Str(s) => Value::Float(s.parse().unwrap_or(0.0)),
                        _ => Value::Float(0.0),
                    }
                } else { Value::Float(0.0) }
            }
            "dec" | "decimal" => {
                // dec("19.99") — concise constructor for exact decimals.
                if let Some(arg) = args.first() {
                    let v = self.eval_expression(arg);
                    self.make_decimal(&v)
                } else { Value::Decimal(Decimal::zero()) }
            }
            "push" => {
                // push(array_name, value) - mutate array
                if args.len() >= 2 {
                    let val = self.eval_expression(&args[1]);
                    if let Expression::Variable(arr_name) = &args[0] {
                        if let Some(Value::Array(ref mut arr)) = self.var_get_mut(arr_name) {
                            arr.push(val);
                        }
                    }
                }
                Value::Void
            }
            "map" => {
                // map() - create empty map
                Value::Map(HashMap::new())
            }
            "set" => {
                // set(map_name, key, value)
                if args.len() >= 3 {
                    let key = self.eval_arg_str(args, 1, "");
                    let val = self.eval_expression(&args[2]);
                    if let Expression::Variable(map_name) = &args[0] {
                        if let Some(Value::Map(ref mut m)) = self.var_get_mut(map_name) {
                            m.insert(key, val);
                        }
                    }
                }
                Value::Void
            }
            "get" => {
                // get(map_name, key)
                if args.len() >= 2 {
                    let key = self.eval_arg_str(args, 1, "");
                    if let Expression::Variable(map_name) = &args[0] {
                        if let Some(Value::Map(m)) = self.var_get(map_name) {
                            return m.get(&key).cloned().unwrap_or(Value::Void);
                        }
                    }
                }
                Value::Void
            }
            "exit" => {
                let code = self.eval_arg_int(args, 0, 0) as i32;
                std::process::exit(code);
            }
            "range" => {
                // range(n) -> [0..n]; range(a, b) -> [a..b]
                let (start, end) = if args.len() >= 2 {
                    (self.eval_arg_int(args, 0, 0), self.eval_arg_int(args, 1, 0))
                } else {
                    (0, self.eval_arg_int(args, 0, 0))
                };
                let mut items = Vec::new();
                let mut i = start;
                while i < end {
                    items.push(Value::Int(i));
                    i += 1;
                }
                Value::Array(items)
            }
            "keys" => {
                // keys(map) -> array of keys
                if let Some(Value::Map(m)) = args.first().map(|a| self.eval_expression(a)) {
                    Value::Array(m.keys().map(|k| Value::Str(k.clone())).collect())
                } else {
                    Value::Array(vec![])
                }
            }
            "values" => {
                if let Some(Value::Map(m)) = args.first().map(|a| self.eval_expression(a)) {
                    Value::Array(m.values().cloned().collect())
                } else {
                    Value::Array(vec![])
                }
            }
            "abs" => {
                match self.eval_arg_val(args, 0) {
                    Value::Float(f) => Value::Float(f.abs()),
                    v => Value::Int(v.as_i64().abs()),
                }
            }
            "assert" => {
                let cond = self.eval_arg_val(args, 0);
                if !cond.is_truthy_val() {
                    let msg = self.eval_arg_str(args, 1, "assertion failed");
                    // In `ran test` mode, record the failure instead of exiting
                    // so the harness can run the remaining tests.
                    if test_mode_active() {
                        record_test_failure(&msg);
                    } else {
                        // Library code must not abruptly exit (R3.1, R4.3): raise a
                        // recoverable RuntimeFault that unwinds to the nearest catch
                        // boundary so an assertion failure can be caught instead of
                        // killing the process.
                        runtime_error(
                            "E1013",
                            &format!("assertion failed: {}", msg),
                            "periksa kondisi `assert`; tangani dengan `try`/recover bila kegagalan dapat dipulihkan",
                        );
                    }
                }
                Value::Void
            }
            _ => {
                // Closure bound to a variable (or passed as an argument and now
                // invoked): `f(args)` where `f` holds a `Value::Closure`. Checked
                // before named functions so first-class function values work
                // wherever a call expression appears (R8.1, R8.2).
                if let Some(Value::Closure { params, body, captured }) = self.var_get(name) {
                    let arg_values: Vec<Value> =
                        args.iter().map(|a| self.eval_expression(a)).collect();
                    return self.call_closure(&params, &body, &captured, arg_values);
                }
                // User-defined function
                if let Some(body) = self.functions.get(name).cloned() {
                    let arg_values: Vec<Value> = args.iter().map(|a| self.eval_expression(a)).collect();
                    let param_names = self.fn_params.get(name).cloned().unwrap_or_default();
                    let mut_flags = self.fn_mut.get(name).cloned().unwrap_or_default();
                    let params: Vec<(String, Value)> = param_names
                        .iter()
                        .enumerate()
                        .filter_map(|(i, p)| arg_values.get(i).map(|v| (p.clone(), v.clone())))
                        .collect();
                    // `&mut` write-back: parameters declared `&mut` whose argument
                    // is a caller lvalue have their final value written back after
                    // the call returns, so the callee's mutation is observable by
                    // the caller (R11.6). Non-`&mut` params remain pass-by-value.
                    let writebacks = self.collect_writebacks(name, &param_names, &mut_flags, args);
                    if writebacks.is_empty() {
                        return self.run_function_frame(&body[..], params);
                    }
                    let capture: Vec<String> = writebacks.iter().map(|(n, _)| n.clone()).collect();
                    let (ret, finals) = self.run_function_frame_capture(&body[..], params, &capture);
                    self.apply_writebacks(&writebacks, &finals);
                    return ret;
                }
                Value::Void
            }
        }
    }

    // ========================================================================
    // Method calls on values
    // ========================================================================

    pub(super) fn call_method(&mut self, obj: &Value, method: &str, args: &[Expression]) -> Value {
        match (obj, method) {
            // Decimal methods (exact money math)
            (Value::Decimal(d), "add") => Self::decimal_binop(d, &BinaryOperator::Add, &self.decimal_arg(args, 0)),
            (Value::Decimal(d), "sub") => Self::decimal_binop(d, &BinaryOperator::Sub, &self.decimal_arg(args, 0)),
            (Value::Decimal(d), "mul") => Self::decimal_binop(d, &BinaryOperator::Mul, &self.decimal_arg(args, 0)),
            (Value::Decimal(d), "div") => {
                let b = self.decimal_arg(args, 0);
                let scale = self.eval_arg_int(args, 1, 2).max(0) as u32;
                let mode = Rounding::from_name(&self.eval_arg_str(args, 2, "half_up"));
                if b.is_zero() {
                    runtime_error("E1002", "decimal division by zero", "guard the divisor");
                }
                match d.div(&b, scale, mode) {
                    Ok(r) => Value::Decimal(r),
                    Err(e) => runtime_error("E1003", &e, "reduce scale or operand size"),
                }
            }
            (Value::Decimal(d), "round") => {
                let scale = self.eval_arg_int(args, 0, 2).max(0) as u32;
                let mode = Rounding::from_name(&self.eval_arg_str(args, 1, "half_up"));
                match d.rescale(scale, mode) {
                    Ok(r) => Value::Decimal(r),
                    Err(e) => runtime_error("E1003", &e, "reduce the scale"),
                }
            }
            (Value::Decimal(d), "abs") => Value::Decimal(d.abs()),
            (Value::Decimal(d), "neg") => Value::Decimal(d.neg()),
            (Value::Decimal(d), "is_zero") => Value::Bool(d.is_zero()),
            (Value::Decimal(d), "scale") => Value::Int(d.scale() as i64),
            (Value::Decimal(d), "to_str") => Value::Str(format!("{}", d)),
            (Value::Decimal(d), "to_int") => Value::Int(d.to_i64_trunc()),
            (Value::Decimal(d), "to_float") => Value::Float(d.to_f64()),
            (Value::Decimal(d), "cmp") => {
                let b = self.decimal_arg(args, 0);
                Value::Int(match d.cmp(&b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                })
            }

            // String methods
            (Value::Str(s), "len") => Value::Int(s.len() as i64),
            (Value::Str(s), "to_upper") => Value::Str(s.to_uppercase()),
            (Value::Str(s), "to_lower") => Value::Str(s.to_lowercase()),
            (Value::Str(s), "trim") => Value::Str(s.trim().to_string()),
            (Value::Str(s), "contains") => {
                let needle = self.eval_arg_str(args, 0, "");
                Value::Bool(s.contains(&needle))
            }
            (Value::Str(s), "starts_with") => {
                let prefix = self.eval_arg_str(args, 0, "");
                Value::Bool(s.starts_with(&prefix))
            }
            (Value::Str(s), "ends_with") => {
                let suffix = self.eval_arg_str(args, 0, "");
                Value::Bool(s.ends_with(&suffix))
            }
            (Value::Str(s), "replace") => {
                let from = self.eval_arg_str(args, 0, "");
                let to = self.eval_arg_str(args, 1, "");
                Value::Str(s.replace(&from, &to))
            }
            (Value::Str(s), "split") => {
                let delim = self.eval_arg_str(args, 0, " ");
                let parts: Vec<Value> = s.split(&delim).map(|p| Value::Str(p.to_string())).collect();
                Value::Array(parts)
            }
            (Value::Str(s), "chars") => {
                let chars: Vec<Value> = s.chars().map(|c| Value::Str(c.to_string())).collect();
                Value::Array(chars)
            }
            (Value::Str(s), "repeat") => {
                let n = self.eval_arg_int(args, 0, 1) as usize;
                Value::Str(s.repeat(n))
            }
            (Value::Str(s), "slice") => {
                let start = self.eval_arg_int(args, 0, 0) as usize;
                let end = self.eval_arg_int(args, 1, s.len() as i64) as usize;
                let end = end.min(s.len());
                Value::Str(s[start..end].to_string())
            }

            // Array methods
            (Value::Array(a), "len") => Value::Int(a.len() as i64),
            (Value::Array(a), "first") => a.first().cloned().unwrap_or(Value::Void),
            (Value::Array(a), "last") => a.last().cloned().unwrap_or(Value::Void),
            (Value::Array(a), "contains") => {
                let needle = if let Some(arg) = args.first() {
                    self.eval_expression(arg)
                } else { Value::Void };
                let found = a.iter().any(|v| format!("{}", v) == format!("{}", needle));
                Value::Bool(found)
            }
            (Value::Array(a), "join") => {
                let sep = self.eval_arg_str(args, 0, ",");
                let joined: Vec<String> = a.iter().map(|v| format!("{}", v)).collect();
                Value::Str(joined.join(&sep))
            }
            (Value::Array(a), "reverse") => {
                let mut rev = a.clone();
                rev.reverse();
                Value::Array(rev)
            }
            (Value::Array(a), "slice") => {
                let start = self.eval_arg_int(args, 0, 0) as usize;
                let end = self.eval_arg_int(args, 1, a.len() as i64) as usize;
                Value::Array(a[start..end.min(a.len())].to_vec())
            }

            // Map methods
            (Value::Map(m), "keys") => {
                let keys: Vec<Value> = m.keys().map(|k| Value::Str(k.clone())).collect();
                Value::Array(keys)
            }
            (Value::Map(m), "values") => {
                let vals: Vec<Value> = m.values().cloned().collect();
                Value::Array(vals)
            }
            (Value::Map(m), "has") => {
                let key = self.eval_arg_str(args, 0, "");
                Value::Bool(m.contains_key(&key))
            }
            (Value::Map(m), "len") => Value::Int(m.len() as i64),

            _ => Value::Void,
        }
    }
}
