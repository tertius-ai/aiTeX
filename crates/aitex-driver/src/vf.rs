//! Expansion of DVI virtual fonts into positioned physical-font operations.
//!
//! A VF character packet is a small DVI subroutine. TeX writes only the
//! virtual font to its DVI; every output driver is responsible for interpreting
//! the packet, including nested virtual fonts. Expanding into the driver's
//! existing absolute [`crate::dvi::Item`] representation keeps the PDF writer
//! unaware of that extra indirection.

use crate::dvi;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
struct LocalFont {
    name: String,
    scale: i64,
    design: i64,
    checksum: u32,
}

#[derive(Clone, Debug)]
struct VirtualFont {
    fonts: BTreeMap<u32, LocalFont>,
    packets: BTreeMap<u32, Vec<u8>>,
}

#[derive(Clone, Copy, Default)]
struct State {
    h: i64,
    v: i64,
    w: i64,
    x: i64,
    y: i64,
    z: i64,
}

#[derive(Debug)]
pub enum Error {
    Malformed(String),
    MissingMetrics(String),
    Recursion(String),
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Option<u8> {
        let value = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(value)
    }
    fn unsigned(&mut self, count: usize) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            value = value.checked_mul(256)?.checked_add(self.byte()? as u32)?;
        }
        Some(value)
    }
    fn signed(&mut self, count: usize) -> Option<i64> {
        let value = self.unsigned(count)?;
        let shift = 64usize.checked_sub(count.checked_mul(8)?)?;
        Some(((u64::from(value) << shift) as i64) >> shift)
    }
    fn bytes(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(count)?;
        let value = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(value)
    }
}

fn parse(bytes: &[u8]) -> Option<VirtualFont> {
    let mut input = Reader { bytes, at: 0 };
    if input.byte()? != 247 || input.byte()? != 202 {
        return None;
    }
    let comment = input.byte()? as usize;
    input.bytes(comment)?;
    input.unsigned(4)?; // checksum
    input.unsigned(4)?; // design size

    let mut fonts = BTreeMap::new();
    let mut packets = BTreeMap::new();
    loop {
        let command = input.byte()?;
        match command {
            0..=241 => {
                let length = command as usize;
                let code = input.byte()? as u32;
                input.unsigned(3)?; // TFM width, checked by VFtoVP/TeX
                packets.insert(code, input.bytes(length)?.to_vec());
            }
            242 => {
                let length = input.unsigned(4)? as usize;
                let code = input.unsigned(4)?;
                input.unsigned(4)?;
                packets.insert(code, input.bytes(length)?.to_vec());
            }
            243..=246 => {
                let number = input.unsigned((command - 242) as usize)?;
                let checksum = input.unsigned(4)?;
                let scale = input.unsigned(4)? as i64;
                let design_fix = input.unsigned(4)? as i64;
                let area = input.byte()? as usize;
                let name = input.byte()? as usize;
                let bytes = input.bytes(area.checked_add(name)?)?;
                let name = String::from_utf8_lossy(bytes).into_owned();
                if scale <= 0 || design_fix <= 0 {
                    return None;
                }
                fonts.insert(
                    number,
                    LocalFont {
                        name,
                        scale,
                        // VF stores a fix_word count of points; DVI stores sp.
                        design: design_fix / 16,
                        checksum,
                    },
                );
            }
            248 => break,
            _ => return None,
        }
    }
    Some(VirtualFont { fonts, packets })
}

fn scaled(value: i64, at: i64) -> Option<i64> {
    let product = i128::from(value).checked_mul(i128::from(at))?;
    i64::try_from(product / 1_048_576).ok()
}

struct Expander<V, W, O> {
    resolve_vf: V,
    width: W,
    glyph_offset: O,
    cache: BTreeMap<String, Option<VirtualFont>>,
    font_numbers: BTreeMap<(String, i64), u32>,
    fonts: BTreeMap<u32, dvi::FontDef>,
}

impl<V, W, O> Expander<V, W, O>
where
    V: FnMut(&str) -> Option<Vec<u8>>,
    W: FnMut(&str, i64, u32) -> Option<i64>,
    O: FnMut(&str, i64, u32) -> Option<i64>,
{
    fn virtual_font(&mut self, name: &str) -> Result<Option<VirtualFont>, Error> {
        if !self.cache.contains_key(name) {
            let value =
                match (self.resolve_vf)(name) {
                    Some(bytes) => Some(parse(&bytes).ok_or_else(|| {
                        Error::Malformed(format!("malformed virtual font {name}"))
                    })?),
                    None => None,
                };
            self.cache.insert(name.to_string(), value);
        }
        Ok(self.cache.get(name).cloned().flatten())
    }

    fn physical_font(&mut self, def: &dvi::FontDef) -> u32 {
        // A DVI may name a physical face directly and also reach the same face
        // through a VF packet. The VF carries its own checksum/design metadata,
        // which is often not byte-for-byte identical to the root DVI fontdef,
        // but name plus scaled size is the identity used to load its TFM and
        // PDF program. Keeping two IDs here makes the PDF resolver consume the
        // shared resource for one ID and leave the other unrendered.
        let key = (def.name.clone(), def.at);
        if let Some(&number) = self.font_numbers.get(&key) {
            return number;
        }
        let number = self.fonts.len() as u32;
        self.font_numbers.insert(key, number);
        self.fonts.insert(number, def.clone());
        number
    }

    fn emit_char(
        &mut self,
        def: &dvi::FontDef,
        code: u32,
        h: i64,
        v: i64,
        depth: usize,
        out: &mut Vec<dvi::Item>,
    ) -> Result<(), Error> {
        if depth > 32 {
            return Err(Error::Recursion(def.name.clone()));
        }
        let Some(vf) = self.virtual_font(&def.name)? else {
            let font = self.physical_font(def);
            let advance = (self.width)(&def.name, def.at, code).unwrap_or(0);
            // pdfTeX's dynamically created `\letterspacefont` is a virtual
            // font even though no `.vf` exists on disk. Its packet moves by
            // half the tracking amount, draws the physical glyph, then moves
            // by the other half. The outer DVI advance already contains the
            // complete tracking width; only the painted origin belongs here.
            let glyph_offset = (self.glyph_offset)(&def.name, def.at, code).unwrap_or(0);
            out.push(dvi::Item::ClassicGlyph {
                font: aitex_page::FontInstanceId(font),
                code,
                origin_x: aitex_page::Sp(h.saturating_add(glyph_offset)),
                origin_y: aitex_page::Sp(v),
                advance: aitex_page::Sp(advance),
            });
            return Ok(());
        };
        let Some(packet) = vf.packets.get(&code) else {
            return Ok(()); // a missing VF character is blank, as in DVI drivers
        };
        self.run_packet(&vf, packet, def.at, h, v, depth + 1, out)
    }

    fn run_packet(
        &mut self,
        vf: &VirtualFont,
        packet: &[u8],
        parent_at: i64,
        h: i64,
        v: i64,
        depth: usize,
        out: &mut Vec<dvi::Item>,
    ) -> Result<(), Error> {
        let mut input = Reader {
            bytes: packet,
            at: 0,
        };
        let mut state = State {
            h,
            v,
            ..State::default()
        };
        let mut stack = Vec::new();
        let mut current = vf.fonts.keys().next().copied();
        while input.at < packet.len() {
            let op = input
                .byte()
                .ok_or_else(|| Error::Malformed("truncated VF packet".into()))?;
            match op {
                0..=127 | 128..=131 | 133..=136 => {
                    let (code, advance) = match op {
                        0..=127 => (u32::from(op), true),
                        128..=131 => (
                            input
                                .unsigned((op - 127) as usize)
                                .ok_or_else(|| Error::Malformed("truncated VF set".into()))?,
                            true,
                        ),
                        133..=136 => (
                            input
                                .unsigned((op - 132) as usize)
                                .ok_or_else(|| Error::Malformed("truncated VF put".into()))?,
                            false,
                        ),
                        _ => unreachable!(),
                    };
                    let local = current
                        .and_then(|number| vf.fonts.get(&number))
                        .ok_or_else(|| Error::Malformed("VF character has no font".into()))?;
                    let at = scaled(local.scale, parent_at)
                        .ok_or_else(|| Error::Malformed("VF font scale overflow".into()))?;
                    let def = dvi::FontDef {
                        name: local.name.clone(),
                        at,
                        design: local.design,
                        checksum: local.checksum,
                    };
                    self.emit_char(&def, code, state.h, state.v, depth, out)?;
                    if advance {
                        state.h = state
                            .h
                            .checked_add((self.width)(&def.name, at, code).ok_or_else(|| {
                                Error::MissingMetrics(format!("{} at {}sp", def.name, at))
                            })?)
                            .ok_or_else(|| Error::Malformed("VF position overflow".into()))?;
                    }
                }
                132 | 137 => {
                    let height = input
                        .signed(4)
                        .and_then(|value| scaled(value, parent_at))
                        .ok_or_else(|| Error::Malformed("bad VF rule height".into()))?;
                    let width = input
                        .signed(4)
                        .and_then(|value| scaled(value, parent_at))
                        .ok_or_else(|| Error::Malformed("bad VF rule width".into()))?;
                    out.push(dvi::Item::Rule {
                        x: aitex_page::Sp(state.h),
                        baseline_y: aitex_page::Sp(state.v),
                        width: aitex_page::Sp(width),
                        height: aitex_page::Sp(height),
                    });
                    if op == 132 {
                        state.h += width;
                    }
                }
                138 => {}
                141 => stack.push(state),
                142 => {
                    state = stack
                        .pop()
                        .ok_or_else(|| Error::Malformed("VF pop without push".into()))?;
                }
                143..=146 => {
                    let value = input
                        .signed((op - 142) as usize)
                        .and_then(|value| scaled(value, parent_at))
                        .ok_or_else(|| Error::Malformed("bad VF right".into()))?;
                    state.h += value;
                }
                147 => state.h += state.w,
                148..=151 => {
                    state.w = input
                        .signed((op - 147) as usize)
                        .and_then(|value| scaled(value, parent_at))
                        .ok_or_else(|| Error::Malformed("bad VF w".into()))?;
                    state.h += state.w;
                }
                152 => state.h += state.x,
                153..=156 => {
                    state.x = input
                        .signed((op - 152) as usize)
                        .and_then(|value| scaled(value, parent_at))
                        .ok_or_else(|| Error::Malformed("bad VF x".into()))?;
                    state.h += state.x;
                }
                157..=160 => {
                    let value = input
                        .signed((op - 156) as usize)
                        .and_then(|value| scaled(value, parent_at))
                        .ok_or_else(|| Error::Malformed("bad VF down".into()))?;
                    state.v += value;
                }
                161 => state.v += state.y,
                162..=165 => {
                    state.y = input
                        .signed((op - 161) as usize)
                        .and_then(|value| scaled(value, parent_at))
                        .ok_or_else(|| Error::Malformed("bad VF y".into()))?;
                    state.v += state.y;
                }
                166 => state.v += state.z,
                167..=170 => {
                    state.z = input
                        .signed((op - 166) as usize)
                        .and_then(|value| scaled(value, parent_at))
                        .ok_or_else(|| Error::Malformed("bad VF z".into()))?;
                    state.v += state.z;
                }
                171..=234 => current = Some(u32::from(op - 171)),
                235..=238 => {
                    current = Some(
                        input
                            .unsigned((op - 234) as usize)
                            .ok_or_else(|| Error::Malformed("bad VF font number".into()))?,
                    );
                }
                239..=242 => {
                    let length = input
                        .unsigned((op - 238) as usize)
                        .ok_or_else(|| Error::Malformed("bad VF special".into()))?
                        as usize;
                    let text = input
                        .bytes(length)
                        .ok_or_else(|| Error::Malformed("truncated VF special".into()))?
                        .to_vec();
                    out.push(dvi::Item::Special {
                        x: aitex_page::Sp(state.h),
                        y: aitex_page::Sp(state.v),
                        bytes: text,
                    });
                }
                _ => return Err(Error::Malformed(format!("illegal VF opcode {op}"))),
            }
        }
        if !stack.is_empty() {
            return Err(Error::Malformed("unbalanced VF push/pop".into()));
        }
        Ok(())
    }
}

/// Expand every virtual character in a parsed DVI document. `resolve_vf`
/// returns raw `<font>.vf` bytes when present; `width` supplies physical or
/// nested-virtual TFM advances at the requested size.
pub fn expand(
    doc: &dvi::Document,
    resolve_vf: impl FnMut(&str) -> Option<Vec<u8>>,
    width: impl FnMut(&str, i64, u32) -> Option<i64>,
) -> Result<dvi::Document, Error> {
    expand_with_offsets(doc, resolve_vf, width, |_, _, _| Some(0))
}

/// Expand virtual fonts, including host-synthesized per-glyph packet
/// movements such as pdfTeX's in-memory `\letterspacefont` packets.
pub fn expand_with_offsets(
    doc: &dvi::Document,
    resolve_vf: impl FnMut(&str) -> Option<Vec<u8>>,
    width: impl FnMut(&str, i64, u32) -> Option<i64>,
    glyph_offset: impl FnMut(&str, i64, u32) -> Option<i64>,
) -> Result<dvi::Document, Error> {
    let mut expander = Expander {
        resolve_vf,
        width,
        glyph_offset,
        cache: BTreeMap::new(),
        font_numbers: BTreeMap::new(),
        fonts: BTreeMap::new(),
    };
    let mut pages = Vec::with_capacity(doc.pages.len());
    for page in &doc.pages {
        let mut expanded = dvi::Page {
            items: Vec::new(),
            counts: page.counts,
            width_millibp: page.width_millibp,
            height_millibp: page.height_millibp,
        };
        for item in &page.items {
            match item {
                dvi::Item::ClassicGlyph {
                    font,
                    code,
                    origin_x,
                    origin_y,
                    ..
                } => {
                    let def = doc.fonts.get(&font.0).ok_or_else(|| {
                        Error::Malformed(format!("DVI uses undefined font {}", font.0))
                    })?;
                    expander.emit_char(
                        def,
                        *code,
                        origin_x.0,
                        origin_y.0,
                        0,
                        &mut expanded.items,
                    )?;
                }
                other => expanded.items.push(other.clone()),
            }
        }
        pages.push(expanded);
    }
    Ok(dvi::Document {
        pages,
        fonts: expander.fonts,
        num: doc.num,
        den: doc.den,
        mag: doc.mag,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classic(font: u32, code: u32, h: i64, v: i64, advance: i64) -> dvi::Item {
        dvi::Item::ClassicGlyph {
            font: aitex_page::FontInstanceId(font),
            code,
            origin_x: aitex_page::Sp(h),
            origin_y: aitex_page::Sp(v),
            advance: aitex_page::Sp(advance),
        }
    }

    fn vf(packet: &[u8]) -> Vec<u8> {
        let mut bytes = vec![247, 202, 0];
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&10_485_760u32.to_be_bytes());
        bytes.push(243);
        bytes.push(0);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&1_048_576u32.to_be_bytes());
        bytes.extend_from_slice(&10_485_760u32.to_be_bytes());
        bytes.extend_from_slice(&[0, 5]);
        bytes.extend_from_slice(b"cmr10");
        bytes.push(packet.len() as u8);
        bytes.push(65);
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(packet);
        bytes.push(248);
        bytes
    }

    fn document() -> dvi::Document {
        let mut doc = dvi::Document::default();
        doc.fonts.insert(
            7,
            dvi::FontDef {
                name: "virtual".into(),
                at: 655_360,
                design: 655_360,
                checksum: 0,
            },
        );
        let mut page = dvi::Page::default();
        page.items.push(classic(7, 65, 100, 200, 0));
        doc.pages.push(page);
        doc
    }

    #[test]
    fn expands_a_virtual_character_to_its_physical_font() {
        let output = expand(
            &document(),
            |name| (name == "virtual").then(|| vf(&[65])),
            |_, _, _| Some(327_680),
        )
        .expect("expand");
        assert_eq!(output.fonts[&0].name, "cmr10");
        assert_eq!(
            output.pages[0].items,
            vec![classic(0, 65, 100, 200, 327_680)]
        );
    }

    #[test]
    fn packet_movements_are_scaled_and_push_pop_is_local() {
        // push; right4 0.5em; set B; pop; set A
        let packet = [141, 146, 0, 8, 0, 0, 66, 142, 65];
        let output = expand(
            &document(),
            |name| (name == "virtual").then(|| vf(&packet)),
            |_, _, _| Some(327_680),
        )
        .expect("expand");
        assert_eq!(
            output.pages[0].items,
            vec![
                classic(0, 66, 327_780, 200, 327_680),
                classic(0, 65, 100, 200, 327_680),
            ]
        );
    }

    #[test]
    fn direct_and_virtual_uses_of_one_physical_face_share_a_font_id() {
        let mut input = document();
        input.fonts.insert(
            3,
            dvi::FontDef {
                name: "cmr10".into(),
                at: 655_360,
                design: 655_360,
                checksum: 1,
            },
        );
        input.pages[0]
            .items
            .insert(0, classic(3, 66, 50, 200, 327_680));

        let output = expand(
            &input,
            |name| (name == "virtual").then(|| vf(&[65])),
            |_, _, _| Some(327_680),
        )
        .expect("expand");

        assert_eq!(output.fonts.len(), 1);
        assert_eq!(output.fonts[&0].name, "cmr10");
        assert_eq!(
            output.pages[0].items,
            vec![
                classic(0, 66, 50, 200, 327_680),
                classic(0, 65, 100, 200, 327_680),
            ]
        );
    }

    #[test]
    fn synthetic_virtual_font_can_move_only_the_painted_glyph() {
        let output = expand_with_offsets(
            &document(),
            |_| None,
            |_, _, _| Some(327_680),
            |name, _, _| (name == "virtual").then_some(-8_192),
        )
        .expect("expand");
        assert_eq!(
            output.pages[0].items,
            vec![classic(0, 65, 100 - 8_192, 200, 327_680)]
        );
    }
}
