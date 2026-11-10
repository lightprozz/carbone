# Build the native Rust CLI

This guide builds the native Rust migration preview from the repository root.
The JavaScript implementation remains the compatibility reference; the Rust
binary does not need Node.js or npm to build or render documents.

## Prerequisites

- Rust **1.93.0** and Cargo, pinned by [`rust-toolchain.toml`](../rust-toolchain.toml).
  Install Rust with [rustup](https://rust-lang.org/tools/install/); the pinned
  toolchain uses the minimal profile. Install `rustfmt` as well if you plan to
  run the formatting check below:

  ```sh
  rustup toolchain install 1.93.0 --profile minimal --component rustfmt
  ```

- Node.js and npm are only needed for the JavaScript reference parity check and
  the legacy JavaScript test suite, not for the production Rust build.
- LibreOffice (`soffice` on `PATH`) is optional. It is needed only when asking
  the CLI to convert a rendered document, for example to PDF; ordinary rendering
  does not require it.

## Build

Run these commands from the repository root. Cargo downloads locked Rust
dependencies as needed. The debug and optimized binaries are written to
`target/debug/carbone` and `target/release/carbone`, respectively.

```sh
cargo build --locked
cargo build --locked --release
```

## Render a document

The CLI currently provides the `render` command. This small XML example needs
no LibreOffice and writes only inside a temporary directory:

```sh
tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

cat > "$tmpdir/template.xml" <<'EOF'
<greeting>Hello {d.name}</greeting>
EOF
cat > "$tmpdir/data.json" <<'EOF'
{"name":"Ada & Grace"}
EOF

./target/release/carbone render \
  "$tmpdir/template.xml" "$tmpdir/data.json" \
  --output "$tmpdir/rendered.xml"
cat "$tmpdir/rendered.xml"
```

The output is `<greeting>Hello Ada &amp; Grace</greeting>`; inserted XML
characters are escaped by default. Run `./target/release/carbone render --help`
for the available options. The JSON argument can be `-` to read from standard
input. The [Rust migration guide](RUST.md) describes additional options and
current compatibility limits.

To request a conversion, specify the destination extension and ensure
LibreOffice is installed and discoverable as `soffice`. This example creates
input data in the current directory:

```sh
printf '%s\n' '{"firstname":"John","lastname":"Doe"}' > data.json
./target/release/carbone render examples/simple.odt data.json \
  --output report.pdf --convert-to pdf
```

This conversion command is illustrative; conversion requires a working local
LibreOffice installation and is not part of the standard build or test commands.

## Verify changes

The Rust checks used by CI are:

```sh
cargo fmt --all --check
cargo test --locked --all-targets
cargo test --locked --doc
cargo build --locked --release
```

The Node reference check validates the committed expected outputs against the
existing JavaScript implementation. Separately, the Rust parity tests compare
native output with that same fixture corpus. Install the JavaScript reference
dependencies and run the Node check with:

```sh
npm ci --ignore-scripts
node scripts/check-parity.cjs
```

These Node dependencies support the reference check; they are not dependencies
of the Rust executable. `npm test` runs the separate legacy JavaScript test
suite and does not test the native Rust CLI.

The optional LibreOffice integration test is explicitly ignored in the normal
Rust test run. If LibreOffice is available, run that test separately:

```sh
cargo test --locked --lib document::tests::libreoffice_converts_odt_to_pdf -- --ignored
```

The CI commands and parity check are defined in
[`rust.yml`](../.github/workflows/rust.yml). The convenience
[`scripts/setup.sh`](../scripts/setup.sh) fetches locked Rust dependencies and
installs the Node reference dependencies; it is not needed for a Rust-only
build.
