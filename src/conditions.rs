//! Inline conditional formatters (`ifEQ`, `show`, `and`, ...), ported from
//! `formatters/condition.js`. Block markers (`showBegin`/`hideBegin`) and
//! dynamic arguments (`and(.other)`) are not supported.

use serde_json::Value;

use crate::formatters::display;

/// Per-marker state, equivalent to the JS formatter context.
#[derive(Default)]
pub(crate) struct State {
    condition: Option<bool>,
    and_operator: bool,
    pub(crate) stop: bool,
}

const NAMES: &[&str] = &[
    "ifEQ",
    "ifNE",
    "ifGT",
    "ifGTE",
    "ifLT",
    "ifLTE",
    "ifIN",
    "ifNIN",
    "ifEM",
    "ifNEM",
    "show",
    "elseShow",
    "and",
    "or",
    "ifEmpty",
    "ifEqual",
    "ifContain",
];

pub(crate) fn supports(name: &str) -> bool {
    NAMES.contains(&name)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

fn text(value: &Value) -> String {
    if value.is_null() {
        "null".into()
    } else {
        display(value)
    }
}

fn number(value: &str) -> f64 {
    crate::formatters::parse_float(value)
}

fn contains(data: &Value, wanted: &str) -> Option<bool> {
    match data {
        Value::String(s) => Some(s.contains(wanted)),
        // JS indexOf is strict: the string argument never matches a number.
        Value::Array(items) => Some(items.iter().any(|i| i.as_str() == Some(wanted))),
        _ => None,
    }
}

fn message(args: &[String]) -> Value {
    args.first()
        .map_or(Value::Null, |m| Value::String(m.clone()))
}

fn continues(args: &[String], index: usize) -> bool {
    args.get(index).is_some_and(|a| a == "true")
}

impl State {
    fn update(&mut self, result: bool) {
        self.condition = match (self.and_operator, self.condition) {
            (true, current) => current.map(|c| c && result),
            (false, Some(c)) => Some(c || result),
            (false, None) => Some(result),
        };
    }

    pub(crate) fn apply(&mut self, name: &str, data: Value, args: &[String]) -> Value {
        let arg = args.first().map(String::as_str);
        let compare = |op: fn(f64, f64) -> bool| {
            let (a, b) = (number(&text(&data)), number(arg.unwrap_or("undefined")));
            !a.is_nan() && !b.is_nan() && op(a, b)
        };
        match name {
            "ifEQ" => self.update(text(&data) == arg.unwrap_or("undefined")),
            "ifNE" => self.update(text(&data) != arg.unwrap_or("undefined")),
            "ifGT" => self.update(compare(|a, b| a > b)),
            "ifGTE" => self.update(compare(|a, b| a >= b)),
            "ifLT" => self.update(compare(|a, b| a < b)),
            "ifLTE" => self.update(compare(|a, b| a <= b)),
            "ifIN" | "ifNIN" => {
                let found = arg
                    .filter(|a| !a.is_empty())
                    .and_then(|a| contains(&data, a));
                self.update(found.is_some_and(|f| f == (name == "ifIN")));
            }
            "ifEM" => self.update(is_empty(&data)),
            "ifNEM" => self.update(!is_empty(&data)),
            "show" | "elseShow" => {
                let wanted = name == "show";
                if self.condition == Some(wanted)
                    || (self.condition.is_none() && truthy(&data) == wanted)
                {
                    self.stop = true;
                    return message(args);
                }
            }
            "and" | "or" => {
                self.and_operator = name == "and";
                return args.first().map_or(data, |v| Value::String(v.clone()));
            }
            // Legacy single-formatter conditions.
            "ifEmpty" | "ifEqual" | "ifContain" => {
                let hit = match name {
                    "ifEmpty" => is_empty(&data),
                    "ifEqual" => text(&data) == arg.unwrap_or("undefined"),
                    _ => contains(&data, arg.unwrap_or("undefined")) == Some(true),
                };
                if hit {
                    let index = if name == "ifEmpty" { 1 } else { 2 };
                    let output = if name == "ifEmpty" {
                        message(args)
                    } else {
                        args.get(1).map_or(Value::Null, |m| m.clone().into())
                    };
                    self.stop = !continues(args, index);
                    return output;
                }
            }
            _ => unreachable!("checked by supports()"),
        }
        data
    }
}
