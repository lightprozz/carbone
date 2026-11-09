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

Public functions:

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
- Standard paired `[i]` / `[i+1]` repetition in matching sibling XML elements,
  including nested arrays, empty arrays and inter-row separators. Simple
  single-marker plain-text repetition is supported; XML loops require balanced
  elements. Unpaired loops return errors.
- Markers split across Office formatting runs; XML escaping and invalid XML
  control-character removal. In attributes, quotes are additionally escaped
  for safety (an intentional improvement over the JavaScript behavior).
- `{t(Hello)}` dictionary translation with fallback to the original key.
- Inline conditions with the original `or`-by-default semantics, matched against
  Node on 21 reference cases: `ifEQ`, `ifNE`, `ifGT`, `ifGTE`, `ifLT`, `ifLTE`,
  `ifIN`, `ifNIN`, `ifEM`, `ifNEM`, `and`, `or`, `show`, `elseShow`, plus the
  legacy `ifEmpty`, `ifEqual`, `ifContain`. `show`/`elseShow` stop the chain.
  A missing value compares as `null` rather than JavaScript's `undefined`.
- Literal formatter arguments and formatter chains: `lowerCase`, `upperCase`,
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

- Variables/aliases, custom/sorted/grouped iterators, conditional selectors,
  conditional text blocks (`showBegin`/`hideBegin`), dynamic formatter
  arguments (so `and(.other)` is rejected; `:and`/`:or` without arguments work),
  backslash-escaped quotes inside formatter arguments, counters,
  aggregations, custom formatters, dates/timezones, locale number/currency
  formatting, and XML-injecting formatters such as `convCRLF`.
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

The checked-in corpus in `tests/fixtures/template-parity.json` records expected
outputs produced by the original Node implementation. `check-parity.cjs` verifies
that reference; `--update` deliberately regenerates it and must be reviewed.
Rust tests compare the same cases and exercise existing document fixtures,
archive safety, CLI behavior, and conversion process failures using a fake
executable. The real PDF test is ignored by default and is **not proof of PDF
conversion until explicitly run with LibreOffice installed**.

`scripts/setup.sh` fetches locked Cargo dependencies and installs the locked Node
reference dependencies without lifecycle scripts. `.hoplite/settings.json` wires
it into workspace setup. Rust must already be provisioned. The Rust GitHub Actions
workflow checks formatting, tests, release compilation and the Node corpus; it
does not run the optional LibreOffice integration test.

## Remaining migration stages

1. Port conditional blocks, variables, dynamic formatter arguments and advanced
   iteration using the existing builder/parser tests as the oracle.
2. Port date/currency/locale formatters, aggregate/counter semantics and a native
   custom-formatter API.
3. Port spreadsheet preprocessing and embedded-document handling using existing
   XLSX/ODS and embedded-document fixtures.
4. Port conversion lifecycle/worker options and the remaining CLI operations;
   run real conversion and cross-platform compatibility gates.
5. Only then retire the Node implementation and its dependency/workflow files.

Keeping the original source and tests during these stages is intentional; it
prevents removing working features before native parity is demonstrated.
