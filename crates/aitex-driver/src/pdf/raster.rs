struct Raster {
    width: usize,
    height: usize,
    color: RasterColor,
    bits_per_component: u8,
    encoding: RasterEncoding,
    /// Optional eight-bit opacity plane. It is emitted as a PDF soft mask so
    /// transparent PNGs composite over TeX content instead of being flattened
    /// against an arbitrary white background at asset-load time.
    alpha: Option<AlphaEncoding>,
    /// A PDF color-key mask for PNG tRNS values that are exactly transparent
    /// or opaque. Partial and multi-index alpha continue through the explicit
    /// soft-mask normalization path.
    color_key: Option<Vec<u16>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RasterColor {
    Gray,
    Rgb,
    Cmyk { inverted: bool },
    Indexed { palette: Vec<u8> },
}

enum RasterEncoding {
    Raw(Vec<u8>),
    /// An ordinary eight-bit grayscale or three-component JPEG. PDF readers understand
    /// the source stream directly through `DCTDecode`; decoding it at asset
    /// load time changed colour conversion and needlessly expanded memory.
    Dct(Vec<u8>),
    /// Concatenated PNG `IDAT` bytes: already a zlib stream with one PNG
    /// predictor byte per row, exactly what PDF's FlateDecode predictor 15
    /// consumes.
    FlatePng(Vec<u8>),
}

enum AlphaEncoding {
    Raw(Vec<u8>),
    /// Concatenated IDAT bytes from an eight-bit grayscale PNG. Like the RGB
    /// stream above, these are already suitable for PDF predictor 15 and stay
    /// compressed throughout import and object assembly.
    FlatePng(Vec<u8>),
}

/// Parse Ghostscript's `ppmraw` output. Comments and arbitrary whitespace are
/// legal between header fields; the RGB body begins after the max value.
fn parse_ppm(bytes: &[u8]) -> Option<Raster> {
    fn token<'a>(bytes: &'a [u8], p: &mut usize) -> Option<&'a [u8]> {
        loop {
            while bytes.get(*p).is_some_and(u8::is_ascii_whitespace) {
                *p += 1;
            }
            if bytes.get(*p) != Some(&b'#') {
                break;
            }
            while bytes.get(*p).is_some_and(|b| *b != b'\n') {
                *p += 1;
            }
        }
        let start = *p;
        while bytes
            .get(*p)
            .is_some_and(|b| !b.is_ascii_whitespace() && *b != b'#')
        {
            *p += 1;
        }
        (start < *p).then_some(&bytes[start..*p])
    }

    let mut p = 0;
    if token(bytes, &mut p)? != b"P6" {
        return None;
    }
    let width: usize = std::str::from_utf8(token(bytes, &mut p)?)
        .ok()?
        .parse()
        .ok()?;
    let height: usize = std::str::from_utf8(token(bytes, &mut p)?)
        .ok()?
        .parse()
        .ok()?;
    if token(bytes, &mut p)? != b"255" {
        return None;
    }
    match bytes.get(p) {
        Some(b'\r') if bytes.get(p + 1) == Some(&b'\n') => p += 2,
        Some(separator) if separator.is_ascii_whitespace() => p += 1,
        _ => return None,
    }
    let len = width.checked_mul(height)?.checked_mul(3)?;
    let pixels = bytes.get(p..p.checked_add(len)?)?.to_vec();
    Some(Raster {
        width,
        height,
        color: RasterColor::Rgb,
        bits_per_component: 8,
        encoding: RasterEncoding::Raw(pixels),
        alpha: None,
        color_key: None,
    })
}

/// Parse MuPDF's raw RGB-alpha PAM output. PAM keeps the opacity samples
/// explicit, unlike PPM, so the driver can split them into a PDF RGB image and
/// `/SMask` without depending on a PNG inflater.
fn parse_pam(bytes: &[u8]) -> Option<Raster> {
    if !bytes.starts_with(b"P7\n") {
        return None;
    }
    let header_end = bytes.windows(7).position(|window| window == b"ENDHDR\n")? + 7;
    let header = std::str::from_utf8(bytes.get(..header_end)?).ok()?;
    let field = |name: &str| {
        header
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(str::trim)
    };
    let width: usize = field("WIDTH ")?.parse().ok()?;
    let height: usize = field("HEIGHT ")?.parse().ok()?;
    let depth: usize = field("DEPTH ")?.parse().ok()?;
    if width == 0
        || height == 0
        || depth != 4
        || field("MAXVAL ")? != "255"
        || field("TUPLTYPE ")? != "RGB_ALPHA"
    {
        return None;
    }
    let pixels = width.checked_mul(height)?;
    let body = bytes.get(header_end..header_end.checked_add(pixels.checked_mul(4)?)?)?;
    let mut rgb = Vec::with_capacity(pixels * 3);
    let mut alpha = Vec::with_capacity(pixels);
    for sample in body.chunks_exact(4) {
        rgb.extend_from_slice(&sample[..3]);
        alpha.push(sample[3]);
    }
    Some(Raster {
        width,
        height,
        color: RasterColor::Rgb,
        bits_per_component: 8,
        encoding: RasterEncoding::Raw(rgb),
        alpha: Some(AlphaEncoding::Raw(alpha)),
        color_key: None,
    })
}

const ALPHA_PNG_BUNDLE: &[u8] = b"AITEX-RGBA-PNG\0";
const FILTERED_PNG_BUNDLE: &[u8] = b"AITEX-PNG-ZLIB\0";

// PNG image data stays in predictor-compressed streams throughout this
// module, so this bounds logical PDF samples rather than an allocation. It is
// deliberately large enough for the 24,000 x 20,400 plots accepted by
// pdfTeX, while remaining a finite guard against hostile dimensions.
const MAX_RASTER_DECODED_BYTES: usize = 2 * 1024 * 1024 * 1024;

struct PngStream {
    width: usize,
    height: usize,
    bits_per_component: u8,
    color_type: u8,
    palette: Option<Vec<u8>>,
    transparency: Option<Vec<u8>>,
    idat: Vec<u8>,
}

/// Parse one non-interlaced PNG stream without inflating it. PNG IDAT uses the
/// same row predictors accepted by PDF predictor 15, including packed 1/2/4
/// bit samples. Unknown critical chunks and structurally invalid streams take
/// the bounded host fallback rather than becoming a plausible broken PDF.
fn parse_png_stream(bytes: &[u8]) -> Option<PngStream> {
    if bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut p = 8usize;
    let mut header = None;
    let mut palette = None;
    let mut transparency = None;
    let mut idat = Vec::new();
    let mut saw_idat = false;
    let mut left_idat = false;
    let mut ended = false;
    while p.checked_add(12)? <= bytes.len() {
        let len = u32::from_be_bytes(bytes.get(p..p + 4)?.try_into().ok()?) as usize;
        let kind = bytes.get(p + 4..p + 8)?;
        let data_start = p + 8;
        let data_end = data_start.checked_add(len)?;
        let next = data_end.checked_add(4)?; // CRC, checked but not recomputed
        let data = bytes.get(data_start..data_end)?;
        match kind {
            b"IHDR" => {
                if header.is_some() || p != 8 || data.len() != 13 {
                    return None;
                }
                let width = u32::from_be_bytes(data.get(0..4)?.try_into().ok()?) as usize;
                let height = u32::from_be_bytes(data.get(4..8)?.try_into().ok()?) as usize;
                let depth = data[8];
                let color_type = data[9];
                let valid_depth = match color_type {
                    0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
                    2 => matches!(depth, 8 | 16),
                    3 => matches!(depth, 1 | 2 | 4 | 8),
                    4 | 6 => depth == 8 || depth == 16,
                    _ => false,
                };
                let components = match color_type {
                    0 | 3 => 1usize,
                    2 => 3,
                    4 => 2,
                    6 => 4,
                    _ => return None,
                };
                let row_bits = width
                    .checked_mul(components)?
                    .checked_mul(usize::from(depth))?;
                let row_bytes = row_bits.checked_add(7)? / 8;
                let decoded = row_bytes.checked_add(1)?.checked_mul(height)?;
                if width == 0
                    || height == 0
                    || !valid_depth
                    || data[10] != 0
                    || data[11] != 0
                    || data[12] != 0
                    || decoded > MAX_RASTER_DECODED_BYTES
                {
                    return None;
                }
                header = Some((width, height, depth, color_type));
            }
            b"PLTE" => {
                if saw_idat || palette.is_some() || data.is_empty() || data.len() > 768
                    || data.len() % 3 != 0
                {
                    return None;
                }
                palette = Some(data.to_vec());
            }
            b"tRNS" => {
                if saw_idat || transparency.is_some() {
                    return None;
                }
                transparency = Some(data.to_vec());
            }
            b"IDAT" => {
                if header.is_none() || left_idat {
                    return None;
                }
                saw_idat = true;
                idat.extend_from_slice(data);
            }
            b"IEND" => {
                if !data.is_empty() || !saw_idat {
                    return None;
                }
                ended = true;
                break;
            }
            // An uppercase first byte denotes a critical PNG chunk. Carrying
            // an unknown critical transform into PDF would be incorrect.
            _ if kind.first().is_some_and(u8::is_ascii_uppercase) => return None,
            _ => {
                left_idat |= saw_idat;
            }
        }
        p = next;
    }
    let (width, height, bits_per_component, color_type) = header?;
    if !ended || idat.is_empty() {
        return None;
    }
    if color_type == 3 {
        let entries = palette.as_ref()?.len() / 3;
        if entries == 0 || entries > (1usize << bits_per_component) {
            return None;
        }
    }
    Some(PngStream {
        width,
        height,
        bits_per_component,
        color_type,
        palette,
        transparency,
        idat,
    })
}

/// Preserve directly representable PNG color models, bit depths, predictors,
/// palettes, and exact color-key transparency without expanding their pixels.
fn parse_png(bytes: &[u8]) -> Option<Raster> {
    let png = parse_png_stream(bytes)?;
    let color = match png.color_type {
        0 => RasterColor::Gray,
        2 => RasterColor::Rgb,
        3 => RasterColor::Indexed {
            palette: png.palette.clone()?,
        },
        // Explicit alpha needs a separate PDF soft mask.
        4 | 6 => return None,
        _ => return None,
    };
    let color_key = match (png.color_type, png.transparency.as_deref()) {
        (_, None) => None,
        (0, Some([high, low])) => Some(vec![
            u16::from_be_bytes([*high, *low]),
            u16::from_be_bytes([*high, *low]),
        ]),
        (2, Some([rh, rl, gh, gl, bh, bl])) => {
            let red = u16::from_be_bytes([*rh, *rl]);
            let green = u16::from_be_bytes([*gh, *gl]);
            let blue = u16::from_be_bytes([*bh, *bl]);
            Some(vec![red, red, green, green, blue, blue])
        }
        (3, Some(alpha)) => {
            let entries = png.palette.as_ref()?.len() / 3;
            if alpha.len() > entries {
                return None;
            }
            let transparent = (0..entries)
                .filter(|index| alpha.get(*index).copied().unwrap_or(255) == 0)
                .collect::<Vec<_>>();
            let partial = (0..entries).any(|index| {
                !matches!(alpha.get(index).copied().unwrap_or(255), 0 | 255)
            });
            if partial || transparent.len() > 1 {
                return None;
            }
            if transparent.is_empty() {
                return Some(Raster {
                    width: png.width,
                    height: png.height,
                    color,
                    bits_per_component: png.bits_per_component,
                    encoding: RasterEncoding::FlatePng(png.idat),
                    alpha: None,
                    color_key: None,
                });
            }
            let index = u16::try_from(transparent[0]).ok()?;
            Some(vec![index, index])
        }
        _ => return None,
    };
    if png.color_type != 3 {
        let maximum = if png.bits_per_component == 16 {
            u16::MAX
        } else {
            (1u16 << png.bits_per_component) - 1
        };
        if color_key
            .as_ref()
            .is_some_and(|key| key.iter().any(|value| *value > maximum))
        {
            return None;
        }
    }
    Some(Raster {
        width: png.width,
        height: png.height,
        color,
        bits_per_component: png.bits_per_component,
        encoding: RasterEncoding::FlatePng(png.idat),
        alpha: None,
        color_key,
    })
}

/// Read the host's pair of compressed PNG planes. Keeping colour and opacity
/// separate preserves samples hidden below transparent pixels; zeroing them
/// changes colour when a PDF reader downsamples image and mask. Netpbm may
/// losslessly reduce an achromatic RGB plane to grayscale, so accept either
/// colour model rather than discarding an otherwise valid normalized image.
fn parse_alpha_png_bundle(bytes: &[u8]) -> Option<Raster> {
    let payload = bytes.strip_prefix(ALPHA_PNG_BUNDLE)?;
    let rgb_len = u64::from_be_bytes(payload.get(..8)?.try_into().ok()?) as usize;
    let rgb_end = 8usize.checked_add(rgb_len)?;
    let rgb = payload.get(8..rgb_end)?;
    let alpha = payload.get(rgb_end..)?;
    let rgb = parse_png_stream(rgb)?;
    let alpha = parse_png_stream(alpha)?;
    if !matches!(rgb.color_type, 0 | 2)
        || rgb.bits_per_component != 8
        || rgb.transparency.is_some()
        || alpha.color_type != 0
        || alpha.bits_per_component != 8
        || alpha.transparency.is_some()
        || (rgb.width, rgb.height) != (alpha.width, alpha.height)
    {
        return None;
    }
    Some(Raster {
        width: rgb.width,
        height: rgb.height,
        color: if rgb.color_type == 0 {
            RasterColor::Gray
        } else {
            RasterColor::Rgb
        },
        bits_per_component: 8,
        encoding: RasterEncoding::FlatePng(rgb.idat),
        alpha: Some(AlphaEncoding::FlatePng(alpha.idat)),
        color_key: None,
    })
}

/// Read the CLI's in-process PNG normalization result. Both payloads are
/// complete zlib streams of PNG-filtered scanlines and can therefore be copied
/// directly into PDF image objects. An empty alpha payload denotes a source
/// whose nominal alpha channel was entirely opaque.
fn parse_filtered_png_bundle(bytes: &[u8]) -> Option<Raster> {
    let payload = bytes.strip_prefix(FILTERED_PNG_BUNDLE)?;
    let width = u32::from_be_bytes(payload.get(..4)?.try_into().ok()?) as usize;
    let height = u32::from_be_bytes(payload.get(4..8)?.try_into().ok()?) as usize;
    let rgb_len = usize::try_from(u64::from_be_bytes(
        payload.get(8..16)?.try_into().ok()?,
    ))
    .ok()?;
    let alpha_len = usize::try_from(u64::from_be_bytes(
        payload.get(16..24)?.try_into().ok()?,
    ))
    .ok()?;
    if width == 0 || height == 0 || rgb_len == 0 {
        return None;
    }
    let decoded = width.checked_mul(height)?.checked_mul(3)?;
    if decoded > MAX_RASTER_DECODED_BYTES {
        return None;
    }
    let rgb_end = 24usize.checked_add(rgb_len)?;
    let alpha_end = rgb_end.checked_add(alpha_len)?;
    if alpha_end != payload.len() {
        return None;
    }
    let rgb = payload.get(24..rgb_end)?.to_vec();
    let alpha = if alpha_len > 0 {
        Some(AlphaEncoding::FlatePng(
            payload.get(rgb_end..alpha_end)?.to_vec(),
        ))
    } else {
        None
    };
    Some(Raster {
        width,
        height,
        color: RasterColor::Rgb,
        bits_per_component: 8,
        encoding: RasterEncoding::FlatePng(rgb),
        alpha,
        color_key: None,
    })
}

/// Read the dimensions and color model of an eight-bit JPEG without decoding
/// it. Adobe-authored CMYK streams store inverted samples; PDF represents that
/// convention with a `/Decode [1 0 ...]` array while retaining the original
/// DCT stream.
fn jpeg_dimensions(bytes: &[u8]) -> Option<(usize, usize, u8, bool)> {
    if bytes.get(..2)? != b"\xff\xd8" {
        return None;
    }
    let mut p = 2usize;
    let mut adobe_cmyk = false;
    while p < bytes.len() {
        while bytes.get(p) == Some(&0xff) {
            p += 1;
        }
        let marker = *bytes.get(p)?;
        p += 1;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if marker == 0x00 || marker == 0x01 || marker == 0xd8 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length = u16::from_be_bytes(bytes.get(p..p.checked_add(2)?)?.try_into().ok()?) as usize;
        if length < 2 {
            return None;
        }
        let segment_end = p.checked_add(length)?;
        let segment = bytes.get(p + 2..segment_end)?;
        if marker == 0xee && segment.starts_with(b"Adobe") && segment.len() >= 12 {
            // APP14 byte 11 is Adobe's color-transform selector. Both its
            // ordinary CMYK encoding (0) and YCCK encoding (2) decode to
            // samples inverted relative to PDF's DeviceCMYK convention.
            adobe_cmyk = matches!(segment[11], 0 | 2);
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            if segment.len() < 6 || segment[0] != 8 || !matches!(segment[5], 1 | 3 | 4) {
                return None;
            }
            let height = u16::from_be_bytes(segment.get(1..3)?.try_into().ok()?) as usize;
            let width = u16::from_be_bytes(segment.get(3..5)?.try_into().ok()?) as usize;
            if width == 0 || height == 0 {
                return None;
            }
            let components = segment[5];
            let decoded = width
                .checked_mul(height)?
                .checked_mul(usize::from(components))?;
            if decoded > MAX_RASTER_DECODED_BYTES {
                return None;
            }
            return Some((width, height, components, adobe_cmyk));
        }
        p = segment_end;
    }
    None
}

/// Whether the PDF writer can copy this JPEG's DCT stream directly. The CLI
/// uses this cheap header check to avoid rasterizing an image that this module
/// would then recognize and embed unchanged.
pub fn directly_embeddable_jpeg(bytes: &[u8]) -> bool {
    jpeg_dimensions(bytes).is_some()
}

fn parse_jpeg(bytes: &[u8]) -> Option<Raster> {
    let (width, height, components, adobe_cmyk) = jpeg_dimensions(bytes)?;
    Some(Raster {
        width,
        height,
        color: match components {
            1 => RasterColor::Gray,
            3 => RasterColor::Rgb,
            4 => RasterColor::Cmyk {
                inverted: adobe_cmyk,
            },
            _ => return None,
        },
        bits_per_component: 8,
        encoding: RasterEncoding::Dct(bytes.to_vec()),
        alpha: None,
        color_key: None,
    })
}

fn parse_raster(bytes: &[u8]) -> Option<Raster> {
    parse_filtered_png_bundle(bytes)
        .or_else(|| parse_alpha_png_bundle(bytes))
        .or_else(|| parse_png(bytes))
        .or_else(|| parse_jpeg(bytes))
        .or_else(|| parse_ppm(bytes))
        .or_else(|| parse_pam(bytes))
}

fn image_object(objects: &Objects, raster: &Raster, soft_mask: Option<usize>) -> Vec<u8> {
    let components = match &raster.color {
        RasterColor::Gray | RasterColor::Indexed { .. } => 1,
        RasterColor::Rgb => 3,
        RasterColor::Cmyk { .. } => 4,
    };
    let (filter, bytes, encoded) = match &raster.encoding {
        RasterEncoding::Raw(bytes) => (String::new(), bytes.as_slice(), false),
        RasterEncoding::Dct(bytes) => (
            " /Filter /DCTDecode".to_string(),
            bytes.as_slice(),
            true,
        ),
        RasterEncoding::FlatePng(bytes) => (
            format!(
                " /Filter /FlateDecode /DecodeParms << /Predictor 15 /Colors {components} \
                 /BitsPerComponent {} /Columns {} >>",
                raster.bits_per_component, raster.width
            ),
            bytes.as_slice(),
            true,
        ),
    };
    let soft_mask = soft_mask
        .map(|id| format!(" /SMask {id} 0 R"))
        .unwrap_or_default();
    let color_space = match &raster.color {
        RasterColor::Gray => "/DeviceGray".to_string(),
        RasterColor::Rgb => "/DeviceRGB".to_string(),
        RasterColor::Cmyk { .. } => "/DeviceCMYK".to_string(),
        RasterColor::Indexed { palette } => {
            let high = palette.len().saturating_div(3).saturating_sub(1);
            let palette = palette
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<String>();
            format!("[/Indexed /DeviceRGB {high} <{palette}>]")
        }
    };
    let decode = match &raster.color {
        RasterColor::Cmyk { inverted: true } => " /Decode [1 0 1 0 1 0 1 0]",
        _ => "",
    };
    let color_key = raster.color_key.as_ref().map_or_else(String::new, |key| {
        let values = key
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        format!(" /Mask [{values}]")
    });
    let dictionary = format!(
        "<< /Type /XObject /Subtype /Image /Width {} /Height {} \
         /ColorSpace {color_space} /BitsPerComponent {}{decode}{soft_mask}{color_key}",
        raster.width, raster.height, raster.bits_per_component,
    );
    if encoded {
        objects.encoded_stream_object(dictionary.as_bytes(), filter.as_bytes(), bytes)
    } else {
        objects.stream_object(dictionary.as_bytes(), bytes)
    }
}

type AlphaFingerprint = (usize, u64, u64, usize, usize, bool);

fn alpha_fingerprint(raster: &Raster, alpha: &AlphaEncoding) -> AlphaFingerprint {
    let (bytes, filtered) = match alpha {
        AlphaEncoding::Raw(bytes) => (bytes.as_slice(), false),
        AlphaEncoding::FlatePng(bytes) => (bytes.as_slice(), true),
    };
    let (len, first, second) = bytes_fingerprint(bytes);
    (len, first, second, raster.width, raster.height, filtered)
}

fn write_alpha_object(objects: &mut Objects, raster: &Raster, alpha: &AlphaEncoding) -> usize {
    let id = objects.reserve();
    let (filter, bytes, encoded) = match alpha {
        AlphaEncoding::Raw(bytes) => (String::new(), bytes.as_slice(), false),
        AlphaEncoding::FlatePng(bytes) => (
            format!(
                " /Filter /FlateDecode /DecodeParms << /Predictor 15 /Colors 1 \
                 /BitsPerComponent 8 /Columns {} >>",
                raster.width
            ),
            bytes.as_slice(),
            true,
        ),
    };
    let dictionary = format!(
        "<< /Type /XObject /Subtype /Image /Width {} /Height {} \
         /ColorSpace /DeviceGray /BitsPerComponent 8",
        raster.width, raster.height,
    );
    let object = if encoded {
        objects.encoded_stream_object(dictionary.as_bytes(), filter.as_bytes(), bytes)
    } else {
        objects.stream_object(dictionary.as_bytes(), bytes)
    };
    objects.write_owned(id, object);
    id
}

fn write_raster_objects_with_masks(
    objects: &mut Objects,
    raster: &Raster,
    masks: &mut std::collections::BTreeMap<AlphaFingerprint, usize>,
) -> usize {
    let soft_mask = raster.alpha.as_ref().map(|alpha| {
        let key = alpha_fingerprint(raster, alpha);
        *masks
            .entry(key)
            .or_insert_with(|| write_alpha_object(objects, raster, alpha))
    });
    let image = objects.reserve();
    let object = image_object(objects, raster, soft_mask);
    objects.write_owned(image, object);
    image
}

#[cfg(test)]
fn write_raster_objects(objects: &mut Objects, raster: &Raster) -> usize {
    write_raster_objects_with_masks(objects, raster, &mut std::collections::BTreeMap::new())
}

fn bytes_fingerprint(bytes: &[u8]) -> (usize, u64, u64) {
    // Two independent FNV-1a streams make content-addressed XObject reuse
    // deterministic without retaining another full copy of a large asset as a
    // map key.
    let mut first = 0xcbf2_9ce4_8422_2325u64;
    let mut second = 0x8422_2325_cbf2_9ce4u64;
    for byte in bytes {
        first = (first ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
        second = (second ^ u64::from(*byte)).wrapping_mul(0x100_0000_01e7);
    }
    (bytes.len(), first, second)
}

fn raster_fingerprint(raster: &Raster) -> (usize, u64, u64, u64, usize, usize) {
    let bytes = match &raster.encoding {
        RasterEncoding::Raw(bytes)
        | RasterEncoding::Dct(bytes)
        | RasterEncoding::FlatePng(bytes) => bytes,
    };
    let (mut len, mut first, mut second) = bytes_fingerprint(bytes);
    if let Some(alpha) = &raster.alpha {
        let alpha = match alpha {
            AlphaEncoding::Raw(bytes) | AlphaEncoding::FlatePng(bytes) => bytes,
        };
        let (alpha_len, alpha_first, alpha_second) = bytes_fingerprint(alpha);
        len = len.saturating_add(alpha_len);
        first ^= alpha_first.rotate_left(17);
        second ^= alpha_second.rotate_right(13);
    }
    let mut semantics = u64::from(raster.bits_per_component);
    semantics = semantics.wrapping_mul(257).wrapping_add(match &raster.color {
        RasterColor::Gray => 1,
        RasterColor::Rgb => 2,
        RasterColor::Cmyk { inverted } => {
            if *inverted {
                3
            } else {
                5
            }
        }
        RasterColor::Indexed { palette } => {
            let (palette_len, palette_first, palette_second) = bytes_fingerprint(palette);
            len = len.saturating_add(palette_len);
            first ^= palette_first.rotate_left(7);
            second ^= palette_second.rotate_right(9);
            4
        }
    });
    if let Some(key) = &raster.color_key {
        for value in key {
            semantics = semantics
                .wrapping_mul(0x100_0000_01b3)
                .wrapping_add(u64::from(*value));
        }
    }
    (len, first, second, semantics, raster.width, raster.height)
}

/// Whether a PNG can be copied into a PDF Image XObject without decoding.
pub fn directly_embeddable_png(bytes: &[u8]) -> bool {
    parse_png(bytes).is_some()
}
