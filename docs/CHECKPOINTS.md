# Live document checkpoints

> **v0.3 status:** complete builds retain one process-local, engine-observed
> `\document` boundary for successive TeX passes. Persistent project snapshots
> and watch-mode body selection remain disabled: they additionally require a
> validated mapping from the live source cursor to a separately supplied body,
> and a host lexer cannot prove `\begin{document}` semantics.

P3 uses a deliberately conservative engine boundary. `Fast` can pause
after a real paragraph has completely run `end_graf`, serialize the live job,
replace an unchanged-prefix source suffix, and continue. This is distinct from
a format or project-preamble snapshot: those start a new job and intentionally
discard the run that created them, while a body checkpoint preserves it.

## Complete-build document boundary

`Fast::run_until_document_boundary` watches execution rather than searching
source text. It stops only after the macro frame for LaTeX's canonical
`\document` has actually finished and the complete engine state passes the
same conservative safety checks as a body checkpoint, with the completed-
paragraph requirement relaxed because the first body paragraph has not begun.
The stop is latched across nested expansion, so a trailing expandable command
such as `\ignorespaces` cannot make the engine open or otherwise observe a
body input before returning control to the host. Literal `\begin{document}`
text that never executes cannot create a checkpoint.

During `aitex build`, the host retains at most one callback-free clone of that
live state. A later TeX pass may clone it only when the complete source and
compiler configuration identity match, every file observed before the
boundary still resolves to identical bytes, and the complete job clock tuple
is unchanged. Missing files are dependencies too: if `main.aux` was absent and
then appears, the checkpoint misses. The same generic validation covers
dependent `.aux`, `.bbl`, `.ind`, `.gls`, `.acr`, and `.nls` inputs; a semantic
tool output first read after the boundary remains a fresh body input and does
not invalidate state that never observed it.

Fresh file, font, metadata, Unicode-shaping, write, and page callbacks are
attached to each clone. Any completed file prefix retained by the engine is
replayed through the new writer. An ordinary page sink is allowed to be
installed at the document boundary only while zero pages have escaped. A
changed dependency or clock, unsafe engine state, preamble error, host write
failure, or unobserved canonical macro is an optimization miss and the build
runs a complete pass from the pristine format.

## Safe boundary

The checkpoint request is tested once per main-loop command, outside the token
hot path. A site is eligible only when all of the following hold:

- the nearest live file frame is the outer source fragment being checkpointed;
  fully serialized macro/token-list frames may remain above it, but an active
  included-file frame may not masquerade as provenance for the outer source;
- a new real paragraph has completed and the typesetter is back in outer
  vertical mode;
- no conditional, alignment, box, insertion, math list, equation number,
  discretionary, prefix, pending assignment, accent, leader, or output routine
  is open; an ordinary lexical group may span the boundary because its complete
  save stack is serialized (LaTeX's document environment requires this);
- no token is peeked, backed up, or partly scanned;
- the complete page-builder contribution state is engine-owned;
- no page has escaped through an ordinary streaming page sink. A
  checkpoint-aware sink is eligible only when the host captures the matching
  resumable DVI writer prefix alongside the engine payload.

These refusals are policy, not missing error handling. An ineligible document
continues normally and produces no checkpoint. Future work may widen a
boundary only with a differential proof.

## Payload and provenance

The payload has its own magic header and the same source-derived schema
fingerprint as formats. It serializes the complete `Fast` graph, including
registers, definitions, catcodes, RNG, open auxiliary buffers, page-builder
state, marks, insertions, deferred writes, errors, and the live input stack.
Per-job values deliberately omitted from reusable format dumps—such as frozen
magnification, terminal/log columns, `everyjob` progress, host policy,
dependency-observation flags, and retained page metadata—travel in a separate
checkpoint runtime envelope. When pages stream out of the engine, the host
stores the corresponding `DviWriter` and font/page metadata with each entry.

Each site records the TeX-visible source name, engine file slot, Unicode-scalar
and UTF-8 byte offsets, physical line, completed-paragraph serial, and shipped
page count. Resume requires the replacement source to match exactly through
both saved offsets. A changed prefix, corrupt payload, incompatible engine, or
invalid UTF-8 boundary is a hard miss.

Host callbacks are never serialized. After resume the host must reattach the
file resolver, metadata resolver, TFM loader, file writer, and output sink
before continuing. Completed file bodies are then replayed through the writer,
and a checkpoint-aware page sink resumes from its matching DVI prefix. This is
the same callback-ownership rule as format restore, with the live job's side
effects retained rather than cleared.

## Permanent correctness gate

Engine tests compare continuous execution with checkpoint/resume for an
unchanged source and for an edited suffix, including exact final serialized
state. They also cover prefix rejection, repeated checkpoint collection, open
groups, streaming pages, buffered auxiliary writes, and already-emitted files.

`AITEX_INCREMENTAL=1 aitex watch --stdin ...` enables host integration for document
bodies of at least 8 KiB. It requests distributed paragraph boundaries across
the body, denser tail boundaries, and boundaries around literal `\section`,
`\chapter`, `\part`, `\input`, and `\include` sites. Every request remains
subject to the engine's safe-boundary proof. A boundary is also marked as a
page boundary when the matching DVI prefix has shipped another page.

The host keeps at most 12 entries within a 64 MiB checkpoint-and-DVI-prefix
accounting budget. Payloads use content-defined chunks, SHA-256 identity and
deduplication, and independent LZ4 compression. At most three immutable
decoded payloads remain hot, and one late checkpoint is prepared as a live
engine between requests. The prepared engine is charged at its serialized
payload size for admission; keeping exactly one avoids multiplying the engine
heap by the checkpoint count.

A root-source edit selects the nearest checkpoint whose source prefix is
byte-for-byte unchanged. Each entry records exactly the dependencies observed
before that boundary. If an included file changes, the selector may therefore
resume from the last independently valid boundary before the file was first
read; it never reuses state that already observed the changed subtree. A
changed project-preamble identity, relevant TeX clock, corrupt payload, edit
before the first retained checkpoint, or lack of an independently valid entry
is an explicit miss. A resumed edit consumes rather than clones its selected
DVI prefix, retains earlier distributed entries, and avoids foreground
checkpoint serialization; a later full pass replenishes the tail.

For whitespace and comment-only diffs, the old index supplies candidate
downstream boundaries. Early stop is allowed only when the complete serialized
semantic state digest and exact DVI-writer-prefix digest match at the mapped
boundary, all downstream dependencies and the TeX-visible clock still match,
and every saved final/log/auxiliary artifact is still exact. Otherwise
execution continues to document end. This lexical prefilter is only an
optimization; the full state and output identities are the authority.

After a converged successful frame, the index records SHA-256 identities
for the final DVI/PDF, log, and every auxiliary file written by TeX. An exact
no-op request may return those existing artifacts without reconstructing an
engine only when the source, project key, dependencies, TeX-visible clock,
output path, and every artifact digest still match. A missing or externally
changed artifact rejects this shortcut and resumes from the ordinary body
checkpoint, which repairs the output. The conservative fixpoint allow-list is
`.aux`, `.toc`, `.lof`, `.lot`, `.out`, `.bcf`, `.idx`, `.glo`, `.glsdefs`,
`.acn`, `.nlo`, `.nav`, and `.snm`; arbitrary `\write` products are never
treated as evidence that another pass is unnecessary.

The PDF route has three separately proven reuse levels. An unchanged DVI and
resource set reuses the complete final object graph. A changed document reuses
immutable prepared font programs and resolved font resources only while every
path and content dependency remains valid. Finally, a 32 MiB page cache uses
hash-indexed lookup and shared-key FIFO eviction to reuse an exact content
stream only when page geometry, every event and special, font IDs and advances,
and destinations match. It verifies the complete key bytes rather than
accepting a digest match. Pages with annotations, outlines, named objects,
graphics/XObjects, resource fragments, or other cross-page effects are
conservatively ineligible and render normally.

`tools/incremental_check.py` exercises no-op, reconvergent, body-edit, changed
output, include-subtree, and preamble cases against byte-exact clean aiTeX DVI
and PDF rebuilds. `tools/incremental_adversarial.py` covers output routines,
floats, footnotes, marks, counters, global/cross-file definitions, references,
bibliography/index files, writes, generated inputs, graphics, randomness,
shell-escape refusal, and a very large paragraph. `tools/incremental_corpus.py`
qualifies every mechanically eligible locked P2 DVI root; 53/53 are exact and
three are below the 8 KiB admission threshold. `tools/scale_check.sh` retains
the independent 10,000-page DVI/PDF gate. The committed artifacts are
[`../qualification/holdout-200-p3.incremental.json`](../qualification/holdout-200-p3.incremental.json)
and [`../qualification/performance-p3.json`](../qualification/performance-p3.json).
