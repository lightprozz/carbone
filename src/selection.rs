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
    member_present(value, path).unwrap_or(Value::Null)
}

fn member_present(value: &Value, path: &str) -> Option<Value> {
    let path = path.replace('[', ".").replace(']', "");
    let mut current = value.clone();
    for key in path.split('.').filter(|key| !key.is_empty()) {
        current = match &current {
            Value::Array(items) if key == "length" => json!(items.len()),
            Value::String(text) if key == "length" => json!(text.encode_utf16().count()),
            Value::Array(items) => items.get(key.parse::<usize>().ok()?)?.clone(),
            _ => current.get(key)?.clone(),
        };
    }
    Some(current)
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
        _ => formatters::display(value)
            .trim()
            .parse()
            .unwrap_or(f64::NAN),
    }
}

fn compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    relational(a, b).unwrap_or(std::cmp::Ordering::Equal)
}

fn relational(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    if let (Value::String(a), Value::String(b)) = (a, b) {
        return Some(a.encode_utf16().cmp(b.encode_utf16()));
    }
    numeric(a).partial_cmp(&numeric(b))
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
            Some(json!(index))
        } else {
            member_present(value, &filter.field)
        };
        let present = left.is_some();
        let left = left.unwrap_or(Value::Null);
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
        let equal = present
            && (left == right
                || (!left.is_null()
                    && !right.is_null()
                    && !(left.is_string() && right.is_string())
                    && numeric(&left) == numeric(&right)));
        let order = present.then(|| relational(&left, &right)).flatten();
        match filter.operator.as_str() {
            "=" => equal,
            "!=" => !equal,
            ">" => order.is_some_and(|order| order.is_gt()),
            "<" => order.is_some_and(|order| order.is_lt()),
            ">=" => order.is_some_and(|order| order.is_ge()),
            "<=" => order.is_some_and(|order| order.is_le()),
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse_path;

    fn rows(text: &str, data: &Value) -> Vec<Binding> {
        let path = parse_path(text).unwrap();
        candidates(
            &path,
            path.steps.len() - 1,
            data,
            &RenderOptions::default(),
            &Bindings::new(),
        )
        .unwrap()
    }

    #[test]
    fn stable_sort_keeps_first_duplicate_and_original_index() {
        let data = json!({"items":[{"key":2,"id":"last"},{"key":1,"id":"first"},{"key":1,"id":"duplicate"}]});
        let selected = rows("d.items[key]", &data);
        assert_eq!(
            selected
                .iter()
                .map(|row| row.value["id"].clone())
                .collect::<Vec<_>>(),
            vec![json!("first"), json!("last")]
        );
        assert_eq!((selected[0].index, selected[0].ordinal), (1, 0));
        assert_eq!(rows("d.items[key,i]", &data).len(), 3);
        assert_eq!(rows("d.items[missing]", &data).len(), 1);
    }

    #[test]
    fn object_order_matches_javascript_entries() {
        let data: Value =
            serde_json::from_str(r#"{"items":{"z":1,"10":2,"2":3,"01":4,"4294967295":5,"a":6}}"#)
                .unwrap();
        let selected = rows("d.items[i]", &data);
        assert_eq!(
            selected
                .iter()
                .map(|row| row.value["att"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["2", "10", "z", "01", "4294967295", "a"]
        );
        assert_eq!(selected[0].value["val"], json!(3));
    }

    #[test]
    fn negative_indices_and_filters_use_source_length() {
        let data = json!({"items":[10,20,30,40]});
        let path = parse_path("d.items[i=-1]").unwrap();
        assert_eq!(
            path.resolve(
                &data,
                &RenderOptions::default(),
                &Bindings::new(),
                path.steps.len()
            )
            .0,
            json!(40)
        );
        let selected = rows("d.items[i,i>-3]", &data);
        assert_eq!(
            selected
                .iter()
                .map(|row| row.value.clone())
                .collect::<Vec<_>>(),
            vec![json!(30), json!(40)]
        );
        assert_eq!((selected[0].index, selected[0].ordinal), (2, 0));
    }

    #[test]
    fn filters_match_node_literal_coercion_and_missing_values() {
        let data = json!({"items":[{"n":"10"},{"n":"2"},{"n":1},{"n":null},{}]});
        assert_eq!(rows("d.items[n<2,i]", &data).len(), 3);
        assert!(rows("d.items[n>=abc,i]", &data).is_empty());
        assert!(rows("d.items[n=null,i]", &data).is_empty());
        let data = json!({"items":[{"n":true},{"n":"true"},{"n":1}]});
        assert_eq!(rows("d.items[n=true,i]", &data).len(), 2);
        let data = json!({"items":[{"n":"01"},{"n":1},{"n":true}]});
        assert_eq!(rows("d.items[n=1,i]", &data).len(), 2);
    }

    fn aligned(path: &Path, data: &Value, outer: usize) -> Vec<Binding> {
        let mut bindings = Bindings::new();
        let parent = candidates(path, 1, data, &RenderOptions::default(), &bindings).unwrap();
        bindings.insert(path.loop_key(1), parent[outer].clone());
        candidates(
            path,
            path.steps.len() - 1,
            data,
            &RenderOptions::default(),
            &bindings,
        )
        .unwrap()
    }

    #[test]
    fn aligned_sort_keys_fill_missing_rows_across_parent_siblings() {
        let data = json!({"cars":[{"wheels":[{"size":"D"},{"size":"E"}]},{"wheels":[{"size":"C"},{"size":"D"},{"size":"E"}]}]});
        let path = parse_path("d.cars[i].wheels[size++]").unwrap();
        let first = aligned(&path, &data, 0);
        assert_eq!(
            first
                .iter()
                .map(|row| row.value.clone())
                .collect::<Vec<_>>(),
            vec![Value::Null, json!({"size":"D"}), json!({"size":"E"})]
        );
        assert_eq!((first[1].index, first[1].ordinal), (0, 1));
        let second = aligned(&path, &data, 1);
        assert_eq!(
            second
                .iter()
                .map(|row| row.value["size"].clone())
                .collect::<Vec<_>>(),
            vec![json!("C"), json!("D"), json!("E")]
        );
    }

    #[test]
    fn aligned_union_walks_all_preceding_loops_and_honors_filters() {
        let data = json!({"regions":[{"cars":[{"wheels":[{"size":"A"}]}]},{"cars":[{"wheels":[{"size":"B"}]}]}]});
        let path = parse_path("d.regions[i].cars[i].wheels[size++]").unwrap();
        let mut bindings = Bindings::new();
        for stop in [1, 3] {
            let row = candidates(&path, stop, &data, &RenderOptions::default(), &bindings)
                .unwrap()
                .remove(0);
            bindings.insert(path.loop_key(stop), row);
        }
        let selected = candidates(&path, 5, &data, &RenderOptions::default(), &bindings).unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[1].value, Value::Null);
        let data = json!({"cars":[{"keep":true,"wheels":[{"size":"A"}]},{"keep":false,"wheels":[{"size":"B"}]}]});
        let path = parse_path("d.cars[i,keep=true].wheels[size++]").unwrap();
        assert_eq!(aligned(&path, &data, 0).len(), 1);
    }

    #[test]
    fn aligned_union_has_a_cumulative_visit_limit() {
        let data = json!({"cars":[{"wheels":vec![json!({"size":"A"});50_000]},{"wheels":vec![json!({"size":"B"});50_000]}]});
        let path = parse_path("d.cars[i].wheels[size++]").unwrap();
        let mut bindings = Bindings::new();
        let parent = candidates(&path, 1, &data, &RenderOptions::default(), &bindings)
            .unwrap()
            .remove(0);
        bindings.insert(path.loop_key(1), parent);
        assert!(candidates(&path, 3, &data, &RenderOptions::default(), &bindings).is_err());
    }
}
