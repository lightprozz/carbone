use carbone::{RenderOptions, render_template};
use serde_json::{Value, json};

#[test]
fn matches_node_reference_corpus() {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/template-parity.json")).unwrap();
    let mut failures = Vec::new();
    for case in corpus["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let raw_options = &case["options"];
        let mut options = RenderOptions::default();
        if let Some(complement) = raw_options.get("complement") {
            options.complement = complement.clone();
        }
        if let Some(lang) = raw_options["lang"].as_str() {
            options.lang = lang.to_owned();
        }
        if let Some(translations) = raw_options.get("translations") {
            options.translations = translations.clone();
        }
        if let Some(escape_xml) = raw_options["escape_xml"].as_bool() {
            options.escape_xml = escape_xml;
        }
        let expected = case["expected"].as_str().unwrap();
        match render_template(case["template"].as_str().unwrap(), &case["data"], &options) {
            Ok(actual) if actual == expected => {}
            Ok(actual) => failures.push(format!(
                "{id} ({}; {}):\n  expected: {expected:?}\n    actual: {actual:?}",
                case["feature"], case["source"]
            )),
            Err(error) => failures.push(format!("{id}: unexpected error: {error}")),
        }
    }
    assert!(
        failures.is_empty(),
        "Node parity failures:\n{}",
        failures.join("\n")
    );
}

// The native migration intentionally rejects these Node features instead of
// silently treating their expressions as missing fields.
#[test]
fn rejects_unsupported_variables() {
    let template = "{#myVar=i=0}<xml>{d.cars[$myVar].brand}</xml>";
    assert!(render_template(template, &sample_data(), &RenderOptions::default()).is_err());
}

#[test]
fn rejects_unsupported_sort_iterators() {
    for template in [
        "<xml><row>{d.cars[sort].brand}</row><row>{d.cars[sort+1].brand}</row></xml>",
        "<xml><row>{d.cars[sort,i].brand}</row><row>{d.cars[sort+1,i+1].brand}</row></xml>",
        "<xml>{d.cars[i=d.index].brand}</xml>",
    ] {
        assert!(
            render_template(template, &sample_data(), &RenderOptions::default()).is_err(),
            "unsupported iterator was accepted: {template}"
        );
    }
}

#[test]
fn rejects_unpaired_iterators() {
    for template in [
        "<xml><row>{d.cars[i].brand}</row></xml>",
        "<xml><row>{d.cars[i+1].brand}</row></xml>",
        "{d.cars[i].brand}",
    ] {
        assert!(
            render_template(template, &sample_data(), &RenderOptions::default()).is_err(),
            "unpaired iterator was accepted: {template}"
        );
    }
}

#[test]
fn escapes_quotes_in_attributes_intentionally_unlike_node_text_substitution() {
    let data = json!({"text": "&<>\"'"});
    let template = "<xml double=\"{d.text}\" single='{d.text}'>{d.text}</xml>";
    let result = render_template(template, &data, &RenderOptions::default()).unwrap();
    assert_eq!(
        result,
        "<xml double=\"&amp;&lt;&gt;&quot;&apos;\" single='&amp;&lt;&gt;&quot;&apos;'>&amp;&lt;&gt;\"'</xml>"
    );
}

#[test]
fn rejects_unsupported_conditions() {
    for template in [
        "<xml>{d.cars[i,sort>1].brand}</xml>",
        "<xml>{d.title:ifEQ(Cars):show(yes):elseShow(no)}</xml>",
    ] {
        assert!(
            render_template(template, &sample_data(), &RenderOptions::default()).is_err(),
            "unsupported condition was accepted: {template}"
        );
    }
}

fn sample_data() -> Value {
    json!({"title": "Cars", "cars": [{"brand": "Tesla", "sort": 2}, {"brand": "Toyota", "sort": 1}]})
}
