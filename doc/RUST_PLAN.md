# Rust implementation plan

Status of the native rewrite is in [RUST.md](RUST.md). This document is the
plan and progress record, in three tracks:

- **A. Architecture** – replace the preview engine with a design that can carry
  every remaining feature.
- **B. Community parity** – finish everything the JavaScript v3.9.1 engine does.
- **C. Enterprise-equivalent features** – built from the public documentation.

The project is an internal learning tool and is free to be redesigned: matching
the JavaScript *output* is the goal for track B, matching its *structure* is not.

## Ground rules

1. **Every stage ends green**: `cargo fmt --check`, `cargo test`, and
   `node scripts/check-parity.cjs`.
2. **Track B is oracle-driven.** Expected outputs are recorded from the Node
   engine into `tests/fixtures/`, never written by hand. The 850 existing
   JavaScript tests are the source of cases.
3. **Track C has no oracle.** The Enterprise engine is closed-source, so each
   feature is specified from the public documentation examples and validated by
   structural tests plus opening the result in LibreOffice. Parity with the real
   product cannot be proven and is not claimed.
4. **Unsupported input fails loudly.** A template using a feature that is not
   ported yet returns `Error::Unsupported`; it never renders wrongly.
5. **Licence.** The existing [Carbone Code License](../LICENSE.md) remains
   unchanged. It includes internal-use derivative-work grants and restrictions
   on competing document generators, services and use of the code/documentation.
   Internal educational intent is not a legal determination that every planned
   feature or use is permitted. Confirm the applicable terms before expanding
   use or distribution; proprietary Enterprise binaries are not inspected.

## Implementation progress

- **A1 complete:** dedicated `syntax` parser and zero-copy `xml` tokenizer now
  drive the engine. Byte spans, quoted delimiters, XML entities, split runs,
  comments, CDATA, DOCTYPE subsets and malformed input have regression coverage.
  A bounded deterministic fuzz-style test exercises 4,096 parser inputs; this is
  not a substitute for a dedicated coverage-guided fuzzing campaign.
- **A2 complete:** `compile` produces a static/value/loop plan; `render` walks it
  without reparsing markers or XML. `compile_template` and `Engine::compile`
  return an immutable `CompiledTemplate`, reusable with different data/options
  and shareable across threads. Compilation validates unsupported markers even
  in empty loops. All recorded template cases exercise one-shot and reused plans.
- **A3 complete:** engine-scoped `Registry`, `Formatter` and `FormatterContext`
  route built-ins and conditions through the same interface. Custom formatters
  validate literal arguments at compile time; per-marker condition/stop state is
  isolated. Existing `conditions.rs` supplies an adapter, not a render special
  case. Custom formatter output remains a normally escaped JSON value. B6 adds
  private deferred/raw output handling for built-in line breaks; public raw-XML
  and package-edit actions remain deferred.
- **B1–B6 core implemented and integrated:** relative formatter arguments and
  typed `and`/`or` values; parameterized compile-time aliases; array/object
  filtering, custom/multiple-key sorting and grouping; nested, flat and aligned
  `++` iteration; safe XML conditional blocks; counters, arithmetic arguments,
  enums and deferred DOCX/ODT line breaks. The original JavaScript remains the
  reference engine. This is not a claim that every Community edge case is ported.
- **Verification:** 283 template oracle records (282 supported outputs and one
  explicitly rejected alias case) plus 106 formatter/error/known-limit records
  are checked against Node 3.9.1. Rust exercises supported outputs and
  explicit rejection cases; template plans render twice to check state isolation.
  The full track-B exit criterion below has not been reached.
- **Alias edge cases closed:** empty `()` declarations, Unicode/digit/hyphen
  names and JavaScript replacement tokens now have recorded Node coverage.
- **Known B1–B6 boundaries:** aliases reject regular-expression syntax in names,
  non-ASCII parameter identifiers, whitespace-only parameter lists, non-marker
  top-level expansions and suffix substitutions injecting parentheses. The last
  case is recorded and explicitly rejected instead of silently diverging from
  the reference parser's truncation behavior. Relative formatter paths use
  dot-separated property access, not bracket indexing. Bare `d.x`/`c.x` formatter arguments are literals, as in the
  Community oracle, not dynamic roots (correcting the original B1 proposal).
  Bare primitive iterator rendering remains a native extension, not Node parity.
  Counters support literal and relative starts, initialized from the first
  surviving token rather than reset on each row. They still require one marker
  per loop and a nonnegative integer start (empty strings start at zero);
  missing/null, negative and fractional starts, multiple count markers and
  formatter chains dependent on reference token IDs fail explicitly. Hidden
  rows advance counters before their XML is discarded.
  Enum prototype inheritance and XML line breaks inside attributes are rejected.
  Structural block formatters must terminate their chain and have matched pairs.
- **Next:** close the remaining B1–B6 compatibility boundaries, then A4/A5 and
  B7 onward in the order below. These stages and all Enterprise-equivalent
  features are **not implemented** by this milestone.

## Track A – architecture

### Why the preview engine had to change

The original `src/template.rs` found markers with a regex and recursively sliced
the template string around each `[i]`/`[i+1]` pair. That was enough for the
first stage but could not express the remaining features:

- sorting, filtering and multiple iterators need the loop to be planned before
  any output is produced;
- conditional blocks, pictures, colours and HTML must edit XML *around* a
  marker, not only replace the marker text;
- formatter arguments are plain strings, so dynamic arguments (`.other`,
  `$alias`) and maths expressions could not be evaluated;
- conditions required special-casing in the render loop;
- every loop iteration re-parsed its fragment.

### Target design

A compile-once, render-many pipeline. Each layer is a separate module with its
own tests:

```
bytes ──▶ package ──▶ preprocess ──▶ compile ──▶ render ──▶ postprocess ──▶ package ──▶ convert
          (zip/xml)   (per format)   (to plan)   (plan+data)  (per format)    (zip)      (LibreOffice)
```

| Layer | Module | Responsibility |
|---|---|---|
| Package | `package/` | Read/write ZIP or flat files into a `Template { parts, kind }`. Today's `document.rs`, minus rendering. |
| XML | `xml/` | Tolerant tokenizer (tags, text, positions). No DOM; Office XML is too large and too irregular. Replaces the ad-hoc `tag_end`/`elements` helpers. |
| Syntax | `syntax/` | Lexer and parser from marker text to an AST: `Path`, `Selector` (index, iterator, filter), `Formatter { name, args: Vec<Expr> }`, `Expr` (literal, relative path, alias, maths). Replaces the regex and `split_top`. |
| Compile | `compile/` | Marker AST + XML tokens → `Plan`: a tree of `Static`, `Value`, `Loop { source, iterators, filters, body }`, `Block { condition, body }`. Loop boundaries are resolved here, once. |
| Render | `render/` | Walk the plan with a `Scope` stack (data, complement, parent chain, loop state, aliases). Sort/filter/group happen here on plain data. |
| Formatters | `formatters/` | `trait Formatter { fn call(&self, ctx: &mut Ctx, value, args) -> Result<Output> }` in a `Registry`. `Output` is `Value`, `RawXml`, or a `PostAction` (picture, link, colour). Conditions become ordinary formatters using `Ctx` state. Users can register their own. |
| Format hooks | `formats/{docx,xlsx,pptx,odt,ods,odp,html}` | `trait FormatHooks { preprocess, postprocess }`. All format-specific knowledge lives here (shared strings, relationships, content types, manifest). |
| Convert | `convert/` | `trait Converter`; LibreOffice implementation with a worker pool. |

Public API after the redesign:

```rust
let engine = Engine::builder().formatter("myFmt", my_fmt).build();
let template = engine.compile_file("invoice.docx")?;   // reusable, Send + Sync
let bytes = template.render(&data, &options)?;
```

### Stages

| # | Work | Done when |
|---|---|---|
| A1 | `xml` tokenizer and `syntax` parser, used by the existing engine | All original oracle cases still pass; parser fuzz test does not panic |
| A2 | `compile` → `Plan` and `render` walker replace the recursive slicer | Same oracle cases pass; a compiled template renders twice with different data |
| A3 | `Registry`/`Ctx` formatter trait; conditions and built-ins moved onto it | `conditions.rs` special-case removed from the render loop |
| A4 | `Template`/`FormatHooks` split out of `document.rs` | DOCX/ODT/PPTX/ODP fixture tests unchanged |
| A5 | Benchmarks (`criterion`) for compile and render; corpus size raised to ~300 cases from `test.builder.buildXML.js` and `test.carbone.js` | Baseline numbers recorded in `RUST.md` |

A1–A3 are now complete and precede the remaining template-language work: every item in track B below lands
in the new engine, not the preview one.

## Track B – Community parity

Ordered by dependency. "Tests" is the number of existing JavaScript tests that
mention the feature and can be turned into oracle cases.

| # | Feature | Notes | Tests |
|---|---|---|---|
| B1 | Dynamic formatter arguments | Implemented: `.sibling`, `..parent`, typed `and(.other)`; bare `d.x`/`c.x` are Node literals | 15 oracle cases |
| B2 | Aliases `{#name = …}` / `{$name}` with parameters | Compile-time expansion, empty signatures, literal Unicode/digit/hyphen names and replacement tokens | 38 output oracles + 1 rejection |
| B3 | Filters `[i, attr > 1]`, `[attr = x]`, object iteration `[i].att`/`.val` | Evaluated in `render` | ~41 |
| B4 | Custom and multiple iterators, sorting, `[i+1]` on any attribute, loops across non-sibling rows | The core of `extracter.js`; hardest item of this track | ~15 + 75 extracter |
| B5 | Conditional blocks `showBegin`/`showEnd`, `hideBegin`/`hideEnd` | Needs `parser.findSafeConditionalBlockPosition` behaviour on the XML tokens | ~3 + parser |
| B6 | `count`, maths expressions in `add/sub/mul/div`, `convEnum`, `convCRLF` | Relative count starts retain the first surviving start; `convCRLF` is the first `RawXml` output | ~10 |
| B7 | Number and currency: `formatN`, `formatC`, `convCurr`, locales | Port `_locale.js` and `_currency.js` as data tables | ~12 |
| B8 | Dates: `formatD`, `convDate`, timezones, locales | `jiff` or `chrono` + `chrono-tz`; map dayjs format tokens | in `test.formatter.js` |
| B9 | Spreadsheets: shared strings → inline, numeric cell typing, calc-chain and row-counter removal (`preprocessor.js`) | Removes today's "spreadsheets unsupported" error | ~47 + 19 |
| B10 | Embedded documents, soft page breaks, `hardRefresh`, `reportName`, `options.enum`, `variableStr` | Remaining `index.js` options | ~10 |
| B11 | Translations tooling: lang files loading, `carbone translate`, `carbone find` | CLI parity | 14 |
| B12 | Converter: persistent LibreOffice workers, retries, timeouts, `convertTo` options (PDF filter options, CSV separators) | Needs LibreOffice in CI | 24 |
| B13 | Retire JavaScript | Only after the full JS suite has an equivalent passing Rust case | – |

Exit criterion for track B: every non-skipped case in `test.builder.buildXML.js`,
`test.carbone.js`, `test.formatter.js` and `test.preprocessor.js` has a passing
oracle case, and the real PDF conversion test runs in CI.

## Track C – Enterprise-equivalent features

Built from the public documentation at
<https://carbone.io/documentation/design/overview/template-feature.html>. Each
feature gets a short spec in `doc/spec/<feature>.md` (syntax accepted, formats
covered, known differences) before code.

| # | Feature | Depends on | Size | Approach |
|---|---|---|---|---|
| C1 | Aggregators `aggSum`, `aggAvg`, `aggMin`, `aggMax`, `aggCount`, `cumSum`, `cumCount`, with `partitionBy` | B3, B4 | S | Two-pass render: aggregate over the loop source, then emit |
| C2 | Store and transform `:set`, extra text/array/number/interval formatters listed in the docs | A3 | S–M | Plain registry additions |
| C3 | Distinct, lookup, smart conditions (`drop`/`keep` of row, paragraph, table…) | B4, B5 | M | `Block` nodes that target the enclosing XML element by kind |
| C4 | Hyperlinks | A4 | S | DOCX/XLSX relationships, ODF `xlink:href`; URL validation |
| C5 | Colours `bindColor` and `:color` | A4 | M | Style cloning in `styles.xml` / automatic styles |
| C6 | Pictures from URL or base64 | A4, C4 | M | Fetch behind an allow-list with size and time limits; `image` crate for dimensions; fit modes |
| C7 | Barcodes and QR codes | C6 | M | `barcoders`, `qrcode`, `rxing`; emitted as pictures |
| C8 | HTML → native content `:html` | A1, C4, C5 | L | Subset first: `p`, `b`, `i`, `u`, `br`, lists, links, tables; `html5ever` parser |
| C9 | Charts | C6 | L | Stage 1: bind data into native DOCX/ODT charts. Stage 2: generated SVG via `plotters` |
| C10 | Forms (checkbox, text field, dropdown values) | A4 | M | DOCX/ODT form controls; PDF forms only through conversion |
| C11 | Pagination helpers and file operations (append/merge PDF, page selection) | B12 | M | `lopdf` for post-conversion operations |
| C12 | PDF templates and digital signatures | C11 | L | `lopdf` + CMS signing; highest risk, last |
| C13 | HTML/Markdown template formats, in-template options | B12 | M | HTML→PDF needs a browser engine; decide then |
| C14 | HTTP service: REST API compatible in shape with the documented one, template store, batch | all | M | `axum`; optional crate `carbone-server` |

Security work that track C introduces and must be designed in, not added
later: outbound fetches (pictures) need an allow-list, private-address
blocking and size limits; HTML input is sanitised to the supported subset;
signature keys are never read from template data.

## Order of work

1. **A1 → A2 → A3** (new engine, no new features)
2. **B1 → B2 → B3 → B4 → B5 → B6** (template language complete)
3. **A4 → A5**, then **B7, B8** (formatters) and **B9, B10** (spreadsheets, options)
4. **B11, B12** – Community parity reached; **B13** optional
5. **C1 → C2 → C3 → C4 → C5** (small, no new heavy dependencies)
6. **C6 → C7 → C10**
7. **C8, C9** (large)
8. **C11 → C12 → C13 → C14**

Steps 1–2 are the critical path. Later features may still require changes to
the plan representation, formatter outputs or format hooks as their specs mature. Steps 5–8 can be reordered freely to match what the internal project
needs first.

## Risks

- **B4 (iteration model).** The JavaScript engine sorts emitted XML parts by a
  position array rather than nesting loops. If the plan tree cannot reproduce an
  edge case, the fallback is to port the "sort parts" algorithm inside `render`.
- **No oracle for track C.** Mitigated by per-feature specs and LibreOffice
  round-trip tests, but differences from the real Enterprise output will exist.
- **LibreOffice in CI.** Needed from B12 on; until then conversion stays
  unverified.
- **Scope.** Track C is larger than tracks A and B combined. C8, C9 and C12 are
  each comparable to all of track B's template-language work.
