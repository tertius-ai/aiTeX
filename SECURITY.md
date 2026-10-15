# Security policy

## Supported versions

The latest 0.3.x source release and `main` are supported. Older tags do not
receive security fixes.

## Reporting a vulnerability

Please do not disclose a suspected vulnerability in a public issue. Use
[GitHub private vulnerability reporting](https://github.com/tertius-ai/aiTeX/security/advisories/new)
to send:

- the affected version or commit and platform;
- the smallest safe reproducer;
- expected and observed impact;
- whether untrusted TeX, bibliography, font, image, archive, cache, or model
  content is involved; and
- any proposed mitigation.

Do not include real API keys, private documents, or third-party material you
cannot redistribute. A maintainer will acknowledge the report, reproduce it,
coordinate a fix and disclosure, and credit the reporter if desired. This
volunteer project does not promise a fixed response-time SLA.

## Security boundary

aiTeX processes complex, potentially hostile input. Its page, font, Unicode,
and PDF/DVI crates forbid unsafe Rust. The engine's documented hot loop and a
narrow CLI boundary for Unix process handling and allocation metrics use
reviewed unsafe blocks. Font, image, cache, and model-response parsers use
explicit bounds and reject invalid state. Release qualification includes
malformed-format unit and integration tests, path and archive safety checks,
dependency policy, and isolated repair validation.

Important boundaries remain:

- aiTeX requires a host TeX Live/MacTeX tree and may invoke BibTeX, Biber,
  MakeIndex, MakeGlossaries, MuPDF, Ghostscript, Netpbm, or ImageMagick for
  document-requested work;
- shell-escape and arbitrary model shell/filesystem access are not part of the
  supported product;
- ordinary build, watch, pass, and lint commands make no model request;
- live explain/fix requests require explicit source-upload consent and are
  subject to the selected provider's security and retention policies;
- repair records can contain source context and provider output and should be
  protected like the document itself; and
- a successful PDF build is not a sandbox for unrelated host tools or a
  guarantee that an untrusted document is harmless to every downstream viewer.

See [docs/PRIVACY.md](docs/PRIVACY.md) for credentials, uploaded context, local
records, and provider behavior.
