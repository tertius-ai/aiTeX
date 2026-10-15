//! A TeX engine in pure Rust.
//!
//! **This crate's only dependencies are `serde` + `postcard`** — a deliberate,
//! reasoned exception to the zero-dep posture (docs/history/PLAN.md decision 8) for engine
//! state dump/restore (see [`checkpoint`]); both are pure Rust, no_std, and
//! wasm-clean. `tools/check_zero_deps.py` enforces that nothing else creeps in.
//! Anything heavier — OpenType, shaping, PDF — belongs in `aitex-fonts` or
//! `aitex-driver` instead.
//!
//! `no_std` + `alloc` so wasm32 stays reachable and no ambient filesystem or
//! clock can creep in: all I/O goes through [`World`] (decision 1).

#![no_std]

extern crate alloc;

pub mod align;
pub mod catcode;
pub mod counters;
pub mod dimen;
pub mod dvi;
pub mod fast;
pub mod fastcore;
pub mod fastset;
pub mod hyphenate;
pub mod intern;
pub mod linebreak;
pub mod macros;
pub mod math;
pub mod md5;
pub mod node;
pub mod page;
pub mod profile;
pub mod rand;
pub mod regex;
pub mod scaled;
pub mod shipout;
pub mod tfm;
pub mod token;
pub mod world;

/// Dependency-free final-page vocabulary shared with output backends.
pub use aitex_page as positioned_page;
pub use catcode::{CatCode, CatCodeTable};
pub use dimen::{Unit, make_dimen};
pub use dvi::{DviError, DviWriter, FontDef};
pub use macros::{MacroDef, MacroError, Param};
pub use node::{Glue, Node, TeXBox, hpack, vpack};
pub use page::{Decision, PageBuilder};
pub use scaled::{MAX_DIMEN, Scaled, UNITY};
pub use tfm::TfmFont;
pub use token::{Token, Tokenizer};
pub use world::{FileKind, IoError, MemoryWorld, World};
