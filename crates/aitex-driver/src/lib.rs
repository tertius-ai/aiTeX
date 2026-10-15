//! DVI → PDF.
//!
//! `docs/history/PLAN.md` decision 3 keeps PDF out of the engine: DVI is the differential
//! oracle every tool here compares against, and a driver that reads it cannot
//! change what the engine decided. It also makes `graphicx` and `hyperref`
//! *driver* work — they emit `\special`s, and the engine need not know what a
//! hyperlink is.
//!
//! Font parsing and preparation are delegated to `aitex-fonts`; stream
//! compression uses zlib-rs. Neither dependency enters the TeX engine.
#![forbid(unsafe_code)]

pub mod dvi;
pub mod fontmap;
pub mod pdf;
pub mod pk;
pub mod vf;
