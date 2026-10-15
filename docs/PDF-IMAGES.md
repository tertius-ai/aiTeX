# Lossless PDF image handling

aiTeX selects a deterministic, format-aware path for every graphics asset. The
production policy is lossless: JPEG DCT and compatible PNG IDAT streams are
copied without decoding, safe PDF pages and EPS operators remain vector forms,
and unsupported representations use a bounded renderer fallback. There is no
silent downsampling, JPEG recompression, profile removal, or opaque-background
flattening.

The native PNG path preserves 1/2/4/8/16-bit grayscale, 8/16-bit RGB, and
1/2/4/8-bit indexed samples with PDF predictor 15. A single exact PNG `tRNS`
sample or palette index becomes a PDF color-key mask. Partial or multi-entry
alpha is normalized into an RGB image plus an eight-bit soft mask; equivalent
soft masks are shared independently of the color planes. Grayscale and RGB
JPEGs retain their original DCT bytes. Source-identical images, imported PDF
forms, and supported EPS forms are content-addressed independently of crop,
scale, rotation, and filename.

Decoded-size arithmetic is checked and capped at 512 MiB before admission.
Unsupported or unsafe assets must find the bounded host fallback; otherwise
the CLI emits the source name and refuses to write a plausible PDF with a
missing figure. Deterministic failures are cached under the same 32 MiB,
256-entry hot-asset budget as successful conversions. Image work is serial today and thus
uses no competing worker pool; any future parallel conversion must use the
existing process-wide worker budget.

## Accounting and qualification

`AITEX_IMAGE_TRACE=1` emits one record per resolved use with source format,
original and retained bytes, selected route, cache reuse, elapsed microseconds,
and a conservative in-process transient-byte estimate. `tools/pdf_suite.py`
enables this for the graphics case and records the records beside PDF-stage
time and RSS. `tools/pdf_size.py` records every Image XObject's dimensions,
color space, bit depth, filters, stored/decoded bytes, and mask, plus imported
forms.

The generated matrix covers RGB, partial alpha, one-bit and eight-bit gray,
two-bit indexed color-key transparency, identical bytes under different names,
multiple crops/transforms, and a simple EPS. The broader focused PDF suite also
covers grayscale/RGB JPEG, opaque alpha, multipage PDF selection, simple and
procedure-heavy EPS, malformed inputs, Ghostscript, MuPDF, Poppler rendering,
and deterministic rebuilds.

On the two-page development matrix from 2026-09-04, aiTeX emitted six shared
image objects and one shared EPS form, versus fourteen image objects and four
forms from dvipdfmx. Classic-xref image payload was 494,407 bytes versus
950,376; the production PDF was 529,508 bytes versus 965,054 (0.549x). The
measured aiTeX PDF stage was 25.5 ms, of which 4.2 ms was assembly; recognizing
the simple EPS before host fallback removed an otherwise unnecessary roughly
95 ms `extractbb` launch on this machine. The
Poppler comparison passed at normalized MAE 0.000857 and 0.510% pixels outside
the 5% per-channel fuzz band. These are generated-fixture results, not claims
about a private publication.
