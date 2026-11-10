use carbone::{RenderOptions, compile_template, render_template};
use serde_json::{Value, json};

#[test]
fn matches_node_reference_corpus() {
    let mut failures = Vec::new();
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).unwrap()
    {
        let entry = entry.unwrap();
        if entry
            .path()
            .extension()
            .is_none_or(|extension| extension != "json")
        {
            continue;
        }
        let corpus: Value =
            serde_json::from_str(&std::fs::read_to_string(entry.path()).unwrap()).unwrap();
        for case in corpus["cases"].as_array().unwrap() {
            let id = case["id"].as_str().unwrap();
            if case["unsupported"] == true {
                if !matches!(
                    compile_template(case["template"].as_str().unwrap()),
                    Err(carbone::Error::Unsupported(_))
                ) {
                    failures.push(format!("{id}: expected explicit rejection"));
                }
                continue;
            }
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
            if let Some(enums) = raw_options.get("enum") {
                options.enums = enums.clone();
            }
            if let Some(extension) = raw_options["extension"].as_str() {
                options.extension = extension.into();
            }
            let expected = case["expected"].as_str().unwrap();
            match compile_template(case["template"].as_str().unwrap()) {
                Ok(template) => {
                    for iteration in 0..2 {
                        match template.render(&case["data"], &options) {
                            Ok(actual) if actual == expected => {}
                            result => failures
                                .push(format!("{id}: compiled render {iteration}: {result:?}")),
                        }
                    }
                }
                Err(error) => failures.push(format!("{id}: compile failed: {error}")),
            }
            match render_template(case["template"].as_str().unwrap(), &case["data"], &options) {
                Ok(actual) if actual == expected => {}
                Ok(actual) => failures.push(format!(
                    "{id} ({}; {}):\n  expected: {expected:?}\n    actual: {actual:?}",
                    case["feature"], case["source"]
                )),
                Err(error) => failures.push(format!("{id}: unexpected error: {error}")),
            }
        }
    }
    assert!(
        failures.is_empty(),
        "Node parity failures:\n{}",
        failures.join("\n")
    );
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

fn sample_data() -> Value {
    json!({"title": "Cars", "cars": [{"brand": "Tesla", "sort": 2}, {"brand": "Toyota", "sort": 1}]})
}

#[test]
fn matches_previously_unsupported_counter_outputs() {
    let corpus: Value =
        serde_json::from_str(include_str!("formatter-fixtures/b6-extras.json")).unwrap();
    for case in corpus["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["kind"] == "render")
    {
        let template = compile_template(case["template"].as_str().unwrap()).unwrap();
        for _ in 0..2 {
            assert_eq!(
                template
                    .render(&case["data"], &RenderOptions::default())
                    .unwrap(),
                case["expected"].as_str().unwrap(),
                "{}",
                case["id"]
            );
        }
    }
    for template in ["{d.name:count}"] {
        assert!(matches!(
            compile_template(template),
            Err(carbone::Error::Unsupported(_))
        ));
    }
}

#[test]
fn rejects_null_dynamic_counter_arguments() {
    let template =
        compile_template("<r><p>{d[i].name:count(.start)}</p><p>{d[i+1].name}</p></r>").unwrap();
    for start in [serde_json::json!(null)] {
        assert!(matches!(
            template.render(
                &serde_json::json!([{ "name": "A", "start": start }]),
                &RenderOptions::default()
            ),
            Err(carbone::Error::Unsupported(_))
        ));
    }
}

#[test]
fn dynamic_counter_state_is_render_local() {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/b6-counter-edges.json")).unwrap();
    let source = corpus["cases"][0]["template"].as_str().unwrap();
    let template = compile_template(source).unwrap();
    for case in corpus["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["template"] == source)
    {
        assert_eq!(
            template
                .render(&case["data"], &RenderOptions::default())
                .unwrap(),
            case["expected"].as_str().unwrap(),
            "{}",
            case["id"]
        );
    }
}

#[test]
fn rejects_unmodeled_counter_id_rewrites_explicitly() {
    for template in [
        "<xml>{t(Title)}<r>{d[i].name:count()}</r><r>{d[i+1].name}</r></xml>",
        "<xml><r>{d[i].name:count():showBegin}x{d[i].name:showEnd}</r><r>{d[i+1].name}</r></xml>",
        "{#gone=   }<xml>{$gone}<r>{d[i].name:count()}</r><r>{d[i+1].name}</r></xml>",
        "<xml><r>{d[i].counter:count()}</r><r>{d[i+1].counter}</r></xml>",
    ] {
        assert!(
            matches!(
                compile_template(template),
                Err(carbone::Error::Unsupported(_))
            ),
            "{template}"
        );
    }
}
