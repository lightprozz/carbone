use std::error::Error;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use carbone::RenderOptions;
use clap::{Args, Parser, Subcommand};
use serde_json::Value;
use tempfile::NamedTempFile;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Parser)]
#[command(name = "carbone", version, about = "Native Carbone template rendering")]
#[command(
    after_help = "This staged native CLI supports render only; legacy find and translate commands are not implemented."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render a template using JSON data.
    Render(RenderArgs),
}

#[derive(Args)]
struct RenderArgs {
    /// Template file to render.
    template: PathBuf,
    /// JSON data file, or - to read JSON from stdin.
    #[arg(value_name = "DATA.json")]
    data: PathBuf,
    /// Destination file, replaced only after rendering succeeds.
    #[arg(long, value_name = "OUTPUT")]
    output: PathBuf,
    /// Additional JSON data exposed as c in the template.
    #[arg(long, value_name = "FILE")]
    complement: Option<PathBuf>,
    /// Language used for rendering.
    #[arg(long, value_name = "LANG")]
    lang: Option<String>,
    /// JSON translations file.
    #[arg(long, value_name = "FILE")]
    translations: Option<PathBuf>,
    /// Convert the rendered document to this extension.
    #[arg(long, value_name = "EXT")]
    convert_to: Option<String>,
    /// Disable XML escaping of inserted values.
    #[arg(long)]
    no_escape: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Render(args) => render(args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("carbone: {error}");
            ExitCode::FAILURE
        }
    }
}

fn read_json(path: &Path) -> Result<Value> {
    let file = File::open(path)
        .map_err(|error| format!("cannot read JSON file '{}': {error}", path.display()))?;
    serde_json::from_reader(file)
        .map_err(|error| format!("invalid JSON in '{}': {error}", path.display()).into())
}

fn render(args: RenderArgs) -> Result<()> {
    let data = if args.data == Path::new("-") {
        serde_json::from_reader(io::stdin().lock())
            .map_err(|error| format!("invalid JSON from stdin: {error}"))?
    } else {
        read_json(&args.data)?
    };
    let mut options = RenderOptions::default();
    if let Some(path) = &args.complement {
        options.complement = read_json(path)?;
    }
    if let Some(lang) = args.lang {
        options.lang = lang;
    }
    if let Some(path) = &args.translations {
        options.translations = read_json(path)?;
    }
    if args.no_escape {
        options.escape_xml = false;
    }

    let mut bytes = carbone::render_file(&args.template, &data, &options)
        .map_err(|error| format!("cannot render '{}': {error}", args.template.display()))?;
    if let Some(extension) = args.convert_to {
        let input_extension = args
            .template
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| !extension.is_empty())
            .ok_or("conversion requires a template with a valid file extension")?;
        bytes = carbone::convert(&bytes, input_extension, &extension)
            .map_err(|error| format!("cannot convert to '{extension}': {error}"))?;
    }

    // A sibling temporary file keeps replacement atomic on the destination filesystem.
    let parent = args
        .output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| format!("cannot prepare output '{}': {error}", args.output.display()))?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.flush())
        .map_err(|error| format!("cannot write output '{}': {error}", args.output.display()))?;
    temporary
        .persist(&args.output)
        .map_err(|error| format!("cannot save output '{}': {error}", args.output.display()))?;
    Ok(())
}
