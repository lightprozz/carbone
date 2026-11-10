use std::collections::HashMap;

use serde_json::Value;

use crate::compile::Node;
use crate::registry::{FormatterContext, Registry};
use crate::syntax::{Expression, Path, Step};
use crate::{Error, RenderOptions, Result, formatters};

const MAX_OUTPUT: usize = 64 * 1024 * 1024;

impl Path {
    fn value<'a>(
        &self,
        data: &'a Value,
        options: &'a RenderOptions,
        bindings: &HashMap<String, usize>,
        stop: usize,
    ) -> &'a Value {
        let mut value = if self.root == 'd' {
            data
        } else {
            &options.complement
        };
        for (position, step) in self.steps[..stop].iter().enumerate() {
            value = match step {
                Step::Key(key) => value.get(key),
                Step::Index(index) => value.get(*index),
                Step::Loop(_) => bindings
                    .get(&self.loop_key(position))
                    .and_then(|index| value.get(*index)),
            }
            .unwrap_or(&Value::Null);
        }
        value
    }
}

fn push(output: &mut String, text: &str) -> Result<()> {
    if output.len().saturating_add(text.len()) > MAX_OUTPUT {
        return Err(Error::Template("rendered output exceeds 64 MiB".into()));
    }
    output.push_str(text);
    Ok(())
}

pub(crate) fn render(
    nodes: &[Node],
    registry: &Registry,
    data: &Value,
    options: &RenderOptions,
) -> Result<String> {
    let mut output = String::new();
    walk(
        nodes,
        registry,
        data,
        options,
        &mut HashMap::new(),
        &mut output,
    )?;
    Ok(output)
}

fn walk(
    nodes: &[Node],
    registry: &Registry,
    data: &Value,
    options: &RenderOptions,
    bindings: &mut HashMap<String, usize>,
    output: &mut String,
) -> Result<()> {
    for node in nodes {
        match node {
            Node::Static(text) => push(output, text)?,
            Node::Loop {
                source,
                stop,
                key,
                guards,
                body,
            } => {
                let array = source.value(data, options, bindings, *stop);
                let values = match array {
                    Value::Array(values) => values.as_slice(),
                    Value::Null => &[],
                    _ => return Err(Error::Template(format!("loop {key} requires an array"))),
                };
                if values.len() > 100_000 {
                    return Err(Error::Template("loop exceeds 100,000 items".into()));
                }
                for index in 0..values.len() {
                    bindings.insert(key.clone(), index);
                    // A row supported only by empty nested arrays disappears in Carbone.
                    if guards
                        .iter()
                        .any(|path| has_iteration(path, data, options, bindings, 0))
                    {
                        walk(body, registry, data, options, bindings, output)?;
                    }
                }
                bindings.remove(key);
            }
            Node::Value {
                expression,
                attribute,
            } => {
                let value = match expression {
                    Expression::Translation(key) => options
                        .translations
                        .get(&options.lang)
                        .and_then(|map| map.get(key))
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                        .unwrap_or(key)
                        .to_owned(),
                    Expression::Data(path, pipeline) => {
                        let mut value = path
                            .value(data, options, bindings, path.steps.len())
                            .clone();
                        let mut context = FormatterContext::new(data, options);
                        for formatter in pipeline {
                            if context.is_stopped() {
                                break;
                            }
                            value = registry.call(
                                &formatter.name,
                                &mut context,
                                value,
                                &formatter.args,
                            )?;
                        }
                        formatters::display(&value)
                    }
                };
                if options.escape_xml {
                    let mut escaped = escape(&value, *attribute);
                    if matches!(expression, Expression::Translation(_)) {
                        escaped = escaped.replace('\'', "&apos;").replace('¦', "&brvbar;");
                    }
                    push(output, &escaped)?;
                } else {
                    push(output, &value)?;
                }
            }
        }
    }
    Ok(())
}

fn has_iteration(
    path: &Path,
    data: &Value,
    options: &RenderOptions,
    bindings: &mut HashMap<String, usize>,
    start: usize,
) -> bool {
    for (position, step) in path.steps.iter().enumerate().skip(start) {
        if matches!(step, Step::Loop(_)) {
            let key = path.loop_key(position);
            if bindings.contains_key(&key) {
                continue;
            }
            let length = path
                .value(data, options, bindings, position)
                .as_array()
                .map_or(0, Vec::len);
            for index in 0..length {
                bindings.insert(key.clone(), index);
                let found = has_iteration(path, data, options, bindings, position + 1);
                bindings.remove(&key);
                if found {
                    return true;
                }
            }
            return false;
        }
    }
    true
}

fn escape(value: &str, attribute: bool) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' if attribute => escaped.push_str("&quot;"),
            '\'' if attribute => escaped.push_str("&apos;"),
            '\u{0}'..='\u{8}'
            | '\u{b}'..='\u{c}'
            | '\u{e}'..='\u{1f}'
            | '\u{fffe}'
            | '\u{ffff}' => {}
            _ => escaped.push(ch),
        }
    }
    escaped
}
