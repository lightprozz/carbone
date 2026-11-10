# Native Rust migration

This repository now contains a native Rust library and CLI alongside the original
Carbone v3.9.1 JavaScript implementation. **Version 0.1.0 is a migration preview,
not a drop-in or feature-complete replacement.** Rust rendering does not invoke
Node.js, JavaScript, Python, or a hosted service. LibreOffice is still required
for document conversion, including PDF output.

The existing [Carbone Code License](../LICENSE.md) applies without changes. This
crate is not configured for publication to crates.io.

## Build and run

Use Rust **1.93.0**, pinned in `rust-toolchain.toml`. Follow the official
[Rust installation instructions](https://rust-lang.org/tools/install/) if needed.
With rustup already installed, provision the pinned toolchain:

```sh
rustup toolchain install 1.93.0 --profile minimal --component rustfmt
cargo build --locked --release
```

Create `data.json` containing `{"firstname":"John","lastname":"Doe"}`, then:

```sh
./target/release/carbone render examples/simple.odt data.json --output report.odt
# Optional: requires LibreOffice/soffice on PATH
./target/release/carbone render examples/simple.odt data.json --output report.pdf --convert-to pdf
# JSON input can also come from stdin
printf '{"firstname":"John","lastname":"Doe"}' | \
  ./target/release/carbone render examples/simple.odt - --output report.odt
```

Use `carbone render --help` for options. `--complement FILE` exposes additional
JSON under `c`. `--lang fr --translations FILE` selects a dictionary such as
`{"fr":{"Hello":"Bonjour"}}`. For CSV/plain-text output, `--no-escape` disables
XML escaping; do not use it for untrusted content inserted into XML.

The output file is replaced atomically only after rendering and any conversion
succeed. Errors go to stderr with a nonzero exit code. The native CLI currently
provides `render`, not the old `find` or translation-file generation commands.

## Library

Use the local crate as a path dependency while the migration is in progress:

```toml
[dependencies]
carbone = { path = "/path/to/this/repository" }
serde_json = "1"
```

```rust
use carbone::{RenderOptions, render_file, render_template};
use serde_json::json;

fn main() -> carbone::Result<()> {
    let data = json!({"firstname": "John", "lastname": "Doe"});
    let options = RenderOptions::default();
    let text = render_template("Hello {d.firstname}!", &data, &options)?;
    assert_eq!(text, "Hello John!");
    let document = render_file("examples/simple.odt", &data, &options)?;
    std::fs::write("report.odt", document)?;
    Ok(())
}
```

### Reusable compiled templates and custom formatters

```rust
use carbone::{compile_template, RenderOptions};
use serde_json::json;

let template = compile_template("Hello {d.name:upperCase}!")?;
assert_eq!(template.render(&json!({"name": "Ada"}), &RenderOptions::default())?, "Hello ADA!");
assert_eq!(template.render(&json!({"name": "Lin"}), &RenderOptions::default())?, "Hello LIN!");
# Ok::<(), carbone::Error>(())
```

`CompiledTemplate` is an immutable, `Send + Sync` text/XML plan. Each render
has fresh bindings and formatter state; data is never retained by compilation.
Package compilation (`compile_file`) is not implemented yet; archive APIs still
compile each XML part per call.

Implement `Formatter::call` to add a trusted Rust formatter and optionally
`Formatter::validate` to check literal arguments at compile time. Register it
with `Engine::builder().formatter("name", implementation).build()`, then use
`engine.compile(text)?.render(data, options)`. `FormatterContext` exposes the
render inputs and a `stop()` method to end the current marker's pipeline.
Custom formatter implementations are shared across renders: any mutable state
they own must be synchronized and isolated as appropriate. Their JSON values
use the same escaping as built-ins; raw XML and package-edit outputs are not
exposed yet. Formatter registration is engine-scoped, not process-global.

Public functions:

- `compile_template(&str) -> Result<CompiledTemplate>`
- `Engine::compile(&str) -> Result<CompiledTemplate>`
- `CompiledTemplate::render(&Value, &RenderOptions) -> Result<String>`
- `render_template(&str, &Value, &RenderOptions) -> Result<String>`
- `render_bytes(&[u8], &Value, &RenderOptions) -> Result<Vec<u8>>`
- `render_file(impl AsRef<Path>, &Value, &RenderOptions) -> Result<Vec<u8>>`
- `convert(&[u8], input_extension, output_extension) -> Result<Vec<u8>>`

Options are per render; there is no process-global `set` or mutable formatter
registry. Errors distinguish I/O, JSON, ZIP, UTF-8, invalid templates, unsupported
features, and conversion failures. There is no Node callback or binding layer.

## Compatibility boundary

Implemented and tested:

- Scalar/nested `{d.name}` and `{c.name}` markers, adjacent markers, null/missing
  values, fixed array selectors `[i=0]` and native numeric selectors `[0]`.
- Paired `[i]` / `[i+1]` repetition, including nested arrays, empty arrays,
  flat/non-sibling XML boundaries and inter-row separators. Object iteration
  exposes `.att`/`.val`; selectors support filters, negative indices, custom and
  multiple sorting/grouping keys, and aligned `++` columns. Unpaired loops fail.
  Bare primitive iterator rendering is a native extension, not Node parity.
- Relative formatter arguments (`.sibling`, `..parent`) and typed `and`/`or`
  chaining. Community bare `d.x`/`c.x` formatter arguments are literals, not
  dynamic roots; relative argument properties use dots, not bracket indexing.
- Compile-time aliases (`{#name=…}`, `{$name}`), including positional parameters,
  empty `()` declarations, Unicode/digit/hyphen names, and JavaScript replacement
  tokens (`$$`, `$&`, prefix and suffix substitutions).
- Conditional `showBegin`/`showEnd` and `hideBegin`/`hideEnd` blocks, including
  nested loops, split markers and safe removal of disjoint XML regions.
- `count` with literal or relative start arguments, multiple counters per loop,
  and token-inspecting formatter chains. Parser-assigned IDs preserve the
  reference's global-regex cursor and UTF-16 position behavior, including skipped
  ID injection. The first surviving numeric token initializes a render-local
  counter by ID; later starts do not reset it. Only the first matching token in
  each value part resolves. Hidden rows advance counters before XML is discarded.
  Negative/fractional starts remain literal tokens, matching the reference.
- Non-executing arithmetic expressions in `add/sub/mul/div`, `convEnum`
  through `RenderOptions.enums`, and deferred `convCRLF`. DOCX/ODT packages infer
  their source format; flat XML callers set `RenderOptions.extension`. Data is
  escaped before formatter-owned line-break XML is inserted.
- Markers split across Office formatting runs; XML escaping and invalid XML
  control-character removal. In attributes, quotes are additionally escaped
  for safety (an intentional improvement over the JavaScript behavior).
- `{t(Hello)}` dictionary translation with fallback to the original key.
- Inline conditions with the original `or`-by-default semantics, matched against
  Node on 21 reference cases: `ifEQ`, `ifNE`, `ifGT`, `ifGTE`, `ifLT`, `ifLTE`,
  `ifIN`, `ifNIN`, `ifEM`, `ifNEM`, `and`, `or`, `show`, `elseShow`, plus the
  legacy `ifEmpty`, `ifEqual`, `ifContain`. `show`/`elseShow` stop the chain.
  A missing value compares as `null` rather than JavaScript's `undefined`.
- Formatter arguments and formatter chains: `lowerCase`, `upperCase`,
  `ucFirst`, `ucWords`, `unaccent`, `print`, `prepend`, `substr`/`slice`, `padl`,
  `padr`, `md5`, `arrayJoin`, `arrayMap`, `len`, `int`, `toEN`, `toFR`, `toFixed`,
  `round`, `add`, `sub`, `mul`, `div`. These are a tested subset, not a promise
  of complete JavaScript coercion/Unicode equivalence. Non-finite numeric results
  and invalid UTF-16 slices return errors instead of producing invalid JSON/text.
- UTF-8 text/XML and basic DOCX/ODT/PPTX/ODP ZIP packaging. XML parts, headers,
  footers and relationships are rendered; binary assets remain untouched. ODF
  `mimetype` stays first and uncompressed. Unchanged ZIP entries retain their
  compressed data; changed entries retain compression, timestamps and Unix
  permissions. Arbitrary extra fields and per-entry comments are not guaranteed.
- Optional synchronous LibreOffice conversion using a private profile and
  temporary directory, disabled document macros, a 60-second timeout, and
  validated process exit/output. Extensions cannot contain paths or filter
  arguments. There is no worker pool or retry policy yet.

Not yet ported (do not migrate templates relying on these):

- Full B1–B6 edge-case parity: alias names cannot contain regular-expression
  syntax; parameter identifiers remain ASCII. Whitespace-only parameter lists,
  non-marker top-level expansions and suffix replacements injecting parentheses
  are rejected rather than emulating the reference parser's truncation quirks.
  Counters still require a loop. Missing/null dynamic arguments, counters inside
  structural begin/end pipelines, counters combined with translations, alias
  expansions that remove markers or introduce structural markers, and ambiguous
  `count` text in fields/arguments are explicitly rejected. Translation output
  can change counter IDs at render time; that combination needs a future plan. Enum prototype
  inheritance and XML line-break injection into attributes are rejected.
  Structural block formatters must terminate their chain and have matched pairs.
- Backslash-escaped formatter quotes, aggregations, dates/timezones and locale
  number/currency formatting.
- XLSX/ODS spreadsheet preprocessing: shared strings, numeric cell typing,
  formulas, calculation chains and row/cell reference repair. Spreadsheet
  archives containing template markers are rejected; static archives can be
  copied. Rendering spreadsheet XML directly does not provide preprocessing.
- Embedded documents, signed packages, template includes, dynamic images/colors,
  report names, translation-file generation, caches and conversion worker pools.
  Embedded/signed archive packages and recognized unsupported markers return
  errors rather than silently passing through.
- Full compatibility with legacy text encodings, arbitrary malformed HTML,
  UTF-16 lone surrogates or every JavaScript property/coercion edge case.

Unrecognized literal braces such as GUIDs are retained. The Rust engine is an
interpreter, not a JavaScript evaluator. Data values are never reinterpreted as
new template code. Unsupported features should be ported against reference tests
before broadening this compatibility boundary.

## Limits and untrusted inputs

Rendering limits: 32 MiB per input text template, 64 MiB output text, 64 nesting
levels and 100,000 items per loop. Archive limits: 200 MiB input/expanded/output,
64 MiB per member and 10,000 members. Archives are handled in memory without
extracting member paths to disk; unsafe names, duplicates, symlinks, unsupported
compression, corrupt CRCs and excessive expansion are rejected.

These bounds are not an OS sandbox or a complete CPU budget. LibreOffice profile
isolation is not process isolation; run conversion of untrusted documents in a
restricted OS/container environment. The timeout controls the launched process,
not a general process-tree supervisor. Template text itself is trusted XML; only
inserted data is escaped.

## Verification and development

```sh
cargo fmt --all --check
cargo test --locked --all-targets
cargo test --locked --doc
cargo build --locked --release

# Node is needed only for the migration's compatibility oracle.
npm ci --ignore-scripts
node scripts/check-parity.cjs
TZ=Europe/Paris ./node_modules/.bin/mocha \
  test/test.builder.js test/test.builder.buildXML.js test/test.parser.js \
  test/test.extracter.js test/test.formatter.js test/test.helper.js \
  test/test.input.js --timeout 100000

# Explicit integration gate when LibreOffice is installed:
cargo test --locked --lib document::tests::libreoffice_converts_odt_to_pdf -- --ignored
```

The corpora in `tests/fixtures/` record 326 template results from Node 3.9.1
(325 supported outputs and one explicitly rejected alias edge case);
`tests/formatter-fixtures/` holds 106 additional formatter/error/render
records. `check-parity.cjs` verifies both schemas against the original engine;
`--update` deliberately regenerates expectations and must be reviewed. Rust tests
compare supported outputs, reject recorded unsupported shapes, render reused
plans twice, and exercise existing document fixtures,
archive safety, CLI behavior, and conversion process failures using a fake
executable. The real PDF test is ignored by default and is **not proof of PDF
conversion until explicitly run with LibreOffice installed**.

`scripts/setup.sh` fetches locked Cargo dependencies and installs the locked Node
reference dependencies without lifecycle scripts. `.hoplite/settings.json` wires
it into workspace setup. Rust must already be provisioned. The Rust GitHub Actions
workflow checks formatting, tests, release compilation and the Node corpus; it
does not run the optional LibreOffice integration test.

## Remaining migration stages

Architecture stages A1–A3 (tokenization, compiled plans and a formatter registry)
are implemented. The detailed plan (Community parity, Enterprise-equivalent
features) is in [RUST_PLAN.md](RUST_PLAN.md). In short:

1. Close the documented B1–B6 compatibility boundaries; their core features now
   have native implementations and recorded builder/parser/formatter oracles.
2. Port date/currency/locale formatters and aggregate semantics. The
   native custom-formatter API is already available.
3. Port spreadsheet preprocessing and embedded-document handling using existing
   XLSX/ODS and embedded-document fixtures.
4. Port conversion lifecycle/worker options and the remaining CLI operations;
   run real conversion and cross-platform compatibility gates.
5. Retire the Node implementation and its dependency/workflow files only after
   full parity and explicit approval.

Keeping the original source and tests during these stages is intentional; it
prevents removing working features before native parity is demonstrated.
