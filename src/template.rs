use serde_json::Value;

use crate::{RenderOptions, Result, compile_template};

/// Render XML or text without executing template code.
///
/// Use [`compile_template`] to reuse a template across renders.
pub fn render_template(template: &str, data: &Value, options: &RenderOptions) -> Result<String> {
    compile_template(template)?.render(data, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn render_default(template: &str, data: Value) -> String {
        render_template(template, &data, &RenderOptions::default()).unwrap()
    }

    #[test]
    fn scalar_paths_and_complement() {
        let options = RenderOptions {
            complement: json!({"date":"today"}),
            ..Default::default()
        };
        assert_eq!(
            render_template(
                "{d.name} {c.date} {d.list[i=1]} {d.missing}",
                &json!({"name":"Ada", "list":[4,5]}),
                &options
            )
            .unwrap(),
            "Ada today 5 "
        );
    }

    #[test]
    fn escapes_data_but_preserves_template_and_literal_braces() {
        assert_eq!(
            render_default(
                "<p>{d.name}</p>{uuid}{m.x}",
                json!({"name":"<&>\u{0000}'\""})
            ),
            "<p>&lt;&amp;&gt;'\"</p>{uuid}{m.x}"
        );
        assert_eq!(
            render_default("<p title=\"{d.name}\">x</p>", json!({"name":"\" a='b'"})),
            "<p title=\"&quot; a=&apos;b&apos;\">x</p>"
        );
    }

    #[test]
    fn split_runs_and_chained_formatters() {
        assert_eq!(
            render_default(
                "<p><b>{d.na</b><b>me:upperCase}</b></p>",
                json!({"name":"Ada"})
            ),
            "<p><b>ADA</b><b></b></p>"
        );
        assert_eq!(
            render_default(
                "{d.name:upperCase:prepend('Hello: ')}",
                json!({"name":"Ada"})
            ),
            "Hello: ADA"
        );
    }

    #[test]
    fn sibling_repetition_and_empty_arrays() {
        let template = "<xml><tr>{d[i].name}</tr> <tr>{d[i+1].name}</tr></xml>";
        assert_eq!(
            render_default(template, json!([{ "name":"A" }, { "name":"B" }])),
            "<xml><tr>A</tr> <tr>B</tr> </xml>"
        );
        assert_eq!(render_default(template, json!([])), "<xml></xml>");
    }

    #[test]
    fn nested_repetition_and_literal_data_markers() {
        let template = "<root><group><h>{d[i].name}</h><p>{d[i].items[i]}</p><p>{d[i].items[i+1]}</p></group><group>{d[i+1].name}</group></root>";
        assert_eq!(
            render_default(
                template,
                json!([{ "name":"{d.secret}", "items":["A","B"] }, { "name":"two", "items":["C"] }])
            ),
            "<root><group><h>{d.secret}</h><p>A</p><p>B</p></group><group><h>two</h><p>C</p></group></root>"
        );
    }

    #[test]
    fn translates_with_fallback_and_escapes() {
        let options = RenderOptions {
            lang: "fr".into(),
            translations: json!({"fr":{"Hello":"Bonjour & salut"}}),
            ..Default::default()
        };
        assert_eq!(
            render_template("{t(Hello)} {t(Unknown)}", &Value::Null, &options).unwrap(),
            "Bonjour &amp; salut Unknown"
        );
    }

    #[test]
    fn independent_repetition_sections() {
        let template = "<root><a>{d.items[i].name}</a><a>{d.items[i+1].name}</a><b>{d.items[i].name}</b><b>{d.items[i+1].name}</b></root>";
        assert_eq!(
            render_default(template, json!({"items":[{"name":"A"},{"name":"B"}]})),
            "<root><a>A</a><a>B</a><b>A</b><b>B</b></root>"
        );
        let template = "<root><a>{d.first[i]}</a><a>{d.first[i+1]}</a><group><b>{d.second[i]}</b><b>{d.second[i+1]}</b></group></root>";
        assert_eq!(
            render_default(template, json!({"first":[1,2],"second":[3,4]})),
            "<root><a>1</a><a>2</a><group><b>3</b><b>4</b></group></root>"
        );
    }

    #[test]
    fn literal_braces_and_empty_loop_validation() {
        assert_eq!(
            render_default("{a < b} {nothing}", json!({})),
            "{a < b} {nothing}"
        );
        for formatter in ["unknown", "count", "padl(10001)", "toFixed(101)"] {
            let template =
                format!("<root><p>{{d[i].name:{formatter}}}</p><p>{{d[i+1].name}}</p></root>");
            assert!(render_template(&template, &json!([]), &RenderOptions::default()).is_err());
        }
        let deep = format!("{{d{}.name}}", ".child".repeat(65));
        assert!(render_template(&deep, &json!({}), &RenderOptions::default()).is_err());
    }

    #[test]
    fn malformed_and_unsupported_markers_are_errors() {
        for template in [
            "{d.name",
            "{d.name:upperCase(}",
            "{d.x:unknown}",
            "{d.x[sort]}",
            "{$variable}",
            "{#variable=d.name}",
            "{d.x:showBegin}",
            "{d.x:print(.other)}",
            "{d.x[i]}",
        ] {
            assert!(
                render_template(template, &json!({}), &RenderOptions::default()).is_err(),
                "{template}"
            );
        }
    }
}
