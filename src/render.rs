use serde_json::Value;

use crate::compile::Node;
use crate::registry::{FormatterContext, Registry};
use crate::selection::{self, Bindings};
use crate::syntax::{Argument, Expression, Path};
use crate::{Error, RenderOptions, Result, formatters};

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
                let values = selection::candidates(source, *stop, data, options, bindings)?;
                for binding in values {
                    bindings.insert(key.clone(), binding);
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
                        let (mut value, parents) =
                            path.resolve(data, options, bindings, path.steps.len());
                        let mut context = FormatterContext::new(data, options);
                        for formatter in pipeline {
                            if context.is_stopped() {
                                break;
                            }
                            let mut args = Vec::new();
                            let mut argument_values = Vec::new();
                            for (argument, kind) in formatter.args.iter().zip(&formatter.arguments)
                            {
                                let resolved = match kind {
                                    Argument::Literal => Value::String(argument.clone()),
                                    Argument::Relative { levels, path } => parents
                                        .len()
                                        .checked_sub(*levels)
                                        .and_then(|index| parents.get(index))
                                        .map_or(Value::Null, |parent| {
                                            selection::member(parent, path)
                                        }),
                                    Argument::Root(path) => {
                                        path.resolve(data, options, bindings, path.steps.len()).0
                                    }
                                    Argument::Math(_) => {
                                        return Err(Error::Unsupported(
                                            "arithmetic expression pending".into(),
                                        ));
                                    }
                                };
                                args.push(if resolved.is_null() {
                                    String::new()
                                } else {
                                    formatters::display(&resolved)
                                });
                                argument_values.push(resolved);
                            }
                            context.argument_values = argument_values;
                            value = registry.call(&formatter.name, &mut context, value, &args)?;
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
