//! All I/O the engine performs, behind one trait.
//!
//! This is docs/history/PLAN.md decision 1, and it is the only thing keeping wasm32 and
//! on-demand package fetch reachable. The engine never touches `std::fs` or the
//! network directly: retrofitting that later would mean editing every call
//! site, which is precisely the cost we are avoiding by paying it now.
//!
//! It is also why the engine crate can hold the zero-dependency line — the
//! interesting implementations (filesystem, network, bundle cache) live in
//! crates that are allowed dependencies.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// What the engine is asking for. Distinguished because resolution rules differ
/// per kind — kpathsea searches different trees for fonts than for sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// `.tex`, `.sty`, `.cls`, `.def` — anything TeX reads as source.
    Source,
    /// `.tfm` font metrics.
    FontMetrics,
    /// `.fmt` dumped format.
    Format,
    /// Hyphenation patterns and other data.
    Data,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoError {
    NotFound(String),
    /// Access refused — an absolute path when `openin_any` forbids it, or a
    /// traversal attempt. TeX processes hostile input; this is not paranoia.
    Denied(String),
    Other(String),
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IoError::NotFound(n) => write!(f, "file not found: {n}"),
            IoError::Denied(n) => write!(f, "access denied: {n}"),
            IoError::Other(m) => write!(f, "io error: {m}"),
        }
    }
}

/// Everything outside the engine.
///
/// Deliberately synchronous and byte-oriented: TeX's execution model is
/// strictly sequential, so there is nothing for an async interface to overlap.
/// A fetching implementation blocks; that is honest about what is happening.
pub trait World {
    /// Resolve and read a file. Returning [`IoError::NotFound`] is normal
    /// control flow — TeX probes for optional files constantly.
    fn read(&mut self, name: &str, kind: FileKind) -> Result<Vec<u8>, IoError>;

    /// Does this exist, without paying to read it? Used by `\openin` and
    /// `\IfFileExists`.
    fn exists(&mut self, name: &str, kind: FileKind) -> bool {
        self.read(name, kind).is_ok()
    }

    /// Write an output file (`.log`, `.aux`, `.dvi`).
    fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), IoError>;

    /// Terminal/log output.
    fn print(&mut self, text: &str);

    /// A seed for the engine's pseudo-random generator.
    ///
    /// Entropy is I/O, so it arrives here rather than being reached for from
    /// inside the engine -- the same rule as [`now`](World::now), and for the
    /// same two reasons. `aitex-engine` holds the zero-dependency line, and an
    /// ambient `getrandom` call is exactly what makes a wasm32 build
    /// impossible to produce later.
    ///
    /// `None` means no entropy is available, and that is a normal answer, not
    /// a failure: the engine then runs from a fixed seed and every draw is
    /// reproducible. "Same input, byte-identical DVI" is a stated property of
    /// this port, and it is [`MemoryWorld`]'s answer so that tests and
    /// differential comparisons cannot become flaky.
    fn random_seed(&self) -> Option<i64> {
        None
    }

    /// Seconds since the Unix epoch, for `\year`/`\month`/`\day`/`\time`.
    ///
    /// Returns `None` when no clock is available — on wasm32, or when building
    /// reproducibly. TeX must still start, so callers substitute a fixed date
    /// rather than failing. This is deliberately not `std::time`: that is
    /// exactly the kind of ambient dependency the trait exists to exclude.
    fn now(&self) -> Option<i64> {
        None
    }
}

/// Read-only file probing, for the gullet.
///
/// `\pdffilesize` is *expandable*, so it has to be answerable during expansion
/// — inside `\expanded`, inside an `\edef` body — where the stomach, which owns
/// the [`World`], is not reachable. expl3's `\__file_full_name:n` is built on
/// exactly that: `\exp_args:Ne \__file_full_name_auxi:nn {\__file_size:n {#1}}`
/// expects a *number* to come back out of the expansion, and gets macro text
/// instead if the gullet cannot answer.
///
/// Rather than share ownership of the world between stomach and gullet, the
/// gullet takes its own probe. The query is stateless — a name in, a size out —
/// so a second read-only handle costs nothing and keeps the borrow graph a
/// tree instead of a cycle.
pub trait FileProbe {
    /// The file's size in bytes, or `None` if it does not exist. Absence is
    /// normal control flow: it is how LaTeX asks whether a file is there.
    fn size(&mut self, name: &str) -> Option<usize>;
}

/// Any world can serve as a probe. Implementations that can stat a file more
/// cheaply than reading it should override this.
impl<W: World> FileProbe for W {
    fn size(&mut self, name: &str) -> Option<usize> {
        self.read(name, FileKind::Source).ok().map(|b| b.len())
    }
}

/// An in-memory [`World`]. The engine's test substrate, and the seed of the
/// wasm implementation — no filesystem, no clock, fully deterministic.
#[derive(Debug, Default)]
pub struct MemoryWorld {
    files: Vec<(String, Vec<u8>)>,
    pub written: Vec<(String, Vec<u8>)>,
    pub output: String,
    pub fixed_time: Option<i64>,
}

impl MemoryWorld {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_file(mut self, name: &str, contents: impl AsRef<[u8]>) -> Self {
        self.files
            .push((name.to_owned(), contents.as_ref().to_vec()));
        self
    }

    pub fn written_file(&self, name: &str) -> Option<&[u8]> {
        self.written
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
    }
}

impl World for MemoryWorld {
    fn read(&mut self, name: &str, _kind: FileKind) -> Result<Vec<u8>, IoError> {
        // TeX asks for `foo` meaning `foo.tex`; accept either spelling.
        self.files
            .iter()
            .find(|(n, _)| n == name)
            .or_else(|| {
                self.files
                    .iter()
                    .find(|(n, _)| n.strip_suffix(".tex").is_some_and(|stem| stem == name))
            })
            .map(|(_, b)| b.clone())
            .ok_or_else(|| IoError::NotFound(name.to_owned()))
    }

    fn write(&mut self, name: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.written.push((name.to_owned(), bytes.to_vec()));
        Ok(())
    }

    fn print(&mut self, text: &str) {
        self.output.push_str(text);
    }

    fn now(&self) -> Option<i64> {
        self.fixed_time
    }
}

/// Boxed [`World`], for callers that pick an implementation at runtime.
pub type BoxedWorld = Box<dyn World>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_world_round_trips() {
        let mut w = MemoryWorld::new().with_file("a.tex", b"hello");
        assert_eq!(
            w.read("a.tex", FileKind::Source).expect("present"),
            b"hello"
        );
        assert!(w.exists("a.tex", FileKind::Source));
    }

    #[test]
    fn extension_is_inferred_like_tex_does() {
        let mut w = MemoryWorld::new().with_file("chapter.tex", b"x");
        assert!(
            w.read("chapter", FileKind::Source).is_ok(),
            "\\input{{chapter}} must find chapter.tex"
        );
    }

    #[test]
    fn missing_file_is_ordinary_control_flow() {
        let mut w = MemoryWorld::new();
        assert_eq!(
            w.read("nope.tex", FileKind::Source).unwrap_err(),
            IoError::NotFound("nope.tex".into())
        );
        assert!(!w.exists("nope.tex", FileKind::Source));
    }

    #[test]
    fn writes_are_captured_not_performed() {
        let mut w = MemoryWorld::new();
        w.write("out.dvi", b"\xf7\x02").expect("write");
        assert_eq!(w.written_file("out.dvi"), Some(&b"\xf7\x02"[..]));
    }

    #[test]
    fn clock_is_absent_by_default() {
        // wasm32 and reproducible builds both land here; the engine must cope.
        assert_eq!(MemoryWorld::new().now(), None);
        let w = MemoryWorld {
            fixed_time: Some(1_451_606_400),
            ..Default::default()
        };
        assert_eq!(w.now(), Some(1_451_606_400));
    }
}
