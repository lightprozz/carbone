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
                    let entries = entries(&value);
                    value = entries
                        .iter()
                        .find(|(index, item)| {
                            matches_filters(
                                &selector.filters,
                                item,
                                *index,
                                entries.len(),
                                data,
                                options,
                                bindings,
                            )
                        })
                        .map_or(Value::Null, |(_, value)| value.clone());
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
            items.sort_by(
                |(a, _), (b, _)| match (property_index(a), property_index(b)) {
                    (Some(a), Some(b)) => a.cmp(&b),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
            );
            items
                .into_iter()
                .enumerate()
                .map(|(index, (key, value))| (index, json!({"att":key, "val":value})))
                .collect()
        }
        _ => Vec::new(),
    }
}

fn property_index(key: &str) -> Option<u32> {
    key.parse::<u32>()
        .ok()
        .filter(|index| *index != u32::MAX && index.to_string() == key)
}

const MAX_CANDIDATES: usize = 100_000;

type Row = (usize, Value);

pub(crate) fn candidates(
    path: &Path,
    stop: usize,
    data: &Value,
    options: &RenderOptions,
    bindings: &Bindings,
) -> Result<Vec<Binding>> {
    let selector = selector(&path.steps[stop]);
    let (value, _) = path.resolve(data, options, bindings, stop);
    let mut rows = select_rows(&value, &selector, data, options, bindings)?;
    if selector.aligned {
        let mut union = Vec::new();
        let mut budget = MAX_CANDIDATES;
        collect_aligned_rows(
            path,
            stop,
            0,
            data,
            options,
            bindings,
            &mut budget,
            &mut union,
        )?;
        sort_rows(&mut union, &selector);
        let mut local: HashMap<String, Row> = rows
            .into_iter()
            .map(|row| (key_id(&row, &selector), row))
            .collect();
        rows = union
            .into_iter()
            .map(|row| {
                local
                    .remove(&key_id(&row, &selector))
                    .unwrap_or((row.0, Value::Null))
            })
            .collect();
    }
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

fn selector(step: &Step) -> Selector {
    match step {
        Step::Select(selector) => selector.clone(),
        _ => Selector {
            iterators: vec!["i".into()],
            sentinel: false,
            aligned: false,
            filters: Vec::new(),
        },
    }
}

fn select_rows(
    value: &Value,
    selector: &Selector,
    data: &Value,
    options: &RenderOptions,
    bindings: &Bindings,
) -> Result<Vec<Row>> {
    let length = match value {
        Value::Array(items) => items.len(),
        Value::Object(items) => items.len(),
        Value::Null => 0,
        _ => return Err(Error::Template("loop requires an array or object".into())),
    };
    if length > MAX_CANDIDATES {
        return Err(Error::Template("loop exceeds 100,000 items".into()));
    }
    let mut rows: Vec<_> = entries(value)
        .into_iter()
        .filter(|(index, value)| {
            matches_filters(
                &selector.filters,
                value,
                *index,
                length,
                data,
                options,
                bindings,
            )
        })
        .collect();
    sort_rows(&mut rows, selector);
    Ok(rows)
}

fn keys(row: &Row, selector: &Selector) -> Vec<Value> {
    selector
        .iterators
        .iter()
        .map(|name| {
            if name == "i" {
                json!(row.0)
            } else {
                member(&row.1, name)
            }
        })
        .collect()
}

fn key_id(row: &Row, selector: &Selector) -> String {
    serde_json::to_string(&keys(row, selector)).expect("JSON values serialize")
}

fn sort_rows(rows: &mut Vec<Row>, selector: &Selector) {
    rows.sort_by(|a, b| {
        for (a, b) in keys(a, selector).iter().zip(keys(b, selector)) {
            let order = compare(a, &b);
            if !order.is_eq() {
                return order;
            }
        }
        std::cmp::Ordering::Equal
    });
    // Stable sorting plus dedup_by retains the first source item for equal keys.
    rows.dedup_by(|a, b| keys(a, selector) == keys(b, selector));
}

#[allow(clippy::too_many_arguments)]
fn collect_aligned_rows(
    path: &Path,
    stop: usize,
    start: usize,
    data: &Value,
    options: &RenderOptions,
    bindings: &Bindings,
    budget: &mut usize,
    output: &mut Vec<Row>,
) -> Result<()> {
    let position = (start..stop).find(|position| path.steps[*position].iteration().is_some());
    let position = position.unwrap_or(stop);
    let (source, _) = path.resolve(data, options, bindings, position);
    let length = source.as_array().map_or_else(
        || source.as_object().map_or(0, |items| items.len()),
        Vec::len,
    );
    *budget = budget
        .checked_sub(length)
        .ok_or_else(|| Error::Template("aligned loop exceeds 100,000 items".into()))?;
    let rows = select_rows(
        &source,
        &selector(&path.steps[position]),
        data,
        options,
        bindings,
    )?;
    if position == stop {
        output.extend(rows);
    } else {
        let key = path.loop_key(position);
        let mut siblings = bindings.clone();
        for (ordinal, (index, value)) in rows.into_iter().enumerate() {
            siblings.insert(
                key.clone(),
                Binding {
                    value,
                    index,
                    ordinal,
                },
            );
            collect_aligned_rows(
                path,
                stop,
                position + 1,
                data,
                options,
                &siblings,
                budget,
                output,
            )?;
        }
    }
    Ok(())
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
    length: usize,
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
        let right = if filter.field == "i" {
            let index = raw.parse::<isize>().ok();
            index.map_or(Value::Null, |index| {
                json!(if index < 0 {
                    length as isize + index
                } else {
                    index
                })
            })
        } else if (raw.starts_with('"') && raw.ends_with('"'))
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
            Value::String(raw.into())
        };
        let left = if raw == "true" || raw == "false" {
            Value::String(formatters::display(&left))
        } else {
            left
        };
        let equal = left == right
            || (!left.is_null()
                && !right.is_null()
                && !(left.is_string() && right.is_string())
                && numeric(&left) == numeric(&right));
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
