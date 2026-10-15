# Third-party notices

aiTeX source and native release bundles do not redistribute the LaTeX kernel,
TeX Live/MacTeX packages, fonts, or a preloaded format derived from them. The
installed executable creates a private `latex.afmt` cache from the user's local
TeX installation on first use. That cache is not part of aiTeX's distributed
artifacts.

TeX Live and MacTeX are collections of independently licensed packages. Their
own copying notices and the license statements shipped with each installed
package remain authoritative. In particular, the LaTeX and LaTeX3 kernels are
maintained by the LaTeX Project under the LaTeX Project Public License (LPPL),
version 1.3c or later. Upstream sources and license information are available
from <https://tug.org/texlive/> and <https://github.com/latex3/latex2e>.

The dependency inventory shipped with native bundles describes code linked
into the aiTeX executable. `Cargo.lock`, the generated SBOM, and the license
files distributed with those dependencies are the authoritative records for
that linked software.
