//! Bounded reader for TeX's packed (`PK`) bitmap-font format.
//!
//! PK is the normal fallback when a classic Metafont family has a TFM but no
//! scalable program (for example `tcrm1000` in a minimal TeX Live install).
//! The parser expands each character to a one-bit, row-major bitmap so the PDF
//! writer can embed it as a Type 3 font without invoking a renderer.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub code: u32,
    pub width: usize,
    pub height: usize,
    pub x_offset: i32,
    pub y_offset: i32,
    /// One bit per pixel, most-significant bit first, each row byte-aligned.
    pub bitmap: Vec<u8>,
}

impl Glyph {
    pub fn black(&self, x: usize, y: usize) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let stride = self.width.div_ceil(8);
        self.bitmap
            .get(y * stride + x / 8)
            .is_some_and(|byte| byte & (0x80 >> (x % 8)) != 0)
    }
}

#[derive(Clone, Debug)]
pub struct Font {
    /// Design size in TeX points, multiplied by 2^20.
    pub design_size: i32,
    /// Horizontal pixels per TeX point, multiplied by 2^16.
    pub hppp: i32,
    pub glyphs: BTreeMap<u8, Glyph>,
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
        if count == 0 || count > 4 {
            return None;
        }
        let mut value = 0u32;
        for _ in 0..count {
            value = value.checked_mul(256)?.checked_add(self.byte()? as u32)?;
        }
        Some(value)
    }

    fn signed(&mut self, count: usize) -> Option<i32> {
        let value = self.unsigned(count)?;
        let shift = 32usize.checked_sub(count * 8)?;
        Some(((value << shift) as i32) >> shift)
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        self.at = self.at.checked_add(count)?;
        (self.at <= self.bytes.len()).then_some(())
    }
}

struct Nybbles<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Nybbles<'_> {
    fn get(&mut self) -> Option<usize> {
        let byte = *self.bytes.get(self.at / 2)?;
        let value = if self.at % 2 == 0 {
            byte >> 4
        } else {
            byte & 0x0f
        };
        self.at += 1;
        Some(value as usize)
    }
}

fn packed_number(
    input: &mut Nybbles<'_>,
    dyn_f: usize,
    repeat: &mut Option<usize>,
    nested: bool,
) -> Option<usize> {
    let mut i = input.get()?;
    if i == 0 {
        let mut zeros = 0usize;
        let mut j;
        loop {
            j = input.get()?;
            if j != 0 {
                break;
            }
            zeros = zeros.checked_add(1)?;
        }
        for _ in 0..=zeros {
            j = j.checked_mul(16)?.checked_add(input.get()?)?;
        }
        return j
            .checked_sub(15)?
            .checked_add((13usize.checked_sub(dyn_f)?) * 16 + dyn_f);
    }
    if i <= dyn_f {
        return Some(i);
    }
    if i < 14 {
        return (i - dyn_f - 1)
            .checked_mul(16)?
            .checked_add(input.get()?)?
            .checked_add(dyn_f + 1);
    }
    if nested || repeat.is_some() {
        return None;
    }
    i = if i == 14 {
        packed_number(input, dyn_f, repeat, true)?
    } else {
        1
    };
    *repeat = Some(i);
    packed_number(input, dyn_f, repeat, true)
}

fn set_black(bitmap: &mut [u8], stride: usize, x: usize, y: usize) -> Option<()> {
    let byte = bitmap.get_mut(y.checked_mul(stride)?.checked_add(x / 8)?)?;
    *byte |= 0x80 >> (x % 8);
    Some(())
}

fn decode_bitmap(
    raster: &[u8],
    width: usize,
    height: usize,
    dyn_f: usize,
    first_black: bool,
) -> Option<Vec<u8>> {
    let pixels = width.checked_mul(height)?;
    if pixels > 16 * 1024 * 1024 {
        return None;
    }
    let stride = width.div_ceil(8);
    let mut bitmap = vec![0u8; stride.checked_mul(height)?];
    if pixels == 0 {
        return Some(bitmap);
    }
    if dyn_f == 14 {
        for bit in 0..pixels {
            let source = *raster.get(bit / 8)?;
            if source & (0x80 >> (bit % 8)) != 0 {
                set_black(&mut bitmap, stride, bit % width, bit / width)?;
            }
        }
        return Some(bitmap);
    }

    let mut input = Nybbles {
        bytes: raster,
        at: 0,
    };
    let mut x = 0usize;
    let mut y = 0usize;
    let mut black = first_black;
    let mut repeat = None;
    while y < height {
        let mut count = packed_number(&mut input, dyn_f, &mut repeat, false)?;
        if count == 0 {
            return None;
        }
        while count > 0 {
            let take = count.min(width - x);
            if black {
                for column in x..x + take {
                    set_black(&mut bitmap, stride, column, y)?;
                }
            }
            x += take;
            count -= take;
            if x == width {
                let copies = repeat.take().unwrap_or(0);
                if y.checked_add(copies)? >= height {
                    return None;
                }
                let row_start = y * stride;
                let row = bitmap.get(row_start..row_start + stride)?.to_vec();
                for copy in 1..=copies {
                    let start = (y + copy) * stride;
                    bitmap.get_mut(start..start + stride)?.copy_from_slice(&row);
                }
                y += copies + 1;
                x = 0;
                if y == height && count != 0 {
                    return None;
                }
            }
        }
        black = !black;
    }
    (x == 0 && repeat.is_none()).then_some(bitmap)
}

/// Parse and validate one PK font. Unsupported or malformed input returns
/// `None`; all lengths and bitmap products are checked before allocation.
pub fn parse(bytes: &[u8]) -> Option<Font> {
    let mut input = Reader { bytes, at: 0 };
    if input.byte()? != 247 || input.byte()? != 89 {
        return None;
    }
    let comment = input.byte()? as usize;
    input.skip(comment)?;
    let design_size = input.signed(4)?;
    let _checksum = input.signed(4)?;
    let hppp = input.signed(4)?;
    let _vppp = input.signed(4)?;
    if design_size <= 0 || hppp <= 0 {
        return None;
    }

    let mut glyphs = BTreeMap::new();
    loop {
        let flag = input.byte()?;
        match flag {
            0..=239 => {
                let dyn_f = (flag >> 4) as usize;
                let first_black = flag & 8 != 0;
                let kind = flag & 7;
                let (packet_length, code, width, height, x_offset, y_offset) = match kind {
                    0..=3 => {
                        let length = (kind as usize)
                            .checked_mul(256)?
                            .checked_add(input.byte()? as usize)?;
                        let code = input.byte()? as u32;
                        let end = input.at.checked_add(length)?;
                        input.skip(3)?; // TFM width
                        input.skip(1)?; // horizontal escapement
                        let width = input.byte()? as usize;
                        let height = input.byte()? as usize;
                        let x = input.signed(1)?;
                        let y = input.signed(1)?;
                        (end, code, width, height, x, y)
                    }
                    4..=6 => {
                        let length = ((kind - 4) as usize)
                            .checked_mul(65536)?
                            .checked_add(input.unsigned(2)? as usize)?;
                        let code = input.byte()? as u32;
                        let end = input.at.checked_add(length)?;
                        input.skip(3)?;
                        input.skip(2)?;
                        let width = input.unsigned(2)? as usize;
                        let height = input.unsigned(2)? as usize;
                        let x = input.signed(2)?;
                        let y = input.signed(2)?;
                        (end, code, width, height, x, y)
                    }
                    7 => {
                        let length = input.unsigned(4)? as usize;
                        let code = input.unsigned(4)?;
                        let end = input.at.checked_add(length)?;
                        input.skip(4)?; // TFM width
                        input.skip(4)?; // dx
                        input.skip(4)?; // dy
                        let width = input.unsigned(4)? as usize;
                        let height = input.unsigned(4)? as usize;
                        let x = input.signed(4)?;
                        let y = input.signed(4)?;
                        (end, code, width, height, x, y)
                    }
                    _ => return None,
                };
                if packet_length > bytes.len() || input.at > packet_length {
                    return None;
                }
                let raster = bytes.get(input.at..packet_length)?;
                let bitmap = decode_bitmap(raster, width, height, dyn_f, first_black)?;
                input.at = packet_length;
                if let Ok(code8) = u8::try_from(code) {
                    glyphs.insert(
                        code8,
                        Glyph {
                            code,
                            width,
                            height,
                            x_offset,
                            y_offset,
                            bitmap,
                        },
                    );
                }
            }
            240..=243 => {
                let count = input.unsigned((flag - 239) as usize)? as usize;
                input.skip(count)?;
            }
            244 => input.skip(4)?,
            245 => break,
            246 => {}
            247..=255 => return None,
        }
    }
    Some(Font {
        design_size,
        hppp,
        glyphs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preamble() -> Vec<u8> {
        let mut bytes = vec![247, 89, 0];
        bytes.extend_from_slice(&10_485_760i32.to_be_bytes());
        bytes.extend_from_slice(&0i32.to_be_bytes());
        bytes.extend_from_slice(&544_093i32.to_be_bytes());
        bytes.extend_from_slice(&544_093i32.to_be_bytes());
        bytes
    }

    #[test]
    fn parses_bitmapped_short_packet() {
        let mut bytes = preamble();
        bytes.extend_from_slice(&[0xe0, 9, 65, 0, 0, 0, 2, 2, 2, 0, 2, 0x90, 245]);
        let font = parse(&bytes).expect("PK");
        let glyph = &font.glyphs[&65];
        assert_eq!((glyph.width, glyph.height), (2, 2));
        assert!(glyph.black(0, 0));
        assert!(!glyph.black(1, 0));
        assert!(!glyph.black(0, 1));
        assert!(glyph.black(1, 1));
    }

    #[test]
    fn parses_run_encoded_packet() {
        let mut bytes = preamble();
        // dyn_f=8, first run black: two black then two white pixels.
        bytes.extend_from_slice(&[0x88, 9, 66, 0, 0, 0, 4, 4, 1, 0, 1, 0x22, 245]);
        let font = parse(&bytes).expect("PK");
        let glyph = &font.glyphs[&66];
        assert!(glyph.black(0, 0));
        assert!(glyph.black(1, 0));
        assert!(!glyph.black(2, 0));
        assert!(!glyph.black(3, 0));
    }

    #[test]
    fn malformed_packet_length_is_rejected() {
        let mut bytes = preamble();
        bytes.extend_from_slice(&[0xe0, 200, 65, 245]);
        assert!(parse(&bytes).is_none());
    }
}
