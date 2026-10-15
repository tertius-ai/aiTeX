# Locked-corpus document-class frequency

This report measures literal `\documentclass` use in the 191-root locked holdout. It determines what P2 should prewarm; it is not a claim about the global LaTeX population.

The scanner classified 191 of 191 roots; 0 had no safely parseable literal command.

## Classes

| Class | Roots | Share | Most frequent option sets |
|---|---:|---:|---|
| `article` | 75 | 39.3% | `11pt` (28), `(none)` (17), `12pt` (5), `letterpaper` (4) |
| `amsart` | 27 | 14.1% | `(none)` (3), `11pt` (3), `11pt,reqno` (3), `10pt` (2) |
| `revtex4-2` | 16 | 8.4% | `aps,pra,10pt,twocolumn,floatfix,superscriptaddress,longbibliography` (1), `aps,pra,reprint,nofootinbib,longbibliography` (1), `aps,pra,twocolumn,superscriptaddress,showpacs,nofootinbib` (1), `aps,prd,preprint,superscriptaddress,nofootinbib` (1) |
| `IEEEtran` | 12 | 6.3% | `conference` (7), `conference,10pt` (2), `lettersize,journal` (2), `10pt,journal,compsoc` (1) |
| `acmart` | 8 | 4.2% | `sigconf,nonacm` (4), `sigconf` (3), `sigconf,screen` (1) |
| `sn-jnl` | 7 | 3.7% | `pdflatex,sn-mathphys-num` (4), `(none)` (1), `pdflatex,sn-basic` (1), `pdflatex,sn-mathphys,Numbered` (1) |
| `elsarticle` | 6 | 3.1% | `final` (1), `final,1p,times` (1), `preprint,11pt,authoryear` (1), `preprint,12pt` (1) |
| `llncs` | 5 | 2.6% | `runningheads` (5) |
| `aastex701` | 3 | 1.6% | `twocolumn` (2), `twocolumn,twocolappendix,resetfootnote` (1) |
| `ieeeconf` | 3 | 1.6% | `letterpaper,10pt,conference` (2), `conference` (1) |
| `revtex4-1` | 3 | 1.6% | `aps,letterpaper,twocolumn,preprintnumbers,floatfix,superscriptaddress` (1), `aps,pra,reprint,superscriptaddress,floatfix` (1), `aps,prl,twocolumn,superscriptaddress,nofootinbib` (1) |
| `aa` | 2 | 1.0% | `(none)` (1), `traditabstract` (1) |
| `aastex63` | 1 | 0.5% | `twocolumn` (1) |
| `aastex7` | 1 | 0.5% | `trackchanges,twocolumn,resetfootnote` (1) |
| `amsproc` | 1 | 0.5% | `12pt` (1) |
| `atlasdoc` | 1 | 0.5% | `PAPER,atlasdraft=false,cernpreprint,UKenglish,texmf,orcidlogo` (1) |
| `ceurart` | 1 | 0.5% | `(none)` (1) |
| `dobottechreport` | 1 | 0.5% | `10pt,logo` (1) |
| `epl2` | 1 | 0.5% | `doublecol` (1) |
| `hepthesis` | 1 | 0.5% | `hyperpdf,bindnopdf` (1) |
| `imsart` | 1 | 0.5% | `aos` (1) |
| `interact` | 1 | 0.5% | `(none)` (1) |
| `jfm` | 1 | 0.5% | `(none)` (1) |
| `lmcs` | 1 | 0.5% | `(none)` (1) |
| `mjsartcl` | 1 | 0.5% | `(none)` (1) |
| `mnras` | 1 | 0.5% | `fleqn,usenatbib` (1) |
| `osajnl` | 1 | 0.5% | `9pt,twocolumn,twoside` (1) |
| `packages/icmc` | 1 | 0.5% | `mestrado,pre-defesa` (1) |
| `pasj02` | 1 | 0.5% | `(none)` (1) |
| `quantumarticle` | 1 | 0.5% | `a4paper,11pt` (1) |
| `rasti` | 1 | 0.5% | `fleqn,usenatbib` (1) |
| `scrartcl` | 1 | 0.5% | `11pt,a4paper,DIV=12,numbers=noenddot,sfdefaults=off,enabledeprecatedfontcommands` (1) |
| `socg-lipics-v2021` | 1 | 0.5% | `a4paper,UKenglish,cleveref,autoref,thm-restate,nolineno` (1) |
| `spml_template` | 1 | 0.5% | `(none)` (1) |
| `style/iromlab` | 1 | 0.5% | `(none)` (1) |
| `tibop-article` | 1 | 0.5% | `11pt` (1) |

## Prewarm implication

Snapshots are built lazily for every safe class/option prefix. An optional prewarm policy should start with the leading exact class/option combinations in the versioned JSON artifact, while retaining lazy fallback for the long tail and local classes.

Frequency alone is not an admission rule. In the P2 experiment the shared snapshot was roughly 18--20 MiB and cost about 60--80 ms to write; representative class execution ranged from about 12 ms for `article` to 270 ms for `acmart`. Class snapshot admission is disabled in v0.2 because a host-side lexical boundary is not a semantic proof. `aitex cache warm` now converges and publishes only the exact requested DVI/PDF output. A future engine-observed class prewarm policy should rank combinations by observed frequency times measured time saved, not frequency alone.

The machine-readable source is [`../qualification/holdout-200.classes.json`](../qualification/holdout-200.classes.json). Regenerate both files with `python3 tools/class_frequency.py`.
