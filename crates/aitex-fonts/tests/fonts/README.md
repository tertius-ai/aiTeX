# Font test fixtures

These fixtures test aiTeX's shaping, subsetting, and color-paint boundaries.
Most are small modified subsets; the COLRv0 fixture is an unmodified upstream
font. They remain under the SIL Open Font License 1.1 in `OFL-1.1.txt`.

| Fixture | Upstream | Source SHA-256 | Output SHA-256 | Covered text |
| --- | --- | --- | --- | --- |
| `NotoSans-Latin-Test.ttf` | [Noto Sans](https://github.com/notofonts/noto-fonts) 2.005 | `9cb49a54e520423033f9727be2e53e4805a60656deb09c219740d8e5f3e033ac` | `7af90644da84abf2ed6df77f3f112cc84aafb0b8c93afb70aedcc8b5024e11bc` | Basic Latin and Latin-1 Supplement |
| `NotoSansHebrew-Test.ttf` | [Noto Sans Hebrew](https://github.com/notofonts/noto-fonts/tree/main/hinted/ttf/NotoSansHebrew) 3.000 | `a7fa16fffb27bedb060a0866267c29e9859aeb9c21cc33f5b3aaf6eb062eca85` | `4f86593591b738cd6dc94027ef88013d810f7ccb4a7b1d3357fbfb9c91245737` | `שָׁלוֹם עוֹלָם` |
| `NotoSansDevanagari-Test.ttf` | [Noto Sans Devanagari](https://github.com/notofonts/noto-fonts/tree/main/hinted/ttf/NotoSansDevanagari) 2.002 | `385e78e6359a9d88a0f243d53b1209d7548361ba2194e2b9ec779bcaa7e8949d` | `e842dc348ba75ca32d6b93b63c45e72a704ba8f011848f495a31bee2bcc8880c` | `नमस्ते भारत` |
| `Amiri-Arabic-Test.ttf` | [Amiri](https://github.com/aliftype/amiri) 1.000 | `b092096eb992aebe59084b0cb5cf0015ae82c7e76b0aac5e7d60ae5113af5f54` | `cc3233f310b85f2fc00647adfb34fc6e30fc360dde4c695eafe9146c76615a28` | `السَّلَامُ عَلَيْكُمْ` |
| `Junicode-Variable-Test.ttf` | [Junicode](https://github.com/psb1558/Junicode-font) 2.206 | `41285b753947e5197ffddacc205a1cf43835cdb15d4689d13ec9d43f2797f187` | `774bef7743bab85ff2cff811f019d9ce0415687870db9f2863c0ff522f170917` | `minimum variable width office` |
| `SourceSerif4-CFF-Test.otf` | [Source Serif](https://github.com/adobe-fonts/source-serif) 4.005 | `edf160d0d584deee8a3bb2c3371b2a7624ca63580fbe02c57c1f4c91e84d8787` | `aef0743735bf153286eda449ca9d3e5bd514b3e9cb7bba50c2238fef773cd407` | `office affine` |
| `SourceSerif4-CFF2-Variable-Test.otf` | [Source Serif](https://github.com/adobe-fonts/source-serif) 4.005 | `867b73c6a954a4a64616906d179f94572a748790a1d022ebeeff07f56ea0221a` | `b393c8effe81ab165fa91b4d79c1b963c5460f783bfd4b4c2dd6db28fecd098d` | `variable office` |
| `NotoSansThai-Test.ttf` | [Noto Sans Thai](https://github.com/notofonts/thai) 2.000 | `404ddfb5ed0aaa6b6ec8a85700d682978992062d67da93903967b56cbd9a4acc` | `7ab8c52fe5f55c42781da401e2593c693cf36d952a337becef5a8415671949ca` | `ภาษาไทย ยินดีต้อนรับ` |
| `NotoSansLao-Test.ttf` | [Noto Sans Lao](https://github.com/notofonts/lao) 2.001 | `4a64d40850990992913d4be44b2e87a93d52f76882c1f0c0755edcb12ec57aa2` | `1c297a0b946a9844eedef8ff1cbfb93accfbbe36122624edd4aeabc0d65eeb6b` | `ສະບາຍດີ ພາສາລາວ` |
| `NotoSansCJKsc-Test.otf` | [Noto Sans CJK](https://github.com/notofonts/noto-cjk) 2.004 | `2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b` | `be01c44b9bf642857b3e0b7acff2132b794b71f22d468056c8abdf42661381bf` | `日本語 中文测试 漢字 かな カナ` |
| `AmiriQuranColored-COLRv0-Test.ttf` | [Amiri Quran Colored](https://github.com/aliftype/amiri) 1.000 | `e29bf08e34703838a577f60b1a20f5d226e709e3a3088248fe40b28fcbec7ac7` | `e29bf08e34703838a577f60b1a20f5d226e709e3a3088248fe40b28fcbec7ac7` | `آ ٠`, COLRv0/CPAL solid layers |

The Latin fixture was refreshed with HarfBuzz 14.2.0 to retain U+0020–U+00FF,
all layout features, and all name records for ordinary product-document text.
The other original fixtures were generated with Skera 0.6.0, retaining all
default layout features, all scripts and languages, name IDs 0–14, and the
`.notdef` outline. The CFF fixture was generated with HarfBuzz 14.2.0 while
retaining all layout features, scripts, names, and the `.notdef` outline; aiTeX
then exercises it through Skera like every other OpenType fixture.
The CFF2 fixture preserves its `wght` and `opsz` variation axes and was made
from Source Serif's official CFF2 variable OTF with HarfBuzz 14.2.0. The Thai,
Lao, and CJK fixtures were generated with HarfBuzz 14.2.0 using the
same broad layout/name retention. Their source hashes name the exact upstream
program even though the locked test artifacts contain only the text above.

The COLRv0 fixture is the unmodified Amiri Quran Colored 1.000 font distributed
by TeX Live and upstream Amiri. Tests select only two mapped glyphs: `آ` has an
ordered foreground/red composition and `٠` uses a green palette color. aiTeX
expands that layer closure before Skera subsetting, so the generated PDF embeds
the small resulting subset rather than the whole fixture.
