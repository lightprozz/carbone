//! In-memory Office packaging and optional, native LibreOffice conversion.
//!
//! Spreadsheet archives are copied only when they contain no Carbone markers;
//! spreadsheet preprocessing is not implemented. Embedded and signed documents
//! are rejected rather than
//! silently leaving nested templates or invalid signatures in the result.

use crate::{Error, RenderOptions, Result, render_template};
use serde_json::Value;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const MAX_DOCUMENT_SIZE: u64 = 200 * 1024 * 1024;
const MAX_MEMBER_SIZE: u64 = 64 * 1024 * 1024;
const MAX_MEMBERS: usize = 10_000;
const CONVERSION_TIMEOUT: Duration = Duration::from_secs(60);

/// Render a UTF-8 template or an OpenDocument/Office Open XML archive in memory.
///
/// Archive paths, member counts, CRCs and expanded sizes are checked. No archive
/// member is extracted to disk. Unchanged parts retain their compressed bytes;
/// changed XML retains compression, timestamps and Unix permissions. ODF's
/// `mimetype` is always emitted first, uncompressed.
pub fn render_bytes(template: &[u8], data: &Value, options: &RenderOptions) -> Result<Vec<u8>> {
    check_size(template.len() as u64, MAX_DOCUMENT_SIZE, "template")?;
    if !template.starts_with(b"PK") {
        let template = String::from_utf8(template.to_vec())?;
        let rendered = render_template(&template, data, options)?.into_bytes();
        check_size(
            rendered.len() as u64,
            MAX_DOCUMENT_SIZE,
            "rendered document",
        )?;
        return Ok(rendered);
    }

    let mut archive = ZipArchive::new(Cursor::new(template))?;
    if archive.len() > MAX_MEMBERS {
        return Err(Error::Template("ZIP has too many members".into()));
    }
    let mut names = HashSet::new();
    let mut total = 0_u64;
    let mut mimetype_index = None;
    for index in 0..archive.len() {
        let file = archive.by_index(index)?;
        let name = file.name();
        validate_member_name(name)?;
        if !names.insert(name.to_owned()) {
            return Err(Error::Template(format!("duplicate ZIP member: {name}")));
        }
        if file.is_symlink()
            || file
                .unix_mode()
                .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o100000 | 0o040000))
        {
            return Err(Error::Unsupported(format!(
                "non-regular ZIP member: {name}"
            )));
        }
        if file.encrypted() {
            return Err(Error::Unsupported("encrypted ZIP members".into()));
        }
        if !matches!(
            file.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(Error::Unsupported(format!("ZIP compression for {name}")));
        }
        check_size(file.size(), MAX_MEMBER_SIZE, "ZIP member")?;
        total = total
            .checked_add(file.size())
            .ok_or_else(|| Error::Template("ZIP size overflow".into()))?;
        check_size(total, MAX_DOCUMENT_SIZE, "expanded ZIP")?;
        reject_unsupported_member(name, file.is_dir())?;
        if name == "mimetype" {
            mimetype_index = Some(index);
        }
    }

    let mut is_spreadsheet = names.contains("xl/workbook.xml");
    let is_odf = if let Some(index) = mimetype_index {
        let mut file = archive.by_index(index)?;
        let bytes = read_limited(&mut file, MAX_MEMBER_SIZE, "mimetype")?;
        is_spreadsheet |= bytes.starts_with(b"application/vnd.oasis.opendocument.spreadsheet");
        bytes.starts_with(b"application/vnd.oasis.opendocument.") && names.contains("content.xml")
    } else {
        false
    };
    let is_ooxml = names.contains("[Content_Types].xml")
        && [
            "word/document.xml",
            "xl/workbook.xml",
            "ppt/presentation.xml",
        ]
        .iter()
        .any(|name| names.contains(*name));
    if !is_odf && !is_ooxml {
        return Err(Error::Unsupported(
            "ZIP is not a recognized Office document".into(),
        ));
    }

    let mut order: Vec<_> = (0..archive.len()).collect();
    if is_odf {
        let index = mimetype_index.expect("ODF has a mimetype member");
        order.retain(|&entry| entry != index);
        order.insert(0, index);
    }
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer.set_raw_comment(archive.comment().into());
    let mut rendered_total = 0_u64;
    let mut expanded_total = 0_u64;
    for index in order {
        let mut file = archive.by_index(index)?;
        let name = file.name().to_owned();
        let mut file_options = file.options();
        // Read even untouched members to verify CRC and actual expanded size.
        let remaining = MAX_DOCUMENT_SIZE - expanded_total;
        let original = read_limited(
            &mut file,
            MAX_MEMBER_SIZE.min(remaining),
            "expanded ZIP member",
        )?;
        expanded_total += original.len() as u64;
        if original.len() as u64 != file.size() {
            return Err(Error::Template(format!("ZIP size mismatch: {name}")));
        }
        let rendered = if name.ends_with(".xml") || name.ends_with(".rels") {
            let xml = String::from_utf8(original.clone())?;
            if is_spreadsheet {
                if contains_carbone_marker(&xml) {
                    return Err(Error::Unsupported(format!(
                        "spreadsheet templates require preprocessing that is not implemented (Carbone marker in {name}); only static XLSX/ODS archives are supported"
                    )));
                }
                original.clone()
            } else {
                render_template(&xml, data, options)?.into_bytes()
            }
        } else {
            original.clone()
        };
        check_size(
            rendered.len() as u64,
            MAX_MEMBER_SIZE,
            "rendered ZIP member",
        )?;
        rendered_total += rendered.len() as u64;
        check_size(rendered_total, MAX_DOCUMENT_SIZE, "rendered ZIP")?;
        let rewrite_mimetype = is_odf && name == "mimetype";
        if rendered == original && !rewrite_mimetype {
            drop(file);
            writer.raw_copy_file(archive.by_index(index)?)?;
        } else {
            if rewrite_mimetype {
                file_options = file_options.compression_method(CompressionMethod::Stored);
            }
            writer.start_file(name, file_options)?;
            writer.write_all(&rendered)?;
        }
    }
    let bytes = writer.finish()?.into_inner();
    check_size(bytes.len() as u64, MAX_DOCUMENT_SIZE, "packaged document")?;
    Ok(bytes)
}

/// Read and render a template, without requiring its extension to identify ZIPs.
pub fn render_file(
    template: impl AsRef<Path>,
    data: &Value,
    options: &RenderOptions,
) -> Result<Vec<u8>> {
    let mut file = File::open(template)?;
    let bytes = read_limited(&mut file, MAX_DOCUMENT_SIZE, "template")?;
    render_bytes(&bytes, data, options)
}

fn check_size(size: u64, limit: u64, what: &str) -> Result<()> {
    if size > limit {
        return Err(Error::Template(format!(
            "{what} exceeds the {limit}-byte limit"
        )));
    }
    Ok(())
}

fn read_limited(reader: &mut impl Read, limit: u64, what: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    check_size(bytes.len() as u64, limit, what)?;
    Ok(bytes)
}

fn validate_member_name(name: &str) -> Result<()> {
    let path = name.strip_suffix('/').unwrap_or(name);
    if path.is_empty()
        || path.contains(['\\', ':', '\0'])
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(Error::Template(format!("unsafe ZIP member path: {name:?}")));
    }
    Ok(())
}

fn contains_carbone_marker(xml: &str) -> bool {
    // Formatting runs may split the opening brace, root and punctuation.
    let mut compact = String::with_capacity(xml.len());
    let mut in_tag = false;
    let mut quote = None;
    for character in xml.chars() {
        if in_tag {
            if let Some(delimiter) = quote {
                if character == delimiter {
                    quote = None;
                }
            } else if matches!(character, '\'' | '"') {
                quote = Some(character);
            } else if character == '>' {
                in_tag = false;
            }
        } else if character == '<' {
            in_tag = true;
        } else if !character.is_whitespace() {
            compact.push(character);
        }
    }
    // Check attributes too, before removing XML tags.
    [xml, compact.as_str()].into_iter().any(|text| {
        text.split('{').skip(1).any(|body| {
            let body = body.trim_start();
            [
                "d.",
                "d[",
                "d:",
                "d(",
                "c.",
                "c[",
                "c:",
                "c(",
                "t.",
                "t[",
                "t:",
                "t(",
                "$",
                "#",
                "bindColor",
            ]
            .iter()
            .any(|prefix| body.starts_with(prefix))
        })
    })
}

fn reject_unsupported_member(name: &str, is_directory: bool) -> Result<()> {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("_xmlsignatures/")
        || matches!(
            lower.as_str(),
            "meta-inf/documentsignatures.xml" | "meta-inf/macrosignatures.xml"
        )
    {
        return Err(Error::Unsupported(
            "digitally signed Office documents".into(),
        ));
    }
    let first_part = lower.split('/').next().unwrap_or_default();
    let odf_object = first_part.strip_prefix("object").is_some_and(|suffix| {
        let number = suffix.trim_start();
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
    });
    if !is_directory
        && (odf_object
            || lower.split('/').any(|part| part == "embeddings")
            || lower.ends_with("/content.xml")
            || [
                ".docx", ".xlsx", ".pptx", ".odt", ".ods", ".odp", ".odg", ".doc", ".xls", ".ppt",
                ".zip", ".docm", ".xlsm", ".pptm", ".xlsb", ".rtf",
            ]
            .iter()
            .any(|suffix| lower.ends_with(suffix)))
    {
        return Err(Error::Unsupported(format!("embedded document: {name}")));
    }
    Ok(())
}

/// Convert a document with `libreoffice` (or `soffice`) from PATH.
///
/// Every call uses a private temporary directory and user profile, never a shell,
/// and a 60-second timeout. Timeout cleanup kills and reaps the direct child only;
/// descendant process cleanup is not guaranteed. There is no worker pool or retry
/// parity with the Node converter. LibreOffice must be installed separately. Extensions
/// may have one leading dot; filter expressions and paths are not accepted.
/// This isolates application state, not LibreOffice's OS permissions: untrusted
/// documents should be converted inside an appropriately restricted sandbox.
pub fn convert(document: &[u8], input_extension: &str, output_extension: &str) -> Result<Vec<u8>> {
    convert_with_programs(
        document,
        input_extension,
        output_extension,
        &[Path::new("libreoffice"), Path::new("soffice")],
        CONVERSION_TIMEOUT,
    )
}

fn extension(extension: &str) -> Result<String> {
    let extension = extension.strip_prefix('.').unwrap_or(extension);
    if extension.is_empty()
        || extension.len() > 12
        || !extension.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err(Error::Conversion(
            "extensions must contain only 1–12 ASCII letters or digits".into(),
        ));
    }
    Ok(extension.to_ascii_lowercase())
}

fn convert_with_programs(
    document: &[u8],
    input_extension: &str,
    output_extension: &str,
    programs: &[&Path],
    timeout: Duration,
) -> Result<Vec<u8>> {
    let input_extension = extension(input_extension)?;
    let output_extension = extension(output_extension)?;
    if document.is_empty() || document.len() as u64 > MAX_DOCUMENT_SIZE {
        return Err(Error::Conversion(
            "input must contain 1–209715200 bytes".into(),
        ));
    }
    let temporary = tempfile::Builder::new()
        .prefix("carbone-convert-")
        .tempdir()?;
    let input = temporary.path().join(format!("document.{input_extension}"));
    let output_directory = temporary.path().join("output");
    let profile = temporary.path().join("profile");
    fs::create_dir(&output_directory)?;
    fs::create_dir_all(profile.join("user"))?;
    // Never execute document macros, including in a newly created profile.
    fs::write(
        profile.join("user/registrymodifications.xcu"),
        r#"<?xml version="1.0" encoding="UTF-8"?><oor:items xmlns:oor="http://openoffice.org/2001/registry"><item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop></item></oor:items>"#,
    )?;
    fs::write(&input, document)?;
    let profile_url = url::Url::from_directory_path(&profile)
        .map_err(|()| Error::Conversion("cannot construct LibreOffice profile URL".into()))?;
    let mut child = None;
    for program in programs {
        match Command::new(program)
            .arg(format!("-env:UserInstallation={profile_url}"))
            .args([
                "--headless",
                "--nologo",
                "--nodefault",
                "--nofirststartwizard",
                "--norestore",
                "--convert-to",
            ])
            .arg(&output_extension)
            .arg("--outdir")
            .arg(&output_directory)
            .arg(&input)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(process) => {
                child = Some(process);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(Error::Conversion(format!(
                    "cannot start LibreOffice: {error}"
                )));
            }
        }
    }
    let mut child = child.ok_or_else(|| Error::Conversion("LibreOffice is not installed or is not available on PATH (tried libreoffice and soffice)".into()))?;
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        result => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Conversion(match result {
                Ok(None) => format!(
                    "LibreOffice timed out after {} seconds",
                    timeout.as_secs_f64()
                ),
                Err(error) => format!("waiting for LibreOffice failed: {error}"),
                Ok(Some(_)) => unreachable!(),
            }));
        }
    };
    if !status.success() {
        return Err(Error::Conversion(format!(
            "LibreOffice exited with {status}"
        )));
    }
    let output = output_directory.join(format!("document.{output_extension}"));
    let metadata = fs::symlink_metadata(&output).map_err(|error| {
        Error::Conversion(format!("LibreOffice produced no expected output: {error}"))
    })?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > MAX_DOCUMENT_SIZE
    {
        return Err(Error::Conversion(
            "LibreOffice output is empty, not a regular file, or too large".into(),
        ));
    }
    let bytes = read_limited(
        &mut File::open(output)?,
        MAX_DOCUMENT_SIZE,
        "converted document",
    )?;
    if bytes.is_empty() {
        return Err(Error::Conversion("LibreOffice output is empty".into()));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zip::write::SimpleFileOptions;

    fn package(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.set_comment("preserved archive comment");
        for (name, bytes) in parts {
            zip.start_file(
                *name,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .unix_permissions(0o640),
            )
            .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn part(bytes: &[u8], name: &str) -> Vec<u8> {
        let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut output = Vec::new();
        zip.by_name(name).unwrap().read_to_end(&mut output).unwrap();
        output
    }

    #[test]
    fn plain_xml_and_file() {
        let template = b"<p>{d.name}</p>";
        let data = json!({"name": "Ada & Grace"});
        let expected = b"<p>Ada &amp; Grace</p>";
        assert_eq!(
            render_bytes(template, &data, &RenderOptions::default()).unwrap(),
            expected
        );
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(template).unwrap();
        assert_eq!(
            render_file(file.path(), &data, &RenderOptions::default()).unwrap(),
            expected
        );
        assert!(matches!(
            render_bytes(&[0xff], &data, &RenderOptions::default()),
            Err(Error::Utf8(_))
        ));
    }

    #[test]
    fn renders_existing_odt_and_preserves_thumbnail() {
        let input = include_bytes!("../examples/simple.odt");
        let output = render_bytes(
            input,
            &json!({"firstname": "Ada", "lastname": "Lovelace"}),
            &RenderOptions::default(),
        )
        .unwrap();
        let content = String::from_utf8(part(&output, "content.xml")).unwrap();
        assert!(content.contains("Ada") && content.contains("Lovelace"));
        assert!(!content.contains("{d."));
        assert_eq!(
            part(input, "Thumbnails/thumbnail.png"),
            part(&output, "Thumbnails/thumbnail.png")
        );
        assert_eq!(
            part(input, "META-INF/manifest.xml"),
            part(&output, "META-INF/manifest.xml")
        );
        let mut zip = ZipArchive::new(Cursor::new(output)).unwrap();
        let first = zip.by_index(0).unwrap();
        assert_eq!(first.name(), "mimetype");
        assert_eq!(first.compression(), CompressionMethod::Stored);
    }

    #[test]
    fn renders_existing_docx_and_complement() {
        let input = include_bytes!("../test/datasets/test_word_render_A.docx");
        let options = RenderOptions {
            complement: json!({"author1": "Grace"}),
            ..RenderOptions::default()
        };
        let output = render_bytes(
            input,
            &json!({"field1": "Ada", "field2": "Lovelace"}),
            &options,
        )
        .unwrap();
        let content = String::from_utf8(part(&output, "word/document.xml")).unwrap();
        for text in ["Ada", "Lovelace", "Grace"] {
            assert!(content.contains(text));
        }
        assert_eq!(
            part(input, "docProps/thumbnail.jpeg"),
            part(&output, "docProps/thumbnail.jpeg")
        );
    }

    #[test]
    fn renders_all_xml_parts_and_preserves_assets_and_metadata() {
        let input = package(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("word/document.xml", b"<p>{d.name}</p>"),
            ("word/header1.xml", b"<p>{d.name}</p>"),
            ("word/footer1.xml", b"<p>{d.name}</p>"),
            ("word/_rels/document.xml.rels", b"<r value='{d.name}'/>"),
            ("word/media/image.png", b"\0\xff{d.name}"),
        ]);
        let output =
            render_bytes(&input, &json!({"name": "Ada"}), &RenderOptions::default()).unwrap();
        for name in [
            "word/document.xml",
            "word/header1.xml",
            "word/footer1.xml",
            "word/_rels/document.xml.rels",
        ] {
            assert!(
                String::from_utf8(part(&output, name))
                    .unwrap()
                    .contains("Ada")
            );
        }
        assert_eq!(
            part(&input, "word/media/image.png"),
            part(&output, "word/media/image.png")
        );
        let mut old = ZipArchive::new(Cursor::new(input)).unwrap();
        let mut new = ZipArchive::new(Cursor::new(output)).unwrap();
        assert_eq!(new.comment(), old.comment());
        for name in ["word/document.xml", "word/media/image.png"] {
            let before = old.by_name(name).unwrap();
            let after = new.by_name(name).unwrap();
            assert_eq!(before.compression(), after.compression());
            assert_eq!(before.unix_mode(), after.unix_mode());
            assert_eq!(before.last_modified(), after.last_modified());
        }
    }

    #[test]
    fn moves_compressed_odf_mimetype_to_first_member() {
        let input = package(&[
            ("content.xml", b"<p>{d.name}</p>"),
            ("mimetype", b"application/vnd.oasis.opendocument.text"),
        ]);
        let output =
            render_bytes(&input, &json!({"name":"Ada"}), &RenderOptions::default()).unwrap();
        let mut zip = ZipArchive::new(Cursor::new(output)).unwrap();
        let first = zip.by_index(0).unwrap();
        assert_eq!(first.name(), "mimetype");
        assert_eq!(first.compression(), CompressionMethod::Stored);
        assert!(first.extra_data().is_none_or(|bytes| bytes.is_empty()));
    }

    #[test]
    fn rejects_unsafe_names_and_embedded_documents() {
        for name in [
            "../escape",
            "/absolute",
            "word/../../escape",
            "C:/escape",
            "word\\escape",
            "word//escape",
            "word/./escape",
        ] {
            let input = package(&[(name, b"x")]);
            assert!(
                matches!(
                    render_bytes(&input, &Value::Null, &RenderOptions::default()),
                    Err(Error::Template(_))
                ),
                "{name}"
            );
        }
        let input = include_bytes!("../test/datasets/test_word_with_embedded_excel.docx");
        assert!(
            matches!(render_bytes(input, &Value::Null, &RenderOptions::default()), Err(Error::Unsupported(message)) if message.contains("embedded"))
        );
        for name in [
            "Object 1/content.xml",
            "Object 2",
            "word/embeddings/oleObject1.bin",
            "_xmlsignatures/sig1.xml",
        ] {
            assert!(matches!(
                render_bytes(
                    &package(&[(name, b"x")]),
                    &Value::Null,
                    &RenderOptions::default()
                ),
                Err(Error::Unsupported(_))
            ));
        }
    }

    #[test]
    fn rejects_symlinks_unknown_archives_and_corruption() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.add_symlink("link", "../target", SimpleFileOptions::default())
            .unwrap();
        assert!(matches!(
            render_bytes(
                &zip.finish().unwrap().into_inner(),
                &Value::Null,
                &RenderOptions::default()
            ),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            render_bytes(
                &package(&[("other.txt", b"x")]),
                &Value::Null,
                &RenderOptions::default()
            ),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            render_bytes(b"PKbroken", &Value::Null, &RenderOptions::default()),
            Err(Error::Zip(_))
        ));
        let input =
            include_bytes!("../test/datasets/zip-failure/too_many_length_or_distance_symbols.zip");
        assert!(render_bytes(input, &Value::Null, &RenderOptions::default()).is_err());
    }

    #[test]
    fn checks_crc_even_for_unchanged_members() {
        let mut input = package(&[
            ("content.xml", b"<p/>"),
            ("mimetype", b"application/vnd.oasis.opendocument.text"),
        ]);
        let central = input
            .windows(4)
            .position(|bytes| bytes == b"PK\x01\x02")
            .unwrap();
        input[central + 16] ^= 1;
        assert!(render_bytes(&input, &Value::Null, &RenderOptions::default()).is_err());
    }

    #[test]
    fn bounds_expansion_and_member_count() {
        assert!(check_size(MAX_DOCUMENT_SIZE + 1, MAX_DOCUMENT_SIZE, "ZIP").is_err());
        assert!(read_limited(&mut Cursor::new(b"12345"), 4, "test").is_err());
        let mut input = package(&[
            ("content.xml", b"<p/>"),
            ("mimetype", b"application/vnd.oasis.opendocument.text"),
        ]);
        let central = input
            .windows(4)
            .position(|bytes| bytes == b"PK\x01\x02")
            .unwrap();
        input[central + 24..central + 28]
            .copy_from_slice(&((MAX_MEMBER_SIZE + 1) as u32).to_le_bytes());
        assert!(
            matches!(render_bytes(&input, &Value::Null, &RenderOptions::default()), Err(Error::Template(message)) if message.contains("limit"))
        );
        let parts: Vec<_> = (0..=MAX_MEMBERS)
            .map(|i| (format!("part{i}"), b"".as_slice()))
            .collect();
        let refs: Vec<_> = parts
            .iter()
            .map(|(name, bytes)| (name.as_str(), *bytes))
            .collect();
        assert!(
            matches!(render_bytes(&package(&refs), &Value::Null, &RenderOptions::default()), Err(Error::Template(message)) if message.contains("too many"))
        );
    }

    #[test]
    fn rejects_spreadsheet_template_fixtures() {
        for input in [
            include_bytes!("../test/datasets/test_xlsx_list.xlsx").as_slice(),
            include_bytes!("../examples/flat_table.ods").as_slice(),
        ] {
            assert!(
                matches!(render_bytes(input, &json!([]), &RenderOptions::default()),
                Err(Error::Unsupported(message)) if message.contains("spreadsheet templates require preprocessing"))
            );
        }
    }

    #[test]
    fn spreadsheet_marker_detection_handles_split_runs_and_static_guids() {
        for marker in [
            "{d.name}",
            "{ c . name}",
            "{t(Hello)}",
            "{$name}",
            "{#name}",
            "{bindColor}",
            "{</text:span><text:span>d</text:span><text:span>.name}",
            "<cell value='{d.name}'/>",
        ] {
            assert!(contains_carbone_marker(marker), "{marker}");
            let input = package(&[
                ("content.xml", marker.as_bytes()),
                (
                    "mimetype",
                    b"application/vnd.oasis.opendocument.spreadsheet",
                ),
            ]);
            assert!(matches!(
                render_bytes(&input, &Value::Null, &RenderOptions::default()),
                Err(Error::Unsupported(_))
            ));
        }
        assert!(!contains_carbone_marker(
            "<p>{79F54976-1DA5-4618-B147-4CDE4B953A38}</p>"
        ));
    }

    #[test]
    fn copies_static_spreadsheets_without_preprocessing() {
        for parts in [
            vec![
                ("[Content_Types].xml", b"<Types/>".as_slice()),
                ("xl/workbook.xml", b"<workbook/>"),
                ("xl/sharedStrings.xml", b"<si>static {GUID}</si>"),
            ],
            vec![
                (
                    "mimetype",
                    b"application/vnd.oasis.opendocument.spreadsheet".as_slice(),
                ),
                ("content.xml", b"<table>static {GUID}</table>"),
                ("META-INF/manifest.xml", b"<manifest/>"),
            ],
        ] {
            let input = package(&parts);
            let output = render_bytes(&input, &Value::Null, &RenderOptions::default()).unwrap();
            for (name, bytes) in parts {
                assert_eq!(part(&output, name), bytes);
            }
        }
    }

    #[test]
    fn still_renders_presentation_archives() {
        for parts in [
            vec![
                ("[Content_Types].xml", b"<Types/>".as_slice()),
                ("ppt/presentation.xml", b"<p/>"),
                ("ppt/slides/slide1.xml", b"<p>{d.name}</p>"),
            ],
            vec![
                (
                    "mimetype",
                    b"application/vnd.oasis.opendocument.presentation".as_slice(),
                ),
                ("content.xml", b"<p>{d.name}</p>"),
            ],
        ] {
            let input = package(&parts);
            let output =
                render_bytes(&input, &json!({"name":"Ada"}), &RenderOptions::default()).unwrap();
            assert_eq!(part(&output, parts.last().unwrap().0), b"<p>Ada</p>");
        }
    }

    #[test]
    fn rejects_underdeclared_expanded_sizes() {
        let mut input = package(&[
            ("content.xml", b"<p>actual data</p>"),
            ("mimetype", b"application/vnd.oasis.opendocument.text"),
        ]);
        let central = input
            .windows(4)
            .position(|bytes| bytes == b"PK\x01\x02")
            .unwrap();
        input[central + 24..central + 28].copy_from_slice(&1_u32.to_le_bytes());
        assert!(render_bytes(&input, &Value::Null, &RenderOptions::default()).is_err());
    }

    #[test]
    fn validates_conversion_extensions_before_launch() {
        for invalid in [
            "",
            ".",
            "../pdf",
            "pdf:writer_pdf_Export",
            "pdf --help",
            "pdf/",
            "é",
            "..docx",
        ] {
            assert!(matches!(
                convert(b"x", "odt", invalid),
                Err(Error::Conversion(_))
            ));
            assert!(matches!(
                convert(b"x", invalid, "pdf"),
                Err(Error::Conversion(_))
            ));
        }
        assert_eq!(extension(".DOCX").unwrap(), "docx");
        assert!(convert(b"", "odt", "pdf").is_err());
    }

    #[cfg(unix)]
    fn fake_converter(script: &str, timeout: Duration) -> Result<Vec<u8>> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("fake office");
        fs::write(&program, format!("#!/bin/sh\n{script}")).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        convert_with_programs(b"input", ".ODT", ".PDF", &[&program], timeout)
    }

    #[test]
    #[cfg(unix)]
    fn conversion_checks_exit_output_timeout_and_arguments() {
        let success = r#"
case "$1" in -env:UserInstallation=file:*) ;; *) exit 2;; esac
while [ "$#" -gt 0 ]; do
 case "$1" in --convert-to) shift; [ "$1" = pdf ] || exit 3;; --outdir) shift; out="$1";; esac
 shift
done
printf '%%PDF-test' > "$out/document.pdf"
"#;
        assert_eq!(
            fake_converter(success, Duration::from_secs(5)).unwrap(),
            b"%PDF-test"
        );
        for script in [
            "exit 7",
            "exit 0",
            &success.replace("printf '%%PDF-test'", "printf ''"),
        ] {
            assert!(matches!(
                fake_converter(script, Duration::from_secs(5)),
                Err(Error::Conversion(_))
            ));
        }
        assert!(
            matches!(fake_converter("exec sleep 5", Duration::from_millis(30)), Err(Error::Conversion(message)) if message.contains("timed out"))
        );
        assert!(
            matches!(convert_with_programs(b"x", "odt", "pdf", &[Path::new("/nonexistent/carbone-office")], Duration::from_secs(1)), Err(Error::Conversion(message)) if message.contains("not installed"))
        );
    }

    #[test]
    #[ignore = "requires LibreOffice installed on PATH"]
    fn libreoffice_converts_odt_to_pdf() {
        let input = render_bytes(
            include_bytes!("../examples/simple.odt"),
            &json!({"firstname":"Ada", "lastname":"Lovelace"}),
            &RenderOptions::default(),
        )
        .unwrap();
        let output = convert(&input, "odt", "pdf").unwrap();
        assert!(output.starts_with(b"%PDF-"));
    }
}
