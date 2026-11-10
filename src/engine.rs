use std::sync::{Arc, LazyLock};

use serde_json::Value;

use crate::compile::{self, Node};
use crate::{Formatter, Registry, RenderOptions, Result, render};

static DEFAULT_ENGINE: LazyLock<Engine> = LazyLock::new(Engine::default);

/// Formatter configuration shared by compiled templates. Built-ins are enabled by default.
#[derive(Clone, Default)]
pub struct Engine {
    registry: Arc<Registry>,
}

impl Engine {
    pub fn builder() -> EngineBuilder {
        EngineBuilder::default()
    }

    /// Compile text or XML once; data and render options are supplied per render.
    pub fn compile(&self, template: &str) -> Result<CompiledTemplate> {
        Ok(CompiledTemplate {
            nodes: compile::compile(template, &self.registry)?,
            registry: self.registry.clone(),
        })
    }
}

#[derive(Default)]
pub struct EngineBuilder {
    registry: Registry,
}

impl EngineBuilder {
    /// Register a formatter, replacing the built-in with the same name if present.
    pub fn formatter<F: Formatter + 'static>(
        mut self,
        name: impl Into<String>,
        formatter: F,
    ) -> Self {
        self.registry.register(name, formatter);
        self
    }

    pub fn build(self) -> Engine {
        Engine {
            registry: Arc::new(self.registry),
        }
    }
}

/// An immutable text/XML render plan. Each render has isolated loop and formatter state.
pub struct CompiledTemplate {
    nodes: Vec<Node>,
    registry: Arc<Registry>,
}

impl CompiledTemplate {
    pub fn render(&self, data: &Value, options: &RenderOptions) -> Result<String> {
        render::render(&self.nodes, &self.registry, data, options)
    }
}

/// Compile with the default built-in formatters, without retaining template data.
pub fn compile_template(template: &str) -> Result<CompiledTemplate> {
    DEFAULT_ENGINE.compile(template)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Error, FormatterContext};
    use serde_json::json;

    #[test]
    fn compiled_template_reuses_plan_with_fresh_data_and_options() {
        let template = compile_template(
            "<xml><p>{d[i].name:upperCase} {c.suffix}</p><p>{d[i+1].name}</p></xml>",
        )
        .unwrap();
        let options = RenderOptions {
            complement: json!({"suffix": "!"}),
            ..Default::default()
        };
        assert_eq!(
            template
                .render(&json!([{"name":"one"},{"name":"two"}]), &options)
                .unwrap(),
            "<xml><p>ONE !</p><p>TWO !</p></xml>"
        );
        assert_eq!(
            template.render(&json!([]), &options).unwrap(),
            "<xml></xml>"
        );
        assert!(template.render(&json!("not an array"), &options).is_err());
        assert_eq!(
            template
                .render(&json!([{"name":"three"}]), &RenderOptions::default())
                .unwrap(),
            "<xml><p>THREE </p></xml>"
        );
    }

    #[test]
    fn compile_rejects_invalid_or_unsupported_templates_without_data() {
        for text in [
            "{d.name",
            "{d.name:unknown}",
            "<p>{d[i].name}</p>",
            "{d.name:print(.other)}",
            "{d.name:showBegin}",
        ] {
            assert!(compile_template(text).is_err(), "{text}");
        }
        // Invalid syntax in an empty loop must not be hidden by its data.
        assert!(compile_template("<xml><p>{d[i]:unknown}</p><p>{d[i+1]}</p></xml>").is_err());
    }

    #[test]
    fn compiled_template_is_send_sync_and_isolates_parallel_renders() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CompiledTemplate>();
        let template =
            Arc::new(compile_template("{d.name:ifEQ(A):show(yes):elseShow(no)} {d.name}").unwrap());
        let handles: Vec<_> = ["A", "B"]
            .into_iter()
            .map(|name| {
                let template = template.clone();
                std::thread::spawn(move || {
                    template
                        .render(&json!({"name":name}), &RenderOptions::default())
                        .unwrap()
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(results, ["yes A", "no B"]);
    }

    struct Greeting;
    impl Formatter for Greeting {
        fn validate(&self, args: &[String]) -> Result<()> {
            if args.len() != 1 {
                return Err(Error::Template("greet needs one argument".into()));
            }
            Ok(())
        }

        fn call(
            &self,
            context: &mut FormatterContext<'_>,
            value: Value,
            args: &[String],
        ) -> Result<Value> {
            Ok(Value::String(format!(
                "{} {} ({})",
                args[0],
                value.as_str().unwrap_or(""),
                context.options.lang
            )))
        }
    }

    #[test]
    fn custom_formatters_are_validated_at_compile_time_and_escaped() {
        let engine = Engine::builder().formatter("greet", Greeting).build();
        assert!(engine.compile("{d.name:greet}").is_err());
        let template = engine.compile("{d.name:greet(Hello)}").unwrap();
        assert_eq!(
            template
                .render(&json!({"name":"<Ada>"}), &RenderOptions::default())
                .unwrap(),
            "Hello &lt;Ada&gt; (en)"
        );
        assert!(matches!(
            compile_template("{d.name:greet(Hello)}"),
            Err(Error::Unsupported(_))
        ));
    }
}
