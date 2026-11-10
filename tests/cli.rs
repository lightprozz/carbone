use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::{TempDir, tempdir};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_carbone"))
}

fn fixture(template: &str, data: &str) -> TempDir {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("template.xml"), template).unwrap();
    fs::write(directory.path().join("data.json"), data).unwrap();
    directory
}

fn render(directory: &Path) -> Command {
    let mut command = cli();
    command.current_dir(directory).args([
        "render",
        "template.xml",
        "data.json",
        "--output",
        "output.xml",
    ]);
    command
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure(output: &Output) {
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty(), "expected an error on stderr");
}

#[test]
fn renders_plain_xml_and_replaces_existing_output() {
    let directory = fixture("<root>{d.name}</root>", r#"{"name":"Ada & Bob"}"#);
    let path = directory.path().join("output.xml");
    fs::write(&path, "old output").unwrap();
    assert_success(&render(directory.path()).output().unwrap());
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "<root>Ada &amp; Bob</root>"
    );
}

#[test]
fn reads_json_from_stdin() {
    let directory = fixture("<root>{d.name}</root>", "{}");
    let mut child = cli()
        .current_dir(directory.path())
        .args(["render", "template.xml", "-", "--output", "output.xml"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"name":"Ada"}"#)
        .unwrap();
    assert_success(&child.wait_with_output().unwrap());
    assert_eq!(
        fs::read_to_string(directory.path().join("output.xml")).unwrap(),
        "<root>Ada</root>"
    );
}

#[test]
fn supports_complement_and_no_escape() {
    let directory = fixture("<root>{d.name} {c.label}</root>", r#"{"name":"A & B"}"#);
    fs::write(
        directory.path().join("complement.json"),
        r#"{"label":"<extra/>"}"#,
    )
    .unwrap();
    assert_success(
        &render(directory.path())
            .args(["--complement", "complement.json", "--no-escape"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("output.xml")).unwrap(),
        "<root>A & B <extra/></root>"
    );
}

#[test]
fn malformed_json_preserves_output() {
    let directory = fixture("<root>{d.name}</root>", "{invalid");
    let path = directory.path().join("output.xml");
    fs::write(&path, "keep me").unwrap();
    let output = render(directory.path()).output().unwrap();
    assert_failure(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid JSON"));
    assert_eq!(fs::read_to_string(path).unwrap(), "keep me");
}

#[test]
fn unknown_formatter_preserves_output() {
    let directory = fixture(
        "<root>{d.name:nonexistentFormatter}</root>",
        r#"{"name":"Ada"}"#,
    );
    let path = directory.path().join("output.xml");
    fs::write(&path, "keep me").unwrap();
    let output = render(directory.path()).output().unwrap();
    assert_failure(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("nonexistentFormatter"));
    assert_eq!(fs::read_to_string(path).unwrap(), "keep me");
}

#[test]
fn missing_input_files_do_not_create_output() {
    for missing in ["template.xml", "data.json"] {
        let directory = fixture("<root>{d.name}</root>", "{}");
        fs::remove_file(directory.path().join(missing)).unwrap();
        assert_failure(&render(directory.path()).output().unwrap());
        assert!(!directory.path().join("output.xml").exists());
    }
}

#[test]
fn malformed_optional_json_preserves_output() {
    for flag in ["--complement", "--translations"] {
        let directory = fixture("<root/>", "{}");
        fs::write(directory.path().join("bad.json"), "{invalid").unwrap();
        let path = directory.path().join("output.xml");
        fs::write(&path, "keep me").unwrap();
        assert_failure(
            &render(directory.path())
                .args([flag, "bad.json"])
                .output()
                .unwrap(),
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "keep me");
    }
}

#[test]
fn invalid_conversion_preserves_output() {
    let directory = fixture("<root/>", "{}");
    let path = directory.path().join("output.xml");
    fs::write(&path, "keep me").unwrap();
    assert_failure(
        &render(directory.path())
            .args(["--convert-to", "not-a-document-format"])
            .output()
            .unwrap(),
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "keep me");
}

#[test]
fn requires_template_data_and_output() {
    for args in [
        vec!["render"],
        vec!["render", "template.xml"],
        vec!["render", "template.xml", "data.json"],
    ] {
        assert_failure(&cli().args(args).output().unwrap());
    }
}

#[test]
fn exposes_help_version_and_only_native_commands() {
    let help = cli().arg("--help").output().unwrap();
    assert_success(&help);
    assert!(String::from_utf8_lossy(&help.stdout).contains("render"));
    let version = cli().arg("--version").output().unwrap();
    assert_success(&version);
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "carbone 0.1.0"
    );
    for legacy_command in ["find", "translate"] {
        assert_failure(&cli().arg(legacy_command).output().unwrap());
    }
}
