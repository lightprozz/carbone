//! Extensible formatters with isolated state for each marker chain.

use std::{collections::HashMap, sync::Arc};

use serde_json::Value;

use crate::{Error, RenderOptions, Result, conditions, extras, formatters};

/// A formatter shared by compiled templates and concurrent renders.
pub trait Formatter: Send + Sync {
    /// Validate literal arguments during compilation, without evaluating data.
    fn validate(&self, _args: &[String]) -> Result<()> {
        Ok(())
    }

    fn call(&self, ctx: &mut FormatterContext<'_>, value: Value, args: &[String]) -> Result<Value>;
}

/// Read-only render inputs and private state for one marker's formatter chain.
pub struct FormatterContext<'a> {
    pub data: &'a Value,
    pub options: &'a RenderOptions,
    pub(crate) conditions: conditions::State,
    pub(crate) argument_values: Vec<Value>,
    pub(crate) raw: bool,
    pub(crate) attribute: bool,
    pub(crate) pending_crlf: usize,
}

impl<'a> FormatterContext<'a> {
    pub(crate) fn new(data: &'a Value, options: &'a RenderOptions) -> Self {
        Self {
            data,
            options,
            conditions: conditions::State::default(),
            argument_values: Vec::new(),
            raw: false,
            attribute: false,
            pending_crlf: 0,
        }
    }

    /// Skip the remaining formatters in this marker's chain.
    pub fn stop(&mut self) {
        self.conditions.stop = true;
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.conditions.stop
    }
}

/// Named formatters. Clones share implementations but can register independently.
#[derive(Clone)]
pub struct Registry {
    formatters: HashMap<String, Arc<dyn Formatter>>,
}

impl Registry {
    /// Register a formatter, replacing any existing formatter with this name.
    pub fn register<F: Formatter + 'static>(&mut self, name: impl Into<String>, formatter: F) {
        self.formatters.insert(name.into(), Arc::new(formatter));
    }

    pub(crate) fn validate(&self, name: &str, args: &[String]) -> Result<()> {
        self.get(name)?.validate(args)
    }

    pub(crate) fn validate_dynamic(&self, name: &str) -> Result<()> {
        self.get(name).map(|_| ())
    }

    pub(crate) fn call(
        &self,
        name: &str,
        ctx: &mut FormatterContext<'_>,
        value: Value,
        args: &[String],
    ) -> Result<Value> {
        self.get(name)?.call(ctx, value, args)
    }

    fn get(&self, name: &str) -> Result<&dyn Formatter> {
        self.formatters.get(name).map(Arc::as_ref).ok_or_else(|| {
            Error::Unsupported(format!(
                "Formatter {name} is not supported by the native renderer"
            ))
        })
    }
}

impl Default for Registry {
    fn default() -> Self {
        let mut registry = Self {
            formatters: HashMap::new(),
        };
        for name in [
            "lowerCase",
            "upperCase",
            "ucFirst",
            "ucWords",
            "unaccent",
            "print",
            "prepend",
            "substr",
            "slice",
            "md5",
            "arrayJoin",
            "arrayMap",
            "len",
            "int",
            "toEN",
            "toFR",
            "round",
            "add",
            "sub",
            "mul",
            "div",
            "padl",
            "padr",
            "toFixed",
        ] {
            registry.register(name, Builtin(name));
        }
        for &name in extras::NAMES {
            registry.register(name, Extra(name));
        }
        for &name in conditions::NAMES {
            registry.register(name, Condition(name));
        }
        registry
    }
}

struct Builtin(&'static str);

impl Formatter for Builtin {
    fn validate(&self, args: &[String]) -> Result<()> {
        if matches!(self.0, "add" | "sub" | "mul" | "div") {
            let expression = args.first().ok_or_else(|| {
                Error::Template(format!(
                    "Formatter {} requires a numeric expression",
                    self.0
                ))
            })?;
            if extras::arithmetic(expression, &Value::Null)? == "NaN" {
                return Err(Error::Template(format!(
                    "Formatter {} requires a numeric expression",
                    self.0
                )));
            }
            Ok(())
        } else {
            formatters::validate(self.0, args)
        }
    }

    fn call(
        &self,
        _ctx: &mut FormatterContext<'_>,
        value: Value,
        args: &[String],
    ) -> Result<Value> {
        if matches!(self.0, "add" | "sub" | "mul" | "div") {
            extras::calculate(self.0, value, args)
        } else {
            formatters::apply(self.0, value, args)
        }
    }
}

struct Extra(&'static str);

impl Formatter for Extra {
    fn call(&self, ctx: &mut FormatterContext<'_>, value: Value, args: &[String]) -> Result<Value> {
        if self.0 == "convCRLF" {
            ctx.pending_crlf += 1;
            return Ok(value);
        }
        if self.0 == "count" {
            let id = args.first().map(String::as_str).unwrap_or("undefined");
            let start = args.get(1).map(String::as_str).unwrap_or("1");
            return Ok(Value::String(format!("__COUNT_{id}_{start}__")));
        }
        let output = extras::apply(
            self.0,
            value,
            args,
            &extras::ExtrasContext {
                enums: &ctx.options.enums,
                extension: &ctx.options.extension,
                count: 0,
                escape_xml: ctx.options.escape_xml && !ctx.raw,
                attribute: ctx.attribute,
            },
        )?;
        ctx.raw |= output.raw;
        Ok(output.value)
    }
}

/// Finalize XML-injecting formatters before counter assembly.
pub(crate) fn finish(ctx: &mut FormatterContext<'_>, mut value: Value) -> Result<Value> {
    for _ in 0..std::mem::take(&mut ctx.pending_crlf) {
        let output = extras::apply(
            "convCRLF",
            value,
            &[],
            &extras::ExtrasContext {
                enums: &ctx.options.enums,
                extension: &ctx.options.extension,
                count: 0,
                escape_xml: ctx.options.escape_xml && !ctx.raw,
                attribute: ctx.attribute,
            },
        )?;
        ctx.raw |= output.raw;
        value = output.value;
    }
    Ok(value)
}

struct Condition(&'static str);

impl Formatter for Condition {
    fn call(&self, ctx: &mut FormatterContext<'_>, value: Value, args: &[String]) -> Result<Value> {
        if matches!(self.0, "and" | "or") && !ctx.argument_values.is_empty() {
            ctx.conditions.apply(self.0, value, &[]);
            return Ok(ctx.argument_values[0].clone());
        }
        Ok(ctx.conditions.apply(self.0, value, args))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use serde_json::json;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    struct Contextual;

    impl Formatter for Contextual {
        fn call(
            &self,
            ctx: &mut FormatterContext<'_>,
            value: Value,
            args: &[String],
        ) -> Result<Value> {
            ctx.stop();
            Ok(json!({
                "value": value,
                "data": ctx.data,
                "lang": ctx.options.lang,
                "args": args,
            }))
        }
    }

    #[test]
    fn custom_formatter_reads_context_and_stops_its_marker() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Registry>();

        let mut registry = Registry::default();
        registry.register("custom", Contextual);
        registry.validate("custom", &[]).unwrap();
        let data = json!({"name": "Ada"});
        let options = RenderOptions::default();
        let mut ctx = FormatterContext::new(&data, &options);
        let output = registry
            .call("custom", &mut ctx, json!(42), &args(&["extra"]))
            .unwrap();
        assert_eq!(
            output,
            json!({
                "value": 42,
                "data": {"name": "Ada"},
                "lang": "en",
                "args": ["extra"],
            })
        );
        assert!(ctx.is_stopped());
        assert!(!FormatterContext::new(&data, &options).is_stopped());
    }

    #[test]
    fn default_registry_delegates_all_existing_formatters() {
        let registry = Registry::default();
        let data = Value::Null;
        let options = RenderOptions::default();
        let mut ctx = FormatterContext::new(&data, &options);
        for (name, formatter) in &registry.formatters {
            if conditions::NAMES.contains(&name.as_str()) {
                formatter.validate(&[]).unwrap();
                let mut isolated = FormatterContext::new(&data, &options);
                let mut state = conditions::State::default();
                let input = json!("Ada");
                let arguments = args(&["Ada", "match"]);
                assert_eq!(
                    registry
                        .call(name, &mut isolated, input.clone(), &arguments)
                        .unwrap(),
                    state.apply(name, input, &arguments),
                    "{name}",
                );
                assert_eq!(isolated.is_stopped(), state.stop, "{name}");
            } else if extras::NAMES.contains(&name.as_str()) {
                registry.validate(name, &[]).unwrap();
                let mut isolated = FormatterContext::new(&data, &options);
                let input = json!("12");
                let actual = registry
                    .call(name, &mut isolated, input.clone(), &[])
                    .unwrap();
                if name == "count" {
                    assert_eq!(actual, json!("__COUNT_undefined_1__"));
                    continue;
                }
                assert_eq!(
                    finish(&mut isolated, actual).unwrap(),
                    extras::apply(
                        name,
                        input,
                        &[],
                        &extras::ExtrasContext {
                            enums: &options.enums,
                            extension: &options.extension,
                            count: 0,
                            escape_xml: true,
                            attribute: false,
                        }
                    )
                    .unwrap()
                    .value,
                    "{name}",
                );
            } else {
                let arguments = args(&["2"]);
                registry.validate(name, &arguments).unwrap();
                let input = json!("12");
                assert_eq!(
                    registry
                        .call(name, &mut ctx, input.clone(), &arguments)
                        .unwrap(),
                    formatters::apply(name, input, &arguments).unwrap(),
                    "{name}",
                );
            }
        }
        assert_eq!(registry.formatters.len(), 41 + extras::NAMES.len());
    }

    #[test]
    fn contextual_adapters_match_node_and_keep_raw_state_per_marker() {
        let corpus: Value =
            serde_json::from_str(include_str!("../tests/formatter-fixtures/b6-extras.json"))
                .unwrap();
        let registry = Registry::default();
        for case in corpus["cases"].as_array().unwrap() {
            if case["kind"] != "extra" && case["kind"] != "calculation" {
                continue;
            }
            let options = RenderOptions {
                enums: case["context"]["enum"].clone(),
                extension: case["context"]["extension"].as_str().unwrap_or("").into(),
                ..Default::default()
            };
            let mut ctx = FormatterContext::new(&Value::Null, &options);
            let mut args = case["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| arg.as_str().unwrap().into())
                .collect::<Vec<_>>();
            if case["name"] == "count" {
                args.insert(0, "123".into());
            }
            let actual = registry.call(
                case["name"].as_str().unwrap(),
                &mut ctx,
                case["value"].clone(),
                &args,
            );
            if case.get("nonfinite").is_some() {
                assert!(matches!(actual, Err(Error::Template(_))), "{}", case["id"]);
                continue;
            }
            if case["name"] == "convCRLF"
                && case["value"].is_string()
                && !matches!(options.extension.as_str(), "odt" | "docx")
            {
                continue;
            }
            let mut actual = finish(&mut ctx, actual.unwrap()).unwrap();
            if case["name"] == "count" {
                let token = actual.as_str().unwrap();
                let mut counters = HashMap::new();
                let mut text = String::new();
                for _ in 0..=case["context"]["count"].as_u64().unwrap_or(0) {
                    text = token.into();
                    crate::render::resolve_counter(&mut text, &mut counters);
                }
                actual = Value::String(text);
            }
            assert_eq!(actual, case["expected"], "{}", case["id"]);
            if ctx.raw {
                let repeated = registry
                    .call("convCRLF", &mut ctx, actual.clone(), &[])
                    .unwrap();
                assert_eq!(
                    finish(&mut ctx, repeated).unwrap(),
                    actual,
                    "{}",
                    case["id"]
                );
            }
            assert!(!FormatterContext::new(&Value::Null, &options).raw);
        }
    }

    #[test]
    fn count_chains_see_the_reference_token_before_assembly() {
        let registry = Registry::default();
        let options = RenderOptions::default();
        let mut ctx = FormatterContext::new(&Value::Null, &options);
        let token = registry
            .call("count", &mut ctx, Value::Null, &args(&["09", "5"]))
            .unwrap();
        assert_eq!(token, json!("__COUNT_09_5__"));
        assert_eq!(
            registry.call("len", &mut ctx, token.clone(), &[]).unwrap(),
            json!(14)
        );
        assert_eq!(
            registry.call("lowerCase", &mut ctx, token, &[]).unwrap(),
            json!("__count_09_5__")
        );
    }

    #[test]
    fn deferred_chains_match_node_render_oracle() {
        let corpus: Value =
            serde_json::from_str(include_str!("../tests/fixtures/b6-render.json")).unwrap();
        let registry = Registry::default();
        for case in corpus["cases"].as_array().unwrap() {
            let Some(template) = case["template"].as_str() else {
                continue;
            };
            if !matches!(
                case["id"].as_str().unwrap(),
                "count-upper"
                    | "count-prepend"
                    | "count-replaced"
                    | "crlf-twice"
                    | "crlf-reordered"
                    | "crlf-escaped"
            ) {
                continue;
            }
            let start = template.find('{').unwrap();
            let end = template[start..].find('}').unwrap() + start;
            let crate::syntax::Expression::Data(_, pipeline) =
                crate::syntax::parse_expression(&template[start + 1..end]).unwrap()
            else {
                panic!()
            };
            let options = RenderOptions {
                extension: case["options"]["extension"].as_str().unwrap_or("").into(),
                ..Default::default()
            };
            let mut output = String::new();
            let values = case["data"].as_array().map_or_else(
                || vec![case["data"]["value"].clone()],
                |items| items.iter().map(|item| item["name"].clone()).collect(),
            );
            let mut cursor = template[..start].to_owned();
            let mut counters = HashMap::new();
            for mut value in values {
                let mut ctx = FormatterContext::new(&Value::Null, &options);
                for formatter in &pipeline {
                    let mut arguments = formatter.args.clone();
                    if formatter.name == "count" {
                        arguments.insert(0, "09".into());
                    }
                    value = registry
                        .call(&formatter.name, &mut ctx, value, &arguments)
                        .unwrap();
                }
                value = finish(&mut ctx, value).unwrap();
                if let Value::String(text) = &mut value {
                    crate::render::resolve_counter(text, &mut counters);
                }
                if template.contains("<r>") {
                    output.push_str(&format!("<r>{}</r>", formatters::display(&value)));
                } else {
                    cursor.push_str(&formatters::display(&value));
                    cursor.push_str(&template[end + 1..]);
                    output = cursor.clone();
                }
            }
            if template.contains("<r>") {
                output = format!("<xml>{output}</xml>");
            }
            assert_eq!(output, case["expected"], "{}", case["id"]);
        }
    }

    #[test]
    fn condition_state_persists_across_builtins_but_not_markers() {
        let registry = Registry::default();
        let data = Value::Null;
        let options = RenderOptions::default();
        let mut first = FormatterContext::new(&data, &options);
        let mut second = FormatterContext::new(&data, &options);

        let value = registry
            .call("ifEQ", &mut first, json!("Ada"), &args(&["Ada"]))
            .unwrap();
        let value = registry.call("lowerCase", &mut first, value, &[]).unwrap();
        assert_eq!(
            registry
                .call("show", &mut first, value, &args(&["match"]))
                .unwrap(),
            json!("match")
        );
        assert!(first.is_stopped());

        // A leaked true condition would incorrectly select show for this false value.
        assert_eq!(
            registry
                .call("show", &mut second, json!(false), &args(&["wrong"]))
                .unwrap(),
            json!(false)
        );
        assert!(!second.is_stopped());
        assert_eq!(
            registry
                .call("elseShow", &mut second, json!(false), &args(&["fresh"]))
                .unwrap(),
            json!("fresh")
        );
        assert!(second.is_stopped());
    }

    struct Validated {
        calls: Arc<AtomicUsize>,
    }

    impl Formatter for Validated {
        fn validate(&self, args: &[String]) -> Result<()> {
            if args.len() != 1 {
                return Err(Error::Template("one argument required".into()));
            }
            Ok(())
        }

        fn call(
            &self,
            _ctx: &mut FormatterContext<'_>,
            value: Value,
            _args: &[String],
        ) -> Result<Value> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(value)
        }
    }

    #[test]
    fn validation_rejects_arguments_without_rendering() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = Registry::default();
        registry.register(
            "validated",
            Validated {
                calls: calls.clone(),
            },
        );
        assert!(matches!(
            registry.validate("validated", &[]),
            Err(Error::Template(_))
        ));
        registry.validate("validated", &args(&["ok"])).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        registry.validate("add", &args(&["1 + 2 * 3"])).unwrap();

        for (name, arguments) in [
            ("add", args(&["1 ++ 2"])),
            ("sub", vec![]),
            ("mul", args(&["bad"])),
            ("div", args(&["bad"])),
            ("padl", args(&["10001"])),
            ("padr", args(&["10001"])),
            ("toFixed", args(&["101"])),
        ] {
            assert!(
                matches!(registry.validate(name, &arguments), Err(Error::Template(_))),
                "{name}"
            );
        }
    }

    #[test]
    fn clones_share_formatters_and_allow_independent_replacement() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut original = Registry::default();
        original.register(
            "custom",
            Validated {
                calls: calls.clone(),
            },
        );
        let mut cloned = original.clone();
        let data = Value::Null;
        let options = RenderOptions::default();
        let mut ctx = FormatterContext::new(&data, &options);
        original.call("custom", &mut ctx, Value::Null, &[]).unwrap();
        cloned.call("custom", &mut ctx, Value::Null, &[]).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        cloned.register("custom", Contextual);
        assert!(original.validate("custom", &[]).is_err());
        cloned.validate("custom", &[]).unwrap();
        cloned.register("upperCase", Contextual);
        assert_eq!(
            original
                .call("upperCase", &mut ctx, json!("Ada"), &[])
                .unwrap(),
            json!("ADA")
        );
        assert!(
            cloned
                .call("upperCase", &mut ctx, json!("Ada"), &[])
                .unwrap()
                .is_object()
        );
    }

    #[test]
    fn unknown_names_fail_during_validation_and_calls() {
        let registry = Registry::default();
        let data = Value::Null;
        let options = RenderOptions::default();
        let mut ctx = FormatterContext::new(&data, &options);
        assert!(matches!(
            registry.validate("missing", &[]),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            registry.call("missing", &mut ctx, Value::Null, &[]),
            Err(Error::Unsupported(_))
        ));
    }
}
