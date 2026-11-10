//! Contextual formatters and a non-executing arithmetic operand evaluator.

use serde_json::Value;

use crate::{Error, Result, formatters};

pub(crate) const NAMES: &[&str] = &["count", "convEnum", "convCRLF"];

pub(crate) struct ExtrasContext<'a> {
    pub enums: &'a Value,
    pub extension: &'a str,
    /// Zero-based occurrence of this marker among visible rendered rows.
    pub count: usize,
    pub escape_xml: bool,
    pub attribute: bool,
}

pub(crate) struct Output {
    pub value: Value,
    /// The value has already been escaped; only formatter-owned XML is raw.
    pub raw: bool,
}

pub(crate) fn apply(
    name: &str,
    value: Value,
    args: &[String],
    ctx: &ExtrasContext<'_>,
) -> Result<Output> {
    let mut raw = false;
    let value = match name {
        "count" => {
            let start = args.first().map(String::as_str).unwrap_or("1");
            if !start.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Error::Unsupported(
                    "count start must be a nonnegative integer".into(),
                ));
            }
            let start = if start.is_empty() {
                0.0
            } else {
                formatters::parse_float(start)
            };
            Value::String(number_text(start + ctx.count as f64))
        }
        "convEnum" => {
            let key = args.first().map(String::as_str).unwrap_or("undefined");
            match property(ctx.enums, key)? {
                Some(map) => property(&map, &js_string(&value))?.unwrap_or(value),
                None => value,
            }
        }
        "convCRLF" => match (value, ctx.extension) {
            (Value::String(text), "odt" | "docx") => {
                if ctx.attribute && text.contains('\n') {
                    return Err(Error::Unsupported(
                        "convCRLF cannot insert XML elements into an attribute".into(),
                    ));
                }
                let text = if ctx.escape_xml {
                    escape(&text, ctx.attribute)
                } else {
                    text
                };
                let line_break = if ctx.extension == "odt" {
                    "<text:line-break/>"
                } else {
                    "</w:t><w:br/><w:t>"
                };
                raw = true;
                let mut converted = String::with_capacity(text.len());
                let mut chars = text.chars().peekable();
                while let Some(ch) = chars.next() {
                    if ch == '\n' {
                        converted.push_str(line_break);
                    } else if ch != '\r' || chars.peek() != Some(&'\n') {
                        converted.push(ch);
                    }
                }
                Value::String(converted)
            }
            (value, _) => value,
        },
        _ => {
            return Err(Error::Unsupported(format!(
                "Formatter {name} is not supported by contextual formatters"
            )));
        }
    };
    Ok(Output { value, raw })
}

fn js_string(value: &Value) -> String {
    if value.is_null() {
        "null".into()
    } else {
        formatters::display(value)
    }
}

fn property(value: &Value, key: &str) -> Result<Option<Value>> {
    let index = || {
        key.parse::<usize>()
            .ok()
            .filter(|index| index.to_string() == key)
    };
    Ok(match value {
        Value::Object(map) => map.get(key).cloned(),
        Value::Array(items) if key == "length" => Some(Value::from(items.len())),
        Value::Array(items) => index().and_then(|index| items.get(index).cloned()),
        Value::String(text) if key == "length" => Some(Value::from(text.encode_utf16().count())),
        Value::String(text) => match index().and_then(|index| text.encode_utf16().nth(index)) {
            Some(unit) => Some(Value::String(String::from_utf16(&[unit]).map_err(
                |_| {
                    Error::Unsupported(
                        "enum string indexing produced an unpaired UTF-16 surrogate".into(),
                    )
                },
            )?)),
            None => None,
        },
        _ => None,
    })
}

fn escape(text: &str, attribute: bool) -> String {
    let mut output = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' if attribute => output.push_str("&quot;"),
            '\'' if attribute => output.push_str("&apos;"),
            '\u{0}'..='\u{8}'
            | '\u{b}'..='\u{c}'
            | '\u{e}'..='\u{1f}'
            | '\u{fffe}'
            | '\u{ffff}' => {}
            _ => output.push(ch),
        }
    }
    output
}

/// Evaluate numeric operands after the renderer has resolved any data references.
pub(crate) fn arithmetic(expression: &str, _value: &Value) -> Result<String> {
    arithmetic_with(expression, |operand| Ok(Value::String(operand.into())))
}

/// Resolve complete operands before parseFloat coercion, preserving dashed paths.
pub(crate) fn arithmetic_with(
    expression: &str,
    mut resolve: impl FnMut(&str) -> Result<Value>,
) -> Result<String> {
    if expression.len() > 64 * 1024 {
        return Err(Error::Template(
            "arithmetic expression exceeds 64 KiB".into(),
        ));
    }
    let bad_expression =
        || Error::Template(format!("Bad Mathematical Expression in \"{expression}\""));
    let mut cursor = expression.len() as isize - 1;
    let mut previous_variable = String::new();
    let mut previous_operator = None;
    let mut operands = Vec::new();
    // The reference joins nonnumeric fragments backwards, including dashes in paths.
    for variable in expression.split(['*', '+', '/', '-']).rev() {
        cursor -= variable.len() as isize + 1;
        let operator = usize::try_from(cursor + 1)
            .ok()
            .and_then(|index| expression.as_bytes().get(index))
            .copied();
        let variable = variable.trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{feff}');
        if variable.is_empty() && previous_operator != Some(b'-') {
            if expression.trim().is_empty() {
                return Ok("NaN".into());
            }
            return Err(bad_expression());
        }
        if !variable
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'.')
        {
            previous_variable = format!(
                "{}{variable}{previous_variable}",
                operator
                    .map(char::from)
                    .map(String::from)
                    .unwrap_or_default()
            );
            continue;
        }
        let operand = resolve(&format!("{variable}{previous_variable}"))?;
        operands.push((operator, formatters::parse_float(&js_string(&operand))));
        previous_variable.clear();
        previous_operator = operator;
    }
    operands.reverse();
    let mut operands = operands.into_iter();
    let Some((first_operator, first)) = operands.next() else {
        return Ok("NaN".into());
    };
    let mut term = match first_operator {
        None => first,
        Some(b'-') => -first,
        _ => return Err(bad_expression()),
    };
    let mut total = 0.0;
    for (operator, value) in operands {
        match operator {
            Some(b'*') => term *= value,
            Some(b'/') => term /= value,
            Some(b'+') => {
                total += term;
                term = value;
            }
            Some(b'-') => {
                total += term;
                term = -value;
            }
            _ => return Err(bad_expression()),
        }
    }
    Ok(number_text(total + term))
}

fn number_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".into()
    } else if value == f64::INFINITY {
        "Infinity".into()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".into()
    } else {
        formatters::display(&Value::from(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/b6-extras.json")).unwrap()
    }

    #[test]
    fn arithmetic_matches_node_parser_oracle() {
        for case in corpus()["cases"].as_array().unwrap() {
            if case["kind"] != "arithmetic" {
                continue;
            }
            let expression = case["expression"].as_str().unwrap();
            let actual = if let Some(operands) = case.get("operands") {
                arithmetic_with(expression, |operand| {
                    Ok(operands
                        .get(operand)
                        .cloned()
                        .unwrap_or_else(|| Value::String(operand.into())))
                })
            } else {
                arithmetic(expression, &Value::Null)
            };
            if let Some(error) = case["error"].as_str() {
                let Err(Error::Template(actual)) = actual else {
                    panic!("{}: expected error {error}, got {actual:?}", case["id"]);
                };
                assert_eq!(actual, error, "{}", case["id"]);
            } else {
                assert_eq!(actual.unwrap(), case["expected"], "{}", case["id"]);
            }
        }
    }

    #[test]
    fn contextual_formatters_match_node_oracle() {
        for case in corpus()["cases"].as_array().unwrap() {
            if case["kind"] != "extra" {
                continue;
            }
            let options = &case["context"];
            let context = ExtrasContext {
                enums: &options["enum"],
                extension: options["extension"].as_str().unwrap_or(""),
                count: options["count"].as_u64().unwrap_or(0) as usize,
                escape_xml: true,
                attribute: false,
            };
            let args = case["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| arg.as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            let output = apply(
                case["name"].as_str().unwrap(),
                case["value"].clone(),
                &args,
                &context,
            )
            .unwrap();
            let actual = if case["rendered"] == true && !output.raw {
                match output.value {
                    Value::String(text) => Value::String(escape(&text, false)),
                    value => value,
                }
            } else {
                output.value
            };
            assert_eq!(actual, case["expected"], "{}", case["id"]);
        }
    }
}
