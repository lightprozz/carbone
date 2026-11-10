use std::collections::HashMap;

use serde_json::{Value, json};

use crate::syntax::{Filter, Path, Selector, Step};
use crate::{Error, RenderOptions, Result, formatters};

#[derive(Clone)]
pub(crate) struct Binding {
    pub(crate) value: Value,
    pub(crate) index: usize,
    pub(crate) ordinal: usize,
}

pub(crate) type Bindings = HashMap<String, Binding>;

impl Path {
    pub(crate) fn resolve(
        &self,
        data: &Value,
        options: &RenderOptions,
        bindings: &Bindings,
        stop: usize,
    ) -> (Value, Vec<Value>) {
        let mut value = if self.root == 'd' {
            data.clone()
        } else {
            options.complement.clone()
        };
        let mut parents = Vec::new();
        for (position, step) in self.steps[..stop].iter().enumerate() {
            match step {
                Step::Key(key) => {
                    if value.is_object() {
                        parents.push(value.clone());
                    }
                    value = member(&value, key);
                }
                Step::Parent => {
                    value = parents.pop().unwrap_or(Value::Null);
                }
                Step::Index(index) => {
                    let entries = entries(&value);
                    let offset = if *index < 0 {
                        entries.len() as isize + index
                    } else {
                        *index
                    };
                    value = usize::try_from(offset)
                        .ok()
                        .and_then(|i| entries.get(i))
                        .map_or(Value::Null, |(_, value)| value.clone());
                }
                Step::Loop(_) => {
                    value = bindings
                        .get(&self.loop_key(position))
                        .map_or(Value::Null, |binding| binding.value.clone());
                }
                Step::Select(selector) if !selector.iterators.is_empty() => {
                    value = bindings
                        .get(&self.loop_key(position))
                        .map_or(Value::Null, |binding| binding.value.clone());
                }
                Step::Select(selector) => {
                    value = entries(&value)
                        .into_iter()
                        .find(|(index, item)| {
                            matches_filters(
                                &selector.filters,
                                item,
                                *index,
                                data,
                                options,
                                bindings,
                            )
                        })
                        .map_or(Value::Null, |(_, value)| value);
                }
            }
        }
        (value, parents)
    }
}

pub(crate) fn member(value: &Value, path: &str) -> Value {
    let path = path.replace('[', ".").replace(']', "");
    let mut current = value.clone();
    for key in path.split('.').filter(|key| !key.is_empty()) {
        current = if key == "length" {
            match &current {
                Value::Array(items) => json!(items.len()),
                Value::String(text) => json!(text.encode_utf16().count()),
                _ => Value::Null,
            }
        } else if let Value::Array(items) = &current {
            key.parse::<usize>()
                .ok()
                .and_then(|index| items.get(index))
                .cloned()
                .unwrap_or(Value::Null)
        } else {
            current.get(key).cloned().unwrap_or(Value::Null)
        };
    }
    current
}

fn entries(value: &Value) -> Vec<(usize, Value)> {
    match value {
        Value::Array(items) => items.iter().cloned().enumerate().collect(),
        Value::Object(items) => {
            // JavaScript enumerates integer property names before other keys.
            let mut items: Vec<_> = items.iter().collect();
            items.sort_by_key(|(key, _)| {
                key.parse::<u32>()
                    .ok()
                    .filter(|index| *index != u32::MAX && index.to_string() == **key)
            });
            // Option puts None first; the numeric keys belong first instead.
            items.sort_by_key(|(key, _)| {
                !key.parse::<u32>()
                    .ok()
                    .is_some_and(|index| index != u32::MAX && index.to_string() == **key)
            });
            items
                .into_iter()
                .enumerate()
                .map(|(index, (key, value))| (index, json!({"att":key, "val":value})))
                .collect()
        }
        _ => Vec::new(),
    }
}

pub(crate) fn candidates(
    path: &Path,
    stop: usize,
    data: &Value,
    options: &RenderOptions,
    bindings: &Bindings,
) -> Result<Vec<Binding>> {
    let (value, _) = path.resolve(data, options, bindings, stop);
    if !value.is_null() && !value.is_array() && !value.is_object() {
        return Err(Error::Template(format!(
            "loop {} requires an array or object",
            path.loop_key(stop)
        )));
    }
    let selector = match &path.steps[stop] {
        Step::Select(selector) => selector.clone(),
        _ => Selector {
            iterators: vec!["i".into()],
            sentinel: false,
            filters: Vec::new(),
        },
    };
    if value
        .as_array()
        .map_or_else(|| value.as_object().map_or(0, |v| v.len()), Vec::len)
        > 100_000
    {
        return Err(Error::Template("loop exceeds 100,000 items".into()));
    }
    let mut rows: Vec<_> = entries(&value)
        .into_iter()
        .filter(|(index, value)| {
            matches_filters(&selector.filters, value, *index, data, options, bindings)
        })
        .collect();
    let keys = |index: usize, value: &Value| -> Vec<Value> {
        selector
            .iterators
            .iter()
            .map(|name| {
                if name == "i" {
                    json!(index)
                } else {
                    member(value, name)
                }
            })
            .collect()
    };
    rows.sort_by(|(a_index, a), (b_index, b)| {
        let a = keys(*a_index, a);
        let b = keys(*b_index, b);
        for (a, b) in a.iter().zip(&b) {
            let order = compare(a, b);
            if !order.is_eq() {
                return order;
            }
        }
        std::cmp::Ordering::Equal
    });
    rows.dedup_by(|(a_index, a), (b_index, b)| keys(*a_index, a) == keys(*b_index, b));
    Ok(rows
        .into_iter()
        .enumerate()
        .map(|(ordinal, (index, value))| Binding {
            value,
            index,
            ordinal,
        })
        .collect())
}

fn numeric(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(*b),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) if s.trim().is_empty() => 0.0,
        _ => formatters::display(value).parse().unwrap_or(f64::NAN),
    }
}

fn compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    if let (Value::String(a), Value::String(b)) = (a, b) {
        return a.encode_utf16().cmp(b.encode_utf16());
    }
    numeric(a)
        .partial_cmp(&numeric(b))
        .unwrap_or(std::cmp::Ordering::Equal)
}

fn matches_filters(
    filters: &[Filter],
    value: &Value,
    index: usize,
    data: &Value,
    options: &RenderOptions,
    bindings: &Bindings,
) -> bool {
    filters.iter().all(|filter| {
        let left = if filter.field == "i" {
            json!(index)
        } else {
            member(value, &filter.field)
        };
        let raw = filter.value.as_str();
        let right = if (raw.starts_with('"') && raw.ends_with('"'))
            || (raw.starts_with('\'') && raw.ends_with('\''))
        {
            Value::String(raw[1..raw.len() - 1].into())
        } else if raw.starts_with("d.") || raw.starts_with("c.") {
            crate::syntax::parse_path(raw)
                .ok()
                .map_or(Value::Null, |path| {
                    path.resolve(data, options, bindings, path.steps.len()).0
                })
        } else {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.into()))
        };
        let equal = left == right
            || (!left.is_null() && !right.is_null() && numeric(&left) == numeric(&right));
        match filter.operator.as_str() {
            "=" => equal,
            "!=" => !equal,
            ">" => compare(&left, &right).is_gt(),
            "<" => compare(&left, &right).is_lt(),
            ">=" => compare(&left, &right).is_ge(),
            "<=" => compare(&left, &right).is_le(),
            _ => false,
        }
    })
}
