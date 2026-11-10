use std::{collections::HashMap, sync::OnceLock};

use regex::Regex;

use serde_json::Value;

use crate::compile::Node;
use crate::registry::{FormatterContext, Registry};
use crate::selection::{self, Bindings};
use crate::syntax::{Argument, Expression, Path};
use crate::{Error, RenderOptions, Result, formatters};

/// The reference resolves the first surviving token in each XML value part.
pub(crate) fn resolve_counter(text: &mut String, counters: &mut HashMap<String, f64>) {
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    let regex = TOKEN.get_or_init(|| Regex::new(r"__COUNT_([0-9]*?)_([0-9]*?)__").unwrap());
    if let Some(captures) = regex.captures(text) {
        let token = captures.get(0).unwrap();
        let number = counters.entry(captures[1].into()).or_insert_with(|| {
            if captures[2].is_empty() {
                0.0
            } else {
                captures[2].parse().unwrap_or(f64::INFINITY)
            }
        });
        let replacement = crate::extras::number_text(*number);
        *number += 1.0;
        text.replace_range(token.range(), &replacement);
    }
}

const MAX_OUTPUT: usize = 64 * 1024 * 1024;

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
        &mut Bindings::new(),
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
    bindings: &mut Bindings,
    counts: &mut HashMap<String, f64>,
    output: &mut String,
) -> Result<()> {
    for node in nodes {
        match node {
            Node::Static(text) => push(output, text)?,
            Node::Block {
                expression,
                hide,
                body,
            } => {
                let (value, context) =
                    evaluate(expression, false, registry, data, options, bindings)?;
                if context.is_stopped() {
                    let text = formatters::display(&value);
                    push(
                        output,
                        &if options.escape_xml && !context.raw {
                            escape(&text, false)
                        } else {
                            text
                        },
                    )?;
                }
                if context.is_stopped() || context.conditions.selected(&value) != *hide {
                    walk(body, registry, data, options, bindings, counts, output)?;
                } else {
                    // The reference evaluates hidden values before filtering XML.
                    walk(
                        body,
                        registry,
                        data,
                        options,
                        bindings,
                        counts,
                        &mut String::new(),
                    )?;
                }
            }
            Node::Loop {
                source,
                stop,
                key,
                guards,
                body,
            } => {
                let values = selection::candidates(source, *stop, data, options, bindings)?;
                for binding in values {
                    bindings.insert(key.clone(), binding);
                    if guards
                        .iter()
                        .any(|path| has_iteration(path, data, options, bindings, 0))
                    {
                        walk(body, registry, data, options, bindings, counts, output)?;
                    }
                }
                bindings.remove(key);
            }
            Node::Value {
                expression,
                attribute,
            } => {
                let (value, context) =
                    evaluate(expression, *attribute, registry, data, options, bindings)?;
                let mut value = formatters::display(&value);
                resolve_counter(&mut value, counts);
                if options.escape_xml && !context.raw {
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

#[allow(clippy::too_many_arguments)]
fn evaluate<'a>(
    expression: &Expression,
    attribute: bool,
    registry: &Registry,
    data: &'a Value,
    options: &'a RenderOptions,
    bindings: &Bindings,
) -> Result<(Value, FormatterContext<'a>)> {
    let mut context = FormatterContext::new(data, options);
    context.attribute = attribute;
    let value = match expression {
        Expression::Translation(key) => Value::String(
            options
                .translations
                .get(&options.lang)
                .and_then(|map| map.get(key))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or(key)
                .to_owned(),
        ),
        Expression::Data(path, pipeline) => {
            let (mut value, parents) = path.resolve(data, options, bindings, path.steps.len());
            if path
                .steps
                .last()
                .is_some_and(|step| step.iteration().is_some())
                && (value.is_object() || value.is_array())
            {
                return Ok((Value::Null, context));
            }
            for formatter in pipeline {
                if context.is_stopped() {
                    break;
                }
                let mut args = Vec::new();
                let mut argument_values = Vec::new();
                for (argument, kind) in formatter.args.iter().zip(&formatter.arguments) {
                    let resolved =
                        resolve_argument(argument, kind, &parents, data, options, bindings)?;
                    if formatter.name == "count" && args.is_empty() && resolved.is_null() {
                        return Err(Error::Unsupported("missing or null count arguments".into()));
                    }
                    args.push(if resolved.is_null() {
                        String::new()
                    } else {
                        formatters::display(&resolved)
                    });
                    argument_values.push(resolved);
                }
                if let Some(id) = &formatter.counter_id {
                    args.insert(0, id.clone());
                    argument_values.insert(0, Value::String(id.clone()));
                }
                context.argument_values = argument_values;
                value = registry.call(&formatter.name, &mut context, value, &args)?;
            }
            value
        }
    };
    let value = crate::registry::finish(&mut context, value)?;
    Ok((value, context))
}

fn resolve_argument(
    argument: &str,
    kind: &Argument,
    parents: &[Value],
    data: &Value,
    options: &RenderOptions,
    bindings: &Bindings,
) -> Result<Value> {
    Ok(match kind {
        Argument::Literal => Value::String(argument.into()),
        Argument::Relative { levels, path } => parents
            .len()
            .checked_sub(*levels)
            .and_then(|index| parents.get(index))
            .map_or(Value::Null, |parent| relative_member(parent, path)),
        Argument::Math(expression) => {
            Value::String(crate::extras::arithmetic_with(expression, |operand| {
                if operand.starts_with('.')
                    && operand
                        .as_bytes()
                        .get(1)
                        .is_none_or(|byte| !byte.is_ascii_digit())
                {
                    let levels = operand.chars().take_while(|ch| *ch == '.').count();
                    resolve_argument(
                        operand,
                        &Argument::Relative {
                            levels,
                            path: operand[levels..].into(),
                        },
                        parents,
                        data,
                        options,
                        bindings,
                    )
                } else {
                    Ok(Value::String(operand.into()))
                }
            })?)
        }
    })
}

fn relative_member(value: &Value, path: &str) -> Value {
    path.split('.').fold(value.clone(), |value, key| {
        if key == "length" {
            selection::member(&value, key)
        } else if let Value::Array(items) = &value {
            key.parse::<usize>()
                .ok()
                .and_then(|i| items.get(i))
                .cloned()
                .unwrap_or(Value::Null)
        } else {
            value.get(key).cloned().unwrap_or(Value::Null)
        }
    })
}

fn has_iteration(
    path: &Path,
    data: &Value,
    options: &RenderOptions,
    bindings: &mut Bindings,
    start: usize,
) -> bool {
    for (position, step) in path.steps.iter().enumerate().skip(start) {
        if step.iteration().is_some() {
            let key = path.loop_key(position);
            if bindings.contains_key(&key) {
                continue;
            }
            if let Ok(values) = selection::candidates(path, position, data, options, bindings) {
                for binding in values {
                    bindings.insert(key.clone(), binding);
                    let found = has_iteration(path, data, options, bindings, position + 1);
                    bindings.remove(&key);
                    if found {
                        return true;
                    }
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
