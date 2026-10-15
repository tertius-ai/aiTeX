# Font architecture

The `aitex-fonts` crate is aiTeX's permanent bytes-in/bytes-out boundary for
font work. It lets the existing classic TeX route and the future Unicode route
share preparation and policy without putting filesystem access or a large font
dependency graph in `aitex-engine`.

## Ownership and flow

```text
CLI / World                     aitex-fonts                      consumers
-----------                     -----------                      ---------
find and read font bytes  -->   inspect face + fsType      -->   cache policy
Unicode itemized run      -->   HarfRust shape             -->   page events
used glyphs + scalars     -->   FontSubsetter / Skera      -->   PDF Type 0/CID
PFB bytes + map transform -->   classic Type 1 preparation -->   shared PDF driver
```

The `aitex-unicode` host service owns paths, system-font discovery, bidi/script
itemization, line-break opportunities, deterministic fallback, and selected
font lifetimes. The font crate accepts borrowed bytes and returns owned aiTeX
data or font bytes. The
engine's only external direct dependencies remain `serde` and `postcard`; its
other direct dependency is the small first-party, dependency-free
`aitex-page` crate.

## Stable aiTeX model

No Fontations, HarfRust, or Skera type appears in the public API. aiTeX has
separate types for a content-addressed face, normalized font instance, source
glyph ID, subset glyph ID, PDF character code, and CID. This prevents an
accidental equality between values that happen to be integers but have
different lifetimes and renumbering rules.

Extensible public records and semantic enums are non-exhaustive. Page glyph
runs, native engine runs, PDF options, and OpenType font sources use
constructors plus optional builders, so adding language, accessibility, color,
or scheduling metadata does not make downstream callers rebuild struct
literals. This keeps the direct and compatibility backends on one stable
contract without adding a runtime abstraction or virtual dispatch to shipout.

A shaping request is one already-itemized run with explicit UTF-8 text, face,
direction, ISO 15924 script, optional BCP 47 language, OpenType features, and
variation coordinates. Output positions are in font units. Glyphs are stored
in visual order and refer to a separate cluster table in logical order; each
cluster retains its exact Unicode text and UTF-8 byte range. The adapter into
`aitex-page` converts those positions to final TeX scaled points. Missing
characters either fail or produce explicitly marked `.notdef` glyphs according
to the caller's stated policy.

## Subsetting and embedding

`FontSubsetter` keeps the implementation replaceable. The first implementation
uses Skera 0.6 with retained glyph IDs. Retention is deliberate: Skera's dense
source-to-subset closure map is currently private, while PDF emission must never
guess that mapping. The returned artifact nevertheless carries an explicit
source-to-subset map and a deterministic identity containing:

- source font SHA-256 and collection face;
- canonical requested source glyph and Unicode sets (the font digest and
  subsetter version determine their transitive closure);
- normalized variation coordinates;
- effective embedding permission;
- aiTeX policy version and subsetter implementation version.

OpenType `fsType` is enforced before preparation. Restricted and bitmap-only
outline embedding fail. A no-subsetting standalone font is returned whole; a
no-subsetting face inside a collection fails until aiTeX can extract that face
without violating the policy. Unsupported outlines and invalid glyph IDs fail
explicitly.

## Positioned PDF output

`aitex-page` is the stable `no_std` vocabulary between layout and output. It
contains final page geometry, classic glyph events, shaped glyph runs, and
their logical Unicode clusters. The shipout traversal captures classic glyphs,
rules, specials, and shaped runs in one order, and the PDF writer consumes the
result directly without reshaping text or reparsing DVI. A specialized DVI-only
branch avoids page-event allocation and per-glyph width lookups.

`aitex_driver::pdf::write_positioned` writes PDF 1.7 and embeds static
TrueType/glyf and CFF1 OpenType faces as Type 0/CID fonts. Horizontal runs use
`Identity-H`; already-shaped vertical runs use a separate `Identity-V` wrapper
over the same embedded program and carry `vmtx`/`VORG`/`VVAR` metrics through
`DW2`/`W2`. Source glyph IDs, subset glyph IDs, PDF character codes, and CIDs
remain distinct values with explicit maps. The current Skera adapter retains
source glyph IDs, making the mapping deterministic until its dense closure map
is available. Production serialization uses PDF 1.5 object and cross-reference
streams for both classic and positioned output; a classic-xref compatibility
mode retains ordinary indirect objects, and readable diagnostic output remains
PDF 1.4.

Every one-glyph semantic cluster receives a cluster-aware `ToUnicode` entry,
including multi-scalar ligatures. Right-to-left runs, many-to-many clusters,
ambiguous mappings, and explicit `.notdef` glyphs are wrapped in logical-order
`ActualText`. Glyph codes reused inside those contextual runs remain out of the
font-global `ToUnicode` map, so independent extractors cannot append a cluster
tail to its `ActualText`. Baseline runs use compact `TJ` arrays; runs with
general offsets use exact per-glyph text matrices.

Prepared subsets are cached by content digest, collection face, variation
coordinates, canonical glyph and Unicode sets, and subsetter version. The
process-wide cache is bounded to 128 entries and 64 MiB and retains safe
failures as well as successes. A filesystem-owning host may also supply a
persisted prepared subset; the driver verifies the full identity before use.

Concrete variable instances are materialized inside `aitex-fonts`: Skrifa draws
the selected `glyf` or CFF2 outlines at the same normalized coordinates used by
HarfRust, CFF cubics are converted with Kurbo's bounded-error `cu2qu` algorithm,
and the result is a static, unhinted TrueType subset with retained source GIDs.
The PDF writer therefore has one ordinary embedding contract for variable
TrueType and CFF2 inputs. For COLRv0, Skrifa resolves ordered solid layers and
CPAL colors into aiTeX-owned types, Skera retains the transitive layer-glyph
closure, and the PDF writer paints those outlines as vectors. Foreground layers
inherit the surrounding text color; library callers may select a palette, while
the CLI currently uses palette zero. `ActualText` preserves the logical base
character even though several layer glyphs are painted. Monochrome runs retain
the compact `TJ` path and do not pay per-glyph color lookups. Partial alpha,
COLRv1 gradients/compositing, SVG, sbix, and CBDT still fail explicitly rather
than degrading silently.
The hybrid writer accepts classic material
and native glyph runs through the shared page contract; it uses separate PDF
resource namespaces so the two cannot alias.

The page vocabulary retains BCP 47 language and an aiTeX-owned semantic role
on each shaped run. `write_positioned_with_semantics` can opt into MCIDs,
standard structure elements, a parent tree, document/run language, and
`MarkInfo` without importing PDF concepts into layout. Poppler recognizes the
fixture as tagged and extracts its vertical CJK in logical order. This is an
executable accessibility foundation, not a PDF/UA claim: artifacts, alternate
text, annotations, metadata, reading-order policy, and independent conformance
validation remain required.

`math_constants` likewise parses the initial OpenType MATH contract—script
scales, minimum display sizes, axis height, and fraction/radical rule
thicknesses—into aiTeX-owned values. A Unicode math-list builder still belongs
upstream of shaping and PDF output.

## Open-source ecosystem

The implementation intentionally participates in the existing Rust font
ecosystem:

- [Fontations](https://github.com/googlefonts/fontations): `read-fonts` and
  `skrifa` provide OpenType parsing, metadata, outlines, and the intended color
  paint traversal.
- [HarfRust](https://github.com/harfbuzz/harfrust): shapes itemized Unicode runs
  using the Fontations font model.
- [Skera](https://github.com/googlefonts/fontations/tree/main/skera): performs
  deterministic OpenType subsetting behind `FontSubsetter`.
- [Kurbo](https://github.com/linebender/kurbo): applies the established `cu2qu`
  cubic-to-quadratic conversion when a CFF2 instance becomes static TrueType.
- [ICU4X](https://github.com/unicode-org/icu4x): supplies Script properties and
  UAX #14 line segmentation.
- [`unicode-bidi`](https://github.com/servo/unicode-bidi): resolves bidi levels
  and visual runs while aiTeX retains logical cluster text for extraction.
- [`fontdb`](https://github.com/RazrFalcon/fontdb): indexes platform and bundled
  fonts. It does not choose fallback order or decide coverage; aiTeX does.

When real TeX documents expose a shared defect or missing capability, the
preferred response is a minimized fixture, an upstream issue, and—where we can
make it robust—a patch and differential test upstream. Dense mapping access in
Skera is the first known API opportunity. aiTeX keeps adapters around the
pre-1.0 APIs, but that is compatibility hygiene rather than an intention to
fork the ecosystem.

The locked tests use small SIL OFL subsets of
[Noto](https://github.com/notofonts/noto-fonts) and
[Amiri](https://github.com/aliftype/amiri), plus a variable-font subset of
[Junicode](https://github.com/psb1558/Junicode-font) and a CFF subset of
[Source Serif](https://github.com/adobe-fonts/source-serif), with source and
output hashes recorded beside the fixtures.

## Experimental CLI integration

`aitex --unicode document.tex` reads the main source and generated control
files as UTF-8. Raw scalar runs are shaped automatically; no input package is
required. The same decoding applies at included `.tex`, `.bbl`, and other TeX
file boundaries. A redundant `inputenc` `utf8` option is accepted as a no-op;
legacy encoding options are an explicit error because mixing a byte decoder
with already-decoded native scalars is ambiguous. Supplying `--font` promotes
ordinary ASCII text to the same native path, while classic math remains
classic. CLI and `.aitex.toml` expose font, font paths, BCP 47 language,
OpenType feature tags, variable coordinates, and `auto`/`ltr`/`rtl`/`ttb`/`btt`
direction. The corresponding engine primitives
are `\aitexnativefont`, `\aitexfont`, `\aitexlanguage`, `\aitexfeatures`,
`\aitexvariations`, `\aitexdirection`, and `\aitextext`; selector assignments
obey TeX group scope.

Project font directories are scanned recursively in lexical path order before
ambient system fonts. A requested family that does not exist is an error;
coverage fallback is deterministic after that preferred family. System-font
indexing is lazy and occurs only when the project set cannot resolve or cover a
run. Selected font programs enter the output-cache manifest, and normalized
user options plus font-path order enter snapshot and output identity. Variable
coordinates are used both when shaping and when constructing the embedded
static instance.

ICU4X itemizes scripts and supplies UAX #14 opportunities,
`unicode-bidi` resolves horizontal embedding levels, and the line breaker
applies visual reordering separately to each line it actually chooses. Spaces
inside ordinary native prose remain in the semantic span through bidi
resolution, then become normal TeX breakable glue.

The compatibility boundary is intentionally smaller than XeTeX or LuaTeX.
Under `--unicode`, aiTeX provides common
[fontspec](https://ctan.org/pkg/fontspec) commands (`\setmainfont`,
`\setsansfont`, `\setmonofont`, `\fontspec`, `\newfontfamily`,
`\newfontface`, `\defaultfontfeatures`, and `\addfontfeatures`) and the common
language selectors from [babel](https://ctan.org/pkg/babel) and
[polyglossia](https://ctan.org/pkg/polyglossia). Native tags such as `+liga`,
`-kern`, and `ss01=1` are accepted directly, along with `Ligatures=Common`,
`Ligatures=NoCommon`, `Kerning=On/Off`, the common `Numbers` choices, and
`Letters=SmallCaps`. Other package options produce an explicit error rather
than a plausible but incorrect substitution. `babel` remains the real package
and retains responsibility for TeX hyphenation and locale behavior; aiTeX
adds its selected language to shaping.

The engine stores only a host callback and a compact native-run node; neither
the font database nor any upstream font/Unicode type is serialized. At shipout,
one traversal emits canonical classic and native positioned events while a
specialized sink also retains exact private DVI as an in-memory oracle. The
hybrid PDF writer preserves classic text, math, specials, links, and graphics
while emitting native fonts directly. The production PDF route consumes those
events without temporary-DVI I/O or a DVI parse; an explicit developer mode
compares them exactly with the independently parsed oracle.

The ordinary fixture now exercises the lower PDF contract's already-shaped
vertical runs through `\aitexdirection{ttb}`. Full vertical paragraph
composition remains later layout work. COLRv1/SVG/bitmap color
painting, native-special graphics-state bridging, a Unicode/OpenType math-list
builder, full PDF/UA semantics, and still broader language/viewer fixtures also
remain.
Locked Arabic, Hebrew,
Devanagari, CJK, Thai, and Lao fixtures cover the first multilingual envelope.
Keeping those concerns out of the font primitive is important:
HarfRust shapes an itemized run, while TeX layout decides which runs exist and
where lines and pages break.
