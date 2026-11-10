//! Native Carbone migration preview. See `doc/RUST.md` for the compatibility scope.
//!
//! ```
//! use carbone::{render_template, RenderOptions};
//! use serde_json::json;
//! let report = render_template("Hello {d.name}!", &json!({"name": "Ada"}),
//!                              &RenderOptions::default())?;
//! assert_eq!(report, "Hello Ada!");
//! # Ok::<(), carbone::Error>(())
//! ```

mod aliases;
mod blocks;
mod compile;
mod conditions;
mod document;
mod engine;
mod formatters;
mod registry;
mod render;
mod selection;
mod syntax;
mod template;
mod xml;

pub use document::{convert, render_bytes, render_file};
pub use engine::{CompiledTemplate, Engine, EngineBuilder, compile_template};
pub use registry::{Formatter, FormatterContext, Registry};
pub use template::render_template;

use serde_json::Value;

/// Options are per render; the native library does not mutate global state.
#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub complement: Value,
    pub lang: String,
    /// A language-keyed dictionary, e.g. `{"fr": {"Hello": "Bonjour"}}`.
    pub translations: Value,
    /// Disable only when rendering plain text or intentionally trusting raw XML data.
    pub escape_xml: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            complement: Value::Null,
            lang: "en".into(),
            translations: Value::Null,
            escape_xml: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("template is not UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("invalid template: {0}")]
    Template(String),
    #[error("not supported by the Rust migration yet: {0}")]
    Unsupported(String),
    #[error("document conversion failed: {0}")]
    Conversion(String),
}

pub type Result<T> = std::result::Result<T, Error>;
