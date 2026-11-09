use serde_json::{Number, Value};
use unicode_normalization::UnicodeNormalization;

const MAX_PAD_LENGTH: usize = 10_000;

pub(crate) fn display(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => number_text(value.as_f64().unwrap_or(f64::NAN)),
        Value::String(value) => value.clone(),
        Value::Array(values) => values.iter().map(display).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

fn js_string(value: &Value) -> String {
    if value.is_null() {
        "null".into()
    } else {
        display(value)
    }
}

fn number_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        }
        .into();
    }
    if value == 0.0 {
        return "0".into();
    }
    if value.abs() >= 1e21 || value.abs() < 1e-6 {
        let scientific = format!("{value:e}");
        let (mantissa, exponent) = scientific.split_once('e').unwrap();
        let exponent: i32 = exponent.parse().unwrap();
        return format!("{mantissa}e{exponent:+}");
    }
    value.to_string()
}

fn template(message: impl Into<String>) -> crate::Error {
    crate::Error::Template(message.into())
}

fn finite(value: f64, name: &str) -> crate::Result<Value> {
    if value.is_finite() {
        // Keep integral results integral for downstream JSON consumers.
        if value == 0.0 {
            return Ok(Value::from(0));
        }
        if value.fract() == 0.0 && value >= i64::MIN as f64 && value < i64::MAX as f64 {
            return Ok(Value::from(value as i64));
        }
        if let Some(value) = Number::from_f64(value) {
            return Ok(Value::Number(value));
        }
    }
    Err(template(format!(
        "Formatter {name} produced a non-finite number, which JSON cannot represent"
    )))
}

fn whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn numeric_argument(value: Option<&String>) -> f64 {
    let Some(value) = value else { return f64::NAN };
    let value = value.trim_matches(whitespace);
    if value.is_empty() {
        return 0.0;
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(digits) = value.strip_prefix(prefix) {
            if digits.is_empty() {
                return f64::NAN;
            }
            return digits
                .chars()
                .try_fold(0.0, |n, c| {
                    c.to_digit(radix).map(|d| n * radix as f64 + d as f64)
                })
                .unwrap_or(f64::NAN);
        }
    }
    match value {
        "Infinity" | "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ if value.contains("inf") || value.contains("Inf") => f64::NAN,
        _ => value.parse().unwrap_or(f64::NAN),
    }
}

fn parse_prefix(value: &str, integer: bool) -> f64 {
    let value = value.trim_start_matches(whitespace);
    let bytes = value.as_bytes();
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    if !integer && value[end..].starts_with("Infinity") {
        return if bytes.first() == Some(&b'-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let start = end;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    let mut digits = end - start;
    if !integer && bytes.get(end) == Some(&b'.') {
        end += 1;
        let start = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        digits += end - start;
    }
    if digits == 0 {
        return f64::NAN;
    }
    if !integer && matches!(bytes.get(end), Some(b'e' | b'E')) {
        let exponent = end;
        end += 1;
        if matches!(bytes.get(end), Some(b'+' | b'-')) {
            end += 1;
        }
        let start = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == start {
            end = exponent;
        }
    }
    value[..end].parse().unwrap_or(f64::NAN)
}

fn utf16_string(units: &[u16]) -> crate::Result<Value> {
    String::from_utf16(units).map(Value::String).map_err(|_| {
        template("Formatter split a UTF-16 surrogate pair; JSON strings cannot represent lone surrogates")
    })
}

fn slice_index(arg: Option<&String>, length: usize, default: usize) -> usize {
    let Some(_) = arg else { return default };
    let n = numeric_argument(arg);
    if n.is_nan() {
        return 0;
    }
    let n = n.trunc();
    if n < 0.0 {
        (length as f64 + n).max(0.0) as usize
    } else {
        n.min(length as f64) as usize
    }
}

fn capitalize(value: &str, words: bool) -> String {
    let mut start = true;
    let mut result = String::with_capacity(value.len());
    for c in value.chars() {
        if start && c.len_utf16() == 1 {
            result.extend(c.to_uppercase());
        } else {
            result.push(c);
        }
        start = words && whitespace(c);
    }
    result
}

fn array_index(key: &str) -> Option<u32> {
    let index: u32 = key.parse().ok()?;
    (index != u32::MAX && index.to_string() == key).then_some(index)
}

fn property(value: &Value, key: &str) -> crate::Result<String> {
    Ok(match value {
        Value::Null => return Err(template("arrayMap cannot read attributes of null")),
        Value::Object(object) => object.get(key).map(display).unwrap_or_default(),
        Value::Array(array) if key == "length" => array.len().to_string(),
        Value::Array(array) => array_index(key)
            .and_then(|i| array.get(i as usize))
            .map(display)
            .unwrap_or_default(),
        Value::String(text) if key == "length" => text.encode_utf16().count().to_string(),
        Value::String(text) => {
            if let Some(unit) = array_index(key).and_then(|i| text.encode_utf16().nth(i as usize)) {
                display(&utf16_string(&[unit])?)
            } else {
                String::new()
            }
        }
        _ => String::new(),
    })
}

fn array_map(value: Value, args: &[String]) -> crate::Result<Value> {
    let Value::Array(values) = value else {
        return Ok(value);
    };
    let separator = args.first().map(String::as_str).unwrap_or(", ");
    let attribute_separator = args.get(1).map(String::as_str).unwrap_or(":");
    let mut rows = Vec::with_capacity(values.len());
    for value in values {
        let attributes = if args.len() > 2 {
            args[2..]
                .iter()
                .map(|key| property(&value, key))
                .collect::<crate::Result<Vec<_>>>()?
        } else {
            match &value {
                Value::Object(object) => {
                    let mut entries: Vec<_> = object.iter().collect();
                    // JavaScript enumerates integer keys before insertion-ordered string keys.
                    entries.sort_by_key(|(key, _)| array_index(key).map_or((1, 0), |i| (0, i)));
                    entries
                        .into_iter()
                        .filter(|(_, v)| !v.is_object() && !v.is_array())
                        .map(|(_, v)| display(v))
                        .collect()
                }
                Value::Array(array) => array
                    .iter()
                    .filter(|v| !v.is_object() && !v.is_array())
                    .map(display)
                    .collect(),
                _ => vec![display(&value)],
            }
        };
        rows.push(attributes.join(attribute_separator));
    }
    Ok(Value::String(rows.join(separator)))
}

fn fixed(value: f64, precision: usize) -> String {
    if !value.is_finite() || value.abs() >= 1e21 {
        return number_text(value);
    }
    let abs = value.abs();
    let mut result = format!("{abs:.precision$}").into_bytes();
    let bits = abs.to_bits();
    let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
    let mantissa = (bits & ((1_u64 << 52) - 1)) | if exponent_bits == 0 { 0 } else { 1_u64 << 52 };
    let exponent = if exponent_bits == 0 {
        -1074
    } else {
        exponent_bits - 1075
    };
    // Rust uses ties-to-even; JavaScript toFixed chooses the larger magnitude at exact ties.
    if mantissa != 0
        && exponent + precision as i32 + mantissa.trailing_zeros() as i32 == -1
        && (mantissa >> mantissa.trailing_zeros()) % 4 == 1
    {
        let mut carry = true;
        for digit in result.iter_mut().rev() {
            if *digit == b'.' {
                continue;
            }
            if *digit == b'9' {
                *digit = b'0';
            } else {
                *digit += 1;
                carry = false;
                break;
            }
        }
        if carry {
            result.insert(0, b'1');
        }
    }
    if value < 0.0 {
        result.insert(0, b'-');
    }
    String::from_utf8(result).unwrap()
}

pub(crate) fn apply(name: &str, value: Value, args: &[String]) -> crate::Result<Value> {
    match name {
        "lowerCase" | "upperCase" | "ucFirst" | "ucWords" | "unaccent" => {
            let Value::String(text) = value else {
                return Ok(value);
            };
            Ok(Value::String(match name {
                "lowerCase" => text.to_lowercase(),
                "upperCase" => text.to_uppercase(),
                "ucFirst" => capitalize(&text, false),
                "ucWords" => capitalize(&text, true),
                _ => text
                    .nfd()
                    .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
                    .collect(),
            }))
        }
        "print" => Ok(args
            .first()
            .cloned()
            .map(Value::String)
            .unwrap_or(Value::Null)),
        "prepend" => Ok(Value::String(format!(
            "{}{}",
            args.first().map(String::as_str).unwrap_or("undefined"),
            js_string(&value)
        ))),
        "substr" | "slice" => {
            let Value::String(text) = value else {
                return Ok(value);
            };
            let units: Vec<_> = text.encode_utf16().collect();
            let begin = slice_index(args.first(), units.len(), 0);
            let end = slice_index(args.get(1), units.len(), units.len()).max(begin);
            utf16_string(&units[begin..end])
        }
        "padl" | "padr" => {
            let length = numeric_argument(args.first());
            if length > MAX_PAD_LENGTH as f64 {
                return Err(template("The target length cannot be greater than 10000"));
            }
            if !value.is_string() && !value.is_number() {
                return Ok(value);
            }
            let text = display(&value);
            let mut units: Vec<_> = text.encode_utf16().collect();
            let needed = (length.max(0.0) as usize).saturating_sub(units.len());
            let pad: Vec<_> = args
                .get(1)
                .map(String::as_str)
                .unwrap_or(" ")
                .encode_utf16()
                .collect();
            if needed == 0 || pad.is_empty() {
                return Ok(Value::String(text));
            }
            let padding: Vec<_> = pad.iter().copied().cycle().take(needed).collect();
            if name == "padl" {
                units.splice(..0, padding);
            } else {
                units.extend(padding);
            }
            utf16_string(&units)
        }
        "md5" => Ok(Value::String(format!(
            "{:x}",
            md5::compute(js_string(&value).as_bytes())
        ))),
        "arrayJoin" => {
            let Value::Array(values) = value else {
                return Ok(value);
            };
            let separator = match args.first().map(String::as_str).unwrap_or(", ") {
                "\\n" => "\n",
                "\\r\\n" => "\r\n",
                other => other,
            };
            Ok(Value::String(
                values
                    .iter()
                    .map(display)
                    .collect::<Vec<_>>()
                    .join(separator),
            ))
        }
        "arrayMap" => array_map(value, args),
        // The renderer injects the loop id and resolves this marker after repetition.
        "count" => Ok(Value::String(format!(
            "__COUNT_{}_{}__",
            args.first().map(String::as_str).unwrap_or("undefined"),
            args.get(1).map(String::as_str).unwrap_or("1")
        ))),
        "len" => Ok(Value::from(match value {
            Value::String(text) => text.encode_utf16().count(),
            Value::Array(values) => values.len(),
            _ => 0,
        })),
        "int" => finite(parse_prefix(&js_string(&value), true), name),
        "toEN" => Ok(Value::String(js_string(&value).replace(',', "."))),
        "toFR" => Ok(Value::String(js_string(&value).replace('.', ","))),
        "toFixed" => {
            let precision = numeric_argument(args.first());
            let precision = if precision.is_nan() {
                0.0
            } else {
                precision.trunc()
            };
            if !(0.0..=100.0).contains(&precision) {
                return Err(template("toFixed precision must be between 0 and 100"));
            }
            Ok(Value::String(fixed(
                parse_prefix(&js_string(&value), false),
                precision as usize,
            )))
        }
        "round" => {
            let text = js_string(&value);
            let precision = args.first().map(String::as_str).unwrap_or("undefined");
            let shifted = if let Some((mantissa, exponent)) = text.split_once('e') {
                let exponent =
                    exponent.parse::<f64>().unwrap_or(f64::NAN) + numeric_argument(args.first());
                format!("{mantissa}e{exponent:+}")
                    .parse::<f64>()
                    .unwrap_or(f64::NAN)
            } else {
                format!("{text}e+{precision}")
                    .parse::<f64>()
                    .unwrap_or(f64::NAN)
            };
            let floor = shifted.floor();
            let rounded = if shifted - floor < 0.5 {
                floor
            } else {
                floor + 1.0
            };
            let result = format!("{}e-{precision}", number_text(rounded))
                .parse()
                .unwrap_or(f64::NAN);
            finite(result, name)
        }
        "add" | "sub" | "mul" | "div" => {
            if value.is_null() {
                return Ok(value);
            }
            let operand = parse_prefix(
                args.first().map(String::as_str).unwrap_or("undefined"),
                false,
            );
            if name == "div" && operand == 0.0 {
                return Ok(value);
            }
            let number = parse_prefix(&js_string(&value), false);
            finite(
                match name {
                    "add" => number + operand,
                    "sub" => number - operand,
                    "mul" => number * operand,
                    _ => number / operand,
                },
                name,
            )
        }
        _ => Err(crate::Error::Unsupported(format!(
            "Formatter {name} is not supported by the native renderer"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(name: &str, value: Value, args: &[&str]) -> Value {
        apply(
            name,
            value,
            &args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn display_uses_javascript_coercion() {
        assert_eq!(display(&json!(null)), "");
        assert_eq!(display(&json!({"a": 1})), "[object Object]");
        assert_eq!(
            display(&json!([1, null, [2, 3], {}, true])),
            "1,,2,3,[object Object],true"
        );
        for (value, expected) in [
            (1.0, "1"),
            (-0.0, "0"),
            (1e21, "1e+21"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
        ] {
            assert_eq!(display(&json!(value)), expected);
        }
    }

    #[test]
    fn string_cases_and_incorrect_types() {
        for name in [
            "lowerCase",
            "upperCase",
            "ucFirst",
            "ucWords",
            "unaccent",
            "substr",
            "slice",
            "padl",
            "padr",
            "arrayJoin",
            "arrayMap",
        ] {
            for value in [Value::Null, json!(true), json!({"x": 1})] {
                assert_eq!(run(name, value.clone(), &[]), value, "{name}");
            }
        }
        assert_eq!(
            run("lowerCase", json!("My CAR ΣΟΣ"), &[]),
            json!("my car σος")
        );
        assert_eq!(run("upperCase", json!("Straße"), &[]), json!("STRASSE"));
        assert_eq!(run("ucFirst", json!("my cAR"), &[]), json!("My cAR"));
        assert_eq!(run("ucFirst", json!(""), &[]), json!(""));
        assert_eq!(
            run("ucWords", json!(" azerty\tss zzzeZ;foo"), &[]),
            json!(" Azerty\tSs ZzzeZ;foo")
        );
        assert_eq!(
            run("unaccent", json!("CRÈME éùïêèà\u{1ab0}"), &[]),
            json!("CREME euieea\u{1ab0}")
        );
    }

    #[test]
    fn print_prepend_and_md5() {
        assert_eq!(run("print", json!(12), &["hello"]), json!("hello"));
        assert_eq!(run("print", json!(12), &[]), Value::Null);
        assert_eq!(run("prepend", Value::Null, &["x"]), json!("xnull"));
        assert_eq!(run("prepend", json!([1, 2]), &["x"]), json!("x1,2"));
        assert_eq!(
            run("md5", json!("hello"), &[]),
            json!("5d41402abc4b2a76b9719d911017c592")
        );
        assert_eq!(
            run("md5", Value::Null, &[]),
            json!("37a6259cc0c1dae299a7866489dff0bd")
        );
    }

    #[test]
    fn slices_use_end_indices_and_utf16() {
        for name in ["substr", "slice"] {
            assert_eq!(run(name, json!("foobar"), &["2", "-1"]), json!("oba"));
            assert_eq!(run(name, json!("foobar"), &["-2"]), json!("ar"));
            assert_eq!(run(name, json!("foobar"), &["5", "1"]), json!(""));
            assert_eq!(
                run(name, json!("foobar"), &["-Infinity", "Infinity"]),
                json!("foobar")
            );
            assert_eq!(run(name, json!("foobar"), &["junk", "2"]), json!("fo"));
            assert_eq!(run(name, json!("A😀B"), &["1", "3"]), json!("😀"));
            assert!(apply(name, json!("😀"), &["1".into()]).is_err());
        }
    }

    #[test]
    fn padding_bounds_and_empty_padding() {
        assert_eq!(run("padl", json!(1223), &["8", "0"]), json!("00001223"));
        assert_eq!(run("padr", json!("abc"), &["8", "foo"]), json!("abcfoofo"));
        assert_eq!(run("padl", json!("abc"), &["8", ""]), json!("abc"));
        assert_eq!(run("padr", json!("abc"), &["-1"]), json!("abc"));
        assert_eq!(run("padr", json!("abc"), &["junk"]), json!("abc"));
        assert_eq!(run("padl", json!("😀"), &["3"]), json!(" 😀"));
        for name in ["padl", "padr"] {
            assert_eq!(
                run(name, json!("a"), &["10000"]).as_str().unwrap().len(),
                10000
            );
            for length in ["10001", "10000.1", "Infinity"] {
                assert!(apply(name, Value::Null, &[length.into()]).is_err());
            }
        }
    }

    #[test]
    fn arrays_keep_flattening_and_filter_order() {
        let data = json!([{"id": 2, "name": "car", "type": "toy", "sub": {"id": 3}, "arr": [12,23]}, {"id": 3, "name": "plane", "type": "concept"}]);
        assert_eq!(
            run("arrayMap", data.clone(), &[]),
            json!("2:car:toy, 3:plane:concept")
        );
        assert_eq!(
            run("arrayMap", data, &[" | ", ";", "name", "id", "missing"]),
            json!("car;2; | plane;3;")
        );
        assert_eq!(
            run(
                "arrayMap",
                json!([{"b": 1, "10": "ten", "2": "two", "a": 2}]),
                &[]
            ),
            json!("two:ten:1:2")
        );
        assert_eq!(
            run("arrayMap", json!([null, [1, {}, 2], "x"]), &[]),
            json!(", 1:2, x")
        );
        assert_eq!(
            run(
                "arrayMap",
                json!([{"x": [1,2], "y": {}}]),
                &[",", ":", "x", "y"]
            ),
            json!("1,2:[object Object]")
        );
        assert!(
            apply(
                "arrayMap",
                json!([null]),
                &[",".into(), ":".into(), "x".into()]
            )
            .is_err()
        );
        assert_eq!(
            run("arrayJoin", json!([1, null, {}, [2, 3]]), &[]),
            json!("1, , [object Object], 2,3")
        );
        assert_eq!(run("arrayJoin", json!([1, 2]), &["\\n"]), json!("1\n2"));
        assert_eq!(
            run("arrayJoin", json!([1, 2]), &["\\r\\n"]),
            json!("1\r\n2")
        );
        assert_eq!(run("arrayJoin", json!([]), &[]), json!(""));
    }

    #[test]
    fn count_is_a_loop_marker_and_len_counts_utf16() {
        assert_eq!(run("count", json!([1, 2]), &["7"]), json!("__COUNT_7_1__"));
        assert_eq!(
            run("count", Value::Null, &["7", "0"]),
            json!("__COUNT_7_0__")
        );
        assert_eq!(run("len", json!("😀"), &[]), json!(2));
        assert_eq!(run("len", json!([1, 2]), &[]), json!(2));
        assert_eq!(run("len", json!({"a": 1}), &[]), json!(0));
    }

    #[test]
    fn numbers_parse_prefixes_and_keep_null_and_zero_division() {
        assert_eq!(run("int", json!(" -12.9tail"), &[]), json!(-12));
        assert_eq!(run("int", json!("0xff"), &[]), json!(0));
        assert_eq!(run("int", json!(1e21), &[]), json!(1));
        for (name, expected) in [
            ("add", 187.0),
            ("sub", 53.0),
            ("mul", 8040.0),
            ("div", 120.0 / 67.0),
        ] {
            assert_eq!(
                run(name, json!("120kg"), &["67kg"]).as_f64(),
                Some(expected)
            );
            assert_eq!(run(name, Value::Null, &["2"]), Value::Null);
            assert!(apply(name, json!(true), &["2".into()]).is_err());
        }
        assert_eq!(run("div", json!("120"), &["0kg"]), json!("120"));
        assert_eq!(run("add", json!("1.2e2tail"), &[".5"]), json!(120.5));
        assert_eq!(run("add", json!("1e+bad"), &["2"]), json!(3));
        assert!(apply("int", json!({}), &[]).is_err());
        assert!(apply("mul", json!(1e308), &["10".into()]).is_err());
        assert_eq!(run("toEN", json!("1,2,3"), &[]), json!("1.2.3"));
        assert_eq!(run("toFR", json!("1.2.3"), &[]), json!("1,2,3"));
    }

    #[test]
    fn fixed_precision_matches_binary_rounding_and_range() {
        for (value, precision, expected) in [
            (2.5, "0", "3"),
            (3.5, "0", "4"),
            (1.25, "1", "1.3"),
            (-1.25, "1", "-1.3"),
            (1.005, "2", "1.00"),
            (-0.0, "2", "0.00"),
            (-0.001, "2", "-0.00"),
            (1e21, "2", "1e+21"),
        ] {
            assert_eq!(run("toFixed", json!(value), &[precision]), json!(expected));
        }
        assert_eq!(run("toFixed", json!("bad"), &["2"]), json!("NaN"));
        assert_eq!(run("toFixed", json!("Infinity"), &["2"]), json!("Infinity"));
        assert_eq!(
            run("toFixed", json!(1), &["100"]).as_str().unwrap().len(),
            102
        );
        assert!(apply("toFixed", json!(1), &["101".into()]).is_err());
        assert!(apply("toFixed", json!(1), &["-1".into()]).is_err());
    }

    #[test]
    fn round_uses_decimal_shift_and_javascript_negative_ties() {
        for (number, precision, expected) in [
            (222.1512, "2", 222.15),
            (222.1552, "2", 222.16),
            (1.005, "2", 1.01),
            (1.005, "3", 1.005),
            (-1.005, "3", -1.005),
            (-1.006, "2", -1.01),
            (-1.005, "2", -1.0),
            (1.05, "1", 1.1),
            (1e-7, "8", 1e-7),
        ] {
            assert_eq!(
                run("round", json!(number), &[precision]).as_f64(),
                Some(expected)
            );
        }
        assert!(apply("round", json!(1), &[]).is_err());
        assert!(apply("round", json!(1), &["-1".into()]).is_err());
    }

    #[test]
    fn unsupported_names_do_not_silently_pass_through() {
        for name in [
            "mod", "unknown", "ifEQ", "show", "formatN", "formatD", "convEnum", "convCRLF",
        ] {
            assert!(
                matches!(
                    apply(name, json!(1), &[]),
                    Err(crate::Error::Unsupported(_))
                ),
                "{name}"
            );
        }
    }
}
