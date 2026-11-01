//! `barcode` data format (Camel `camel-barcode`, ZXing based in Java): marshal text into a
//! barcode image, unmarshal an image back to text.
//!
//! Symbologies (`barcodeFormat`, default `QR_CODE`):
//!
//! | format | encode | decode |
//! |---|---|---|
//! | `QR_CODE` | versions 1-40, ECC L/M/Q/H (`errorCorrectionLevel`, default L like ZXing), numeric / alphanumeric / Kanji (JIS X 0208 text) / byte (UTF-8 with ECI 26 for non-ASCII), RS, mask by penalty rules | finder patterns, sampling, format/version info, unmasking, RS correction, Kanji to Unicode |
//! | `DATA_MATRIX` | ECC 200 square 10x10 .. 144x144 and rectangular 8x18 .. 16x48 (`dataMatrixShape` NONE (ZXing order) / SQUARE / RECTANGLE), ASCII encodation | upright symbols; ASCII, C40, Text, X12, EDIFACT, Base 256 |
//! | `PDF_417` | text / byte / numeric compaction (`pdf417Compaction`, AUTO like ZXing), UTF-8 via ECI 26, error correction level 0-8 (`errorCorrectionLevel`, default 2), ZXing dimensions and rendering | row scanning in all four 90° orientations, per-row voting, RS correction with erasures, Macro PDF417 blocks skipped |
//! | `AZTEC` | compact 1-4 and full-range 4-32 layers, ZXing's 33% error correction, Upper/Lower/Digit/Punct/Mixed + Binary Shift, ECI 26 for non-Latin-1 | upright symbols via the bullseye; every mode, FLG(n) ECIs, RS correction |
//! | `CODE_128` | code sets A/B/C, automatic switching | yes |
//! | `CODE_39` | yes | yes |
//! | `EAN_13`, `UPC_A` | 12/13 (11/12) digits, check digit computed or verified | yes (an EAN-13 starting with 0 is reported as `UPC_A` unless `barcodeFormat=EAN_13`) |
//!
//! Options: `width` / `height` (default 100, ZXing rendering: the symbol is scaled by the
//! largest integer factor that fits, with a 4-module quiet zone for QR and 10 modules of side
//! margin for linear codes, centred), `imageType` (`PNG`, `GIF` (gray palette, LZW) or `JPG`
//! (baseline grayscale, quality 95)),
//! `barcodeFormat`, `errorCorrectionLevel` (`L`, `M`, `Q`, `H`).
//!
//! Unmarshal accepts any PNG (grayscale, RGB(A), palette, all bit depths, interlaced), GIF or
//! baseline JPEG (detected by signature), finds the
//! symbol (when `barcodeFormat` is the default QR_CODE every supported format is tried) and sets
//! the body to the text and the `BarcodeFormat` header (plus `ERROR_CORRECTION_LEVEL` for QR).
//! Detection targets clean, axis-aligned images such as generated ones (QR also in 90° steps
//! and mirrored); it is not a camera-image reader (no perspective correction, local
//! thresholding or damaged finder recovery).

pub mod aztec;
pub mod aztec_detect;
pub mod datamatrix;
pub mod detect;
pub mod dm_detect;
pub mod gif;
pub mod jpeg;
mod kanji_table;
pub mod oned;
pub mod pdf417;
mod pdf417_tables;
pub mod png;
pub mod qr;
pub mod qr_detect;
pub mod rs;
#[cfg(test)]
mod synth;
pub mod vision;

use std::sync::Arc;

use camel_core::dataformat::DataFormat;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::Value;

use crate::{body_bytes, body_text, Opts};

/// A matrix of modules (`true` = dark).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matrix {
    pub width: usize,
    pub height: usize,
    pub bits: Vec<bool>,
}

impl Matrix {
    pub fn new(width: usize, height: usize) -> Matrix {
        Matrix { width, height, bits: vec![false; width * height] }
    }
    pub fn get(&self, x: usize, y: usize) -> bool {
        self.bits[y * self.width + x]
    }
    pub fn set(&mut self, x: usize, y: usize, v: bool) {
        self.bits[y * self.width + x] = v;
    }
    /// Parse rows of `#`/`1` (dark) and `.`/`0`/space (light).
    pub fn from_rows(rows: &[&str]) -> Matrix {
        let h = rows.len();
        let w = rows.first().map(|r| r.chars().count()).unwrap_or(0);
        let mut m = Matrix::new(w, h);
        for (y, r) in rows.iter().enumerate() {
            for (x, c) in r.chars().enumerate() {
                m.set(x, y, c == '#' || c == '1');
            }
        }
        m
    }
}

/// Supported symbologies (ZXing `BarcodeFormat` names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    QrCode,
    DataMatrix,
    Aztec,
    Code128,
    Code39,
    Ean13,
    UpcA,
    Pdf417,
}

impl Format {
    pub fn parse(s: &str) -> Result<Format> {
        let n: String = s.chars().filter(|c| *c != '_' && *c != '-').collect::<String>().to_ascii_uppercase();
        Ok(match n.as_str() {
            "QRCODE" | "QR" => Format::QrCode,
            "DATAMATRIX" => Format::DataMatrix,
            "AZTEC" => Format::Aztec,
            "CODE128" => Format::Code128,
            "CODE39" => Format::Code39,
            "EAN13" => Format::Ean13,
            "UPCA" => Format::UpcA,
            "PDF417" => Format::Pdf417,
            _ => return Err(CamelError::illegal_argument(format!("Unsupported barcodeFormat: {} (supported: QR_CODE, DATA_MATRIX, AZTEC, PDF_417, CODE_128, CODE_39, EAN_13, UPC_A)", s))),
        })
    }
    pub fn name(&self) -> &'static str {
        match self {
            Format::QrCode => "QR_CODE",
            Format::DataMatrix => "DATA_MATRIX",
            Format::Aztec => "AZTEC",
            Format::Code128 => "CODE_128",
            Format::Code39 => "CODE_39",
            Format::Ean13 => "EAN_13",
            Format::UpcA => "UPC_A",
            Format::Pdf417 => "PDF_417",
        }
    }
}

/// Render a 2D matrix like ZXing (`quiet` modules of margin, integer scaling, centred).
pub fn render_2d(m: &Matrix, width: usize, height: usize, quiet: usize) -> png::Gray {
    let (qw, qh) = (m.width + 2 * quiet, m.height + 2 * quiet);
    let (ow, oh) = (width.max(qw), height.max(qh));
    let mult = (ow / qw).min(oh / qh).max(1);
    let left = (ow - m.width * mult) / 2;
    let top = (oh - m.height * mult) / 2;
    let mut g = png::Gray { width: ow, height: oh, pixels: vec![255; ow * oh] };
    for y in 0..m.height {
        for x in 0..m.width {
            if m.get(x, y) {
                for dy in 0..mult {
                    let row = (top + y * mult + dy) * ow;
                    g.pixels[row + left + x * mult..row + left + (x + 1) * mult].fill(0);
                }
            }
        }
    }
    g
}

/// Render a linear code like ZXing's `OneDimensionalCodeWriter` (side margin in modules).
pub fn render_1d(code: &[bool], width: usize, height: usize, margin: usize) -> png::Gray {
    let full = code.len() + margin;
    let ow = width.max(full);
    let oh = height.max(1);
    let mult = (ow / full).max(1);
    let left = (ow - code.len() * mult) / 2;
    let mut row = vec![255u8; ow];
    for (i, b) in code.iter().enumerate() {
        if *b {
            row[left + i * mult..left + (i + 1) * mult].fill(0);
        }
    }
    let mut pixels = Vec::with_capacity(ow * oh);
    for _ in 0..oh {
        pixels.extend_from_slice(&row);
    }
    png::Gray { width: ow, height: oh, pixels }
}

/// Encode `text` as an image raster.
pub fn encode(text: &str, format: Format, ecc: qr::Ecc, width: usize, height: usize) -> Result<png::Gray> {
    encode_with(text, format, ecc, width, height, datamatrix::Shape::Any)
}

/// Encode with a Data Matrix shape preference.
pub fn encode_with(text: &str, format: Format, ecc: qr::Ecc, width: usize, height: usize, shape: datamatrix::Shape) -> Result<png::Gray> {
    encode_full(text, format, ecc, width, height, shape, &pdf417::Pdf417Options::default())
}

/// Encode with every symbology specific option.
pub fn encode_full(text: &str, format: Format, ecc: qr::Ecc, width: usize, height: usize, shape: datamatrix::Shape, pdf: &pdf417::Pdf417Options) -> Result<png::Gray> {
    Ok(match format {
        Format::Pdf417 => pdf417::render(&pdf417::encode_symbol(text, pdf)?, width, height),
        Format::QrCode => render_2d(&qr::encode(text, &qr::QrOptions { ecc, version: None, mask: None })?, width, height, 4),
        Format::Aztec => render_2d(&aztec::encode(text, 33)?, width, height, 0),
        Format::DataMatrix => render_2d(&datamatrix::encode_shape(&latin1_or_utf8(text), shape)?, width, height, 0),
        Format::Code128 => render_1d(&oned::encode_code128(text)?, width, height, 10),
        Format::Code39 => render_1d(&oned::encode_code39(text)?, width, height, 10),
        Format::Ean13 => render_1d(&oned::encode_ean13(text)?, width, height, 9),
        Format::UpcA => render_1d(&oned::encode_upca(text)?, width, height, 9),
    })
}

fn latin1_or_utf8(text: &str) -> Vec<u8> {
    if text.chars().all(|c| (c as u32) < 256) {
        text.chars().map(|c| c as u8).collect()
    } else {
        text.as_bytes().to_vec()
    }
}

fn dm_text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|c| *c as char).collect(),
    }
}

/// A decode result.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    pub text: String,
    pub format: Format,
    pub ecc: Option<qr::Ecc>,
}

fn upc_or_ean(t: String, only: Option<Format>) -> Decoded {
    if t.starts_with('0') && only != Some(Format::Ean13) {
        return Decoded { text: t[1..].to_string(), format: Format::UpcA, ecc: None };
    }
    Decoded { text: t, format: Format::Ean13, ecc: None }
}

fn decode_row(row: &[bool], want: &dyn Fn(Format) -> bool, only: Option<Format>) -> Option<Decoded> {
    if want(Format::Code128) {
        if let Some(t) = oned::decode_code128(row) {
            return Some(Decoded { text: t, format: Format::Code128, ecc: None });
        }
    }
    if want(Format::Ean13) {
        if let Some(t) = oned::decode_ean13(row) {
            return Some(upc_or_ean(t, only));
        }
    }
    if want(Format::Code39) {
        if let Some(t) = oned::decode_code39(row) {
            return Some(Decoded { text: t, format: Format::Code39, ecc: None });
        }
    }
    None
}

/// The fast path for clean, generated images (global threshold, axis-aligned detectors).
fn decode_clean(bin: &detect::Bin, want: &dyn Fn(Format) -> bool, only: Option<Format>) -> Option<Decoded> {
    if want(Format::QrCode) {
        if let Some(d) = detect::find_qr(bin) {
            return Some(Decoded { text: d.text, format: Format::QrCode, ecc: Some(d.ecc) });
        }
    }
    if want(Format::DataMatrix) {
        if let Some(m) = detect::find_datamatrix(bin, &datamatrix::symbol_dims()) {
            if let Ok((b, _)) = datamatrix::decode_matrix(&m) {
                return Some(Decoded { text: dm_text(&b), format: Format::DataMatrix, ecc: None });
            }
        }
    }
    if want(Format::Aztec) {
        if let Some(t) = aztec::find_and_decode(&|x, y| bin.get(x, y), bin.width, bin.height) {
            return Some(Decoded { text: t, format: Format::Aztec, ecc: None });
        }
    }
    if want(Format::Pdf417) {
        if let Some(t) = pdf417::find_and_decode(bin) {
            return Some(Decoded { text: t, format: Format::Pdf417, ecc: None });
        }
    }
    detect::scan_rows(bin).iter().find_map(|row| decode_row(row, want, only))
}

/// Linear symbologies on band-averaged, locally thresholded scan lines (rows of the image
/// and of the image turned by 90°), ZXing `OneDReader` style: centre first, alternating
/// above and below, each line also reversed.
fn decode_linear(g: &png::Gray, want: &dyn Fn(Format) -> bool, only: Option<Format>) -> Option<Decoded> {
    for quarter in [0, 1] {
        let turned;
        let v = if quarter == 0 {
            g
        } else {
            turned = vision::rotate90(g, 1);
            &turned
        };
        let h = v.height;
        if h == 0 || v.width < 20 {
            continue;
        }
        let step = (h / 32).max(1);
        let half = (h / 120).clamp(1, 3);
        for k in 0..32usize {
            let off = (k + 1) / 2 * step;
            let y = if k % 2 == 0 { h / 2 + off } else { (h / 2).wrapping_sub(off) };
            if y >= h {
                continue;
            }
            let mut row = vision::threshold_profile(&vision::row_profile(v, y, half));
            if let Some(d) = decode_row(&row, want, only) {
                return Some(d);
            }
            row.reverse();
            if let Some(d) = decode_row(&row, want, only) {
                return Some(d);
            }
        }
    }
    None
}

/// The camera path on one (preprocessed) image and its deskewed copy.
fn decode_camera(g: &png::Gray, want: &dyn Fn(Format) -> bool, only: Option<Format>) -> Option<Decoded> {
    let angle = vision::dominant_angle(g).unwrap_or(0.0);
    let deskewed = (angle.abs() > 1.0).then(|| vision::rotate(g, -angle));
    let views: Vec<&png::Gray> = std::iter::once(g).chain(deskewed.as_ref()).collect();
    for v in &views {
        let Some(bin) = vision::hybrid(v) else { continue };
        if want(Format::QrCode) {
            if let Some(d) = qr_detect::detect_and_decode(&bin) {
                return Some(Decoded { text: d.text, format: Format::QrCode, ecc: Some(d.ecc) });
            }
        }
        if want(Format::DataMatrix) {
            for m in dm_detect::detect(&bin) {
                if let Ok((b, _)) = datamatrix::decode_matrix(&m) {
                    return Some(Decoded { text: dm_text(&b), format: Format::DataMatrix, ecc: None });
                }
            }
        }
        if want(Format::Aztec) {
            if let Some(t) = aztec_detect::detect_and_decode(&bin) {
                return Some(Decoded { text: t, format: Format::Aztec, ecc: None });
            }
        }
        if want(Format::Pdf417) {
            if let Some(t) = pdf417::find_and_decode(&bin) {
                return Some(Decoded { text: t, format: Format::Pdf417, ecc: None });
            }
        }
        if let Some(d) = decode_linear(v, want, only) {
            return Some(d);
        }
    }
    None
}

/// Decode an image; `only` restricts the symbology.
///
/// Clean generated images take a fast path (global threshold, axis-aligned sampling). Other
/// images go through the camera path: noise-adaptive smoothing, ZXing's hybrid binarizer,
/// rotation/scale/perspective tolerant detectors (QR finder + alignment patterns, Data Matrix
/// L finder by white-rectangle detection and edge fitting, Aztec bull's-eye) and, for linear
/// and stacked symbols, a copy deskewed by the dominant edge orientation.
pub fn decode(img: &png::Gray, only: Option<Format>) -> Result<Decoded> {
    let want = |f: Format| only.is_none_or(|o| o == f || (o == Format::Ean13 && f == Format::UpcA) || (o == Format::UpcA && f == Format::Ean13));
    let clean = detect::binarize(img);
    if let Some(bin) = &clean {
        if let Some(d) = decode_clean(bin, &want, only) {
            return Ok(d);
        }
    }
    if clean.is_none() && vision::hybrid(img).is_none() {
        return Err(CamelError::new("NotFoundException", "No barcode found: the image has no contrast"));
    }
    decode_camera_path(img, only)
}

/// The camera path alone (no clean-image fast path).
pub fn decode_camera_path(img: &png::Gray, only: Option<Format>) -> Result<Decoded> {
    let want = |f: Format| only.is_none_or(|o| o == f || (o == Format::Ean13 && f == Format::UpcA) || (o == Format::UpcA && f == Format::Ean13));
    let sigma = vision::noise_sigma(img);
    // noisy images are smoothed first; otherwise smoothing is the fallback (it also helps
    // against binarisation speckle on soft, out-of-focus edges)
    let smoothed = vision::smooth(img);
    let bases: Vec<std::borrow::Cow<png::Gray>> = if sigma >= 8.0 {
        vec![std::borrow::Cow::Owned(vision::smooth(&smoothed)), std::borrow::Cow::Owned(smoothed), std::borrow::Cow::Borrowed(img)]
    } else if sigma >= 3.0 {
        vec![std::borrow::Cow::Owned(smoothed), std::borrow::Cow::Borrowed(img)]
    } else {
        vec![std::borrow::Cow::Borrowed(img), std::borrow::Cow::Owned(smoothed)]
    };
    for g in &bases {
        if let Some(d) = decode_camera(g, &want, only) {
            return Ok(d);
        }
    }
    Err(CamelError::new("NotFoundException", "No barcode found in the image"))
}

/// The `barcode` data format.
pub struct BarcodeDataFormat {
    pub width: usize,
    pub height: usize,
    pub format: Format,
    /// Whether `barcodeFormat` was configured explicitly (restricts decoding).
    pub format_explicit: bool,
    pub ecc: qr::Ecc,
    pub image_type: ImageType,
    pub shape: datamatrix::Shape,
    /// PDF417 options (`errorCorrectionLevel` 0-8 for `PDF_417`, `pdf417Compaction`).
    pub pdf417: pdf417::Pdf417Options,
}

/// Output image type (`imageType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageType {
    Png,
    Gif,
    Jpg,
}

/// Write a raster in an image format.
pub fn write_image(img: &png::Gray, t: ImageType) -> Vec<u8> {
    match t {
        ImageType::Png => png::encode(img),
        ImageType::Gif => gif::encode(img),
        ImageType::Jpg => jpeg::encode(img, 95),
    }
}

/// Read a PNG, GIF or JPEG (detected from the signature).
pub fn read_image(b: &[u8]) -> Result<png::Gray> {
    if gif::is_gif(b) {
        gif::decode(b)
    } else if jpeg::is_jpeg(b) {
        jpeg::decode(b)
    } else {
        png::decode(b)
    }
}

impl DataFormat for BarcodeDataFormat {
    fn name(&self) -> &str {
        "barcode"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let text = body_text(&ex.message.body)?;
        let img = encode_full(&text, self.format, self.ecc, self.width, self.height, self.shape, &self.pdf417)?;
        ex.message.body = Value::Bytes(write_image(&img, self.image_type));
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let b = body_bytes(&ex.message.body);
        let img = read_image(&b)?;
        let only = if self.format_explicit && self.format != Format::QrCode { Some(self.format) } else { None };
        let d = decode(&img, only)?;
        ex.message.set_header("BarcodeFormat", d.format.name());
        if let Some(e) = d.ecc {
            ex.message.set_header("ERROR_CORRECTION_LEVEL", format!("{:?}", e));
        }
        ex.message.body = Value::String(d.text);
        Ok(())
    }
}

pub(crate) fn create(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    let image_type = match o.string(&["imageType"], "PNG").to_ascii_uppercase().as_str() {
        "PNG" => ImageType::Png,
        "GIF" => ImageType::Gif,
        "JPG" | "JPEG" => ImageType::Jpg,
        other => return Err(CamelError::illegal_argument(format!("Unknown barcode imageType: {} (PNG, GIF or JPG)", other))),
    };
    let shape = match o.get(&["dataMatrixShape", "shape"]) {
        Some(s) => datamatrix::Shape::parse(s).ok_or_else(|| CamelError::illegal_argument(format!("Invalid dataMatrixShape: {} (NONE, SQUARE, RECTANGLE)", s)))?,
        None => datamatrix::Shape::Any,
    };
    let fmt = o.get(&["barcodeFormat"]);
    let format = fmt.map(Format::parse).transpose()?.unwrap_or(Format::QrCode);
    let mut pdf = pdf417::Pdf417Options::default();
    let ecc = match o.get(&["errorCorrectionLevel"]) {
        // PDF417 levels are numeric (ZXing's ERROR_CORRECTION hint 0..8)
        Some(e) if format == Format::Pdf417 => {
            pdf.ec_level = match e.trim().parse::<u8>() {
                Ok(l) if l <= 8 => l,
                _ => return Err(CamelError::illegal_argument(format!("Invalid errorCorrectionLevel for PDF_417: {} (0..8)", e))),
            };
            qr::Ecc::L
        }
        Some(e) => qr::Ecc::parse(e).ok_or_else(|| CamelError::illegal_argument(format!("Invalid errorCorrectionLevel: {} (L, M, Q or H)", e)))?,
        None => qr::Ecc::L,
    };
    if let Some(c) = o.get(&["pdf417Compaction", "compaction"]) {
        pdf.compaction = pdf417::Compaction::parse(c).ok_or_else(|| CamelError::illegal_argument(format!("Invalid pdf417Compaction: {} (AUTO, TEXT, BYTE, NUMERIC)", c)))?;
    }
    let dim = |keys: &[&str]| -> Result<usize> {
        let v = o.num(keys, 100)?;
        if v <= 0 || v > 10_000 {
            return Err(CamelError::illegal_argument(format!("Barcode {} must be between 1 and 10000", keys[0])));
        }
        Ok(v as usize)
    };
    Ok(Arc::new(BarcodeDataFormat {
        width: dim(&["width"])?,
        height: dim(&["height"])?,
        format,
        format_explicit: fmt.is_some(),
        ecc,
        image_type,
        shape,
        pdf417: pdf,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(text: &str, f: Format, w: usize, h: usize) -> Decoded {
        let img = encode(text, f, qr::Ecc::M, w, h).unwrap();
        let png = png::encode(&img);
        decode(&png::decode(&png).unwrap(), None).unwrap()
    }

    #[test]
    fn every_format_through_png() {
        let qr = round("Hello Camel ✓", Format::QrCode, 100, 100);
        assert_eq!((qr.text.as_str(), qr.format, qr.ecc), ("Hello Camel ✓", Format::QrCode, Some(qr::Ecc::M)));
        assert_eq!(round("https://camel.apache.org/manual/data-format.html?x=1", Format::QrCode, 300, 200).text, "https://camel.apache.org/manual/data-format.html?x=1");
        let az = round("Aztec ✓ 2026", Format::Aztec, 120, 120);
        assert_eq!((az.text.as_str(), az.format), ("Aztec ✓ 2026", Format::Aztec));
        let long = "x".repeat(700);
        assert_eq!(round(&long, Format::Aztec, 300, 300).text, long);
        let dm = round("Data Matrix 123456", Format::DataMatrix, 100, 100);
        assert_eq!((dm.text.as_str(), dm.format), ("Data Matrix 123456", Format::DataMatrix));
        assert_eq!(round("Code128-Ab12345", Format::Code128, 100, 60).format, Format::Code128);
        assert_eq!(round("Code128-Ab12345", Format::Code128, 400, 60).text, "Code128-Ab12345");
        assert_eq!(round("CAMEL 39", Format::Code39, 100, 50).text, "CAMEL 39");
        let e = round("5901234123457", Format::Ean13, 200, 80);
        assert_eq!((e.text.as_str(), e.format), ("5901234123457", Format::Ean13));
        let u = round("03600029145", Format::UpcA, 200, 80);
        assert_eq!((u.text.as_str(), u.format), ("036000291452", Format::UpcA));
    }

    #[test]
    fn large_qr_versions_and_orientation() {
        let text: String = (0..900).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let m = qr::encode(&text, &qr::QrOptions { ecc: qr::Ecc::Q, version: None, mask: None }).unwrap();
        assert!(m.width > 100);
        let img = render_2d(&m, 0, 0, 4);
        assert_eq!(decode(&img, None).unwrap().text, text);
        // rotate the raster by 90 degrees and mirror it
        let small = render_2d(&qr::encode("rotate me", &qr::QrOptions::default()).unwrap(), 150, 150, 4);
        let mut rot = png::Gray { width: small.height, height: small.width, pixels: vec![0; small.pixels.len()] };
        for y in 0..small.height {
            for x in 0..small.width {
                rot.pixels[x * rot.width + (small.height - 1 - y)] = small.get(x, y);
            }
        }
        assert_eq!(decode(&rot, None).unwrap().text, "rotate me");
        let mut mirror = small.clone();
        for y in 0..small.height {
            mirror.pixels[y * small.width..(y + 1) * small.width].reverse();
        }
        assert_eq!(decode(&mirror, None).unwrap().text, "rotate me");
    }

    /// Real photographs from ZXing's blackbox test suite (see testdata/zxing/ATTRIBUTION.txt).
    #[test]
    fn zxing_blackbox_photographs() {
        let cases: [(&[u8], Format, &str); 14] = [
            (include_bytes!("testdata/zxing/qrcode-1_1.png"), Format::QrCode, "MEBKM:URL:http\\://en.wikipedia.org/wiki/Main_Page;;"),
            (include_bytes!("testdata/zxing/qrcode-3_01.png"), Format::QrCode, "http://arnaud.sahuguet.com/graffiti/test.php?ll=-74.00309961503218,40.74102573163046,0"),
            (include_bytes!("testdata/zxing/qrcode-3_10.png"), Format::QrCode, "MECARD:N:Google 411,;TEL:18665881077;;"),
            // byte mode without ECI holding Shift_JIS (ZXing guesses the encoding)
            (include_bytes!("testdata/zxing/qrcode-2_10.png"), Format::QrCode, "Google \u{30e2}\u{30d0}\u{30a4}\u{30eb}\r\nhttp://google.jp"),
            // half-width katakana in Shift_JIS
            (include_bytes!("testdata/zxing/qrcode-2_20.png"), Format::QrCode, "*\u{ff83}\u{ff9e}\u{ff7b}\u{ff9e}\u{ff72}\u{ff9d}QR*\r\nhttp://d-qr.net/ex/    "),
            (include_bytes!("testdata/zxing/aztec-2_10.png"), Format::Aztec, "mailto:zxing@googlegroups.com"),
            (include_bytes!("testdata/zxing/datamatrix-2_01.png"), Format::DataMatrix, "http://google.com/m"),
            (include_bytes!("testdata/zxing/datamatrix-2_10.png"), Format::DataMatrix, "This is a test of our DataMatrix support using a longer piece of text, and therefore a more dense barcode."),
            (include_bytes!("testdata/zxing/aztec-2_02.png"), Format::Aztec, "This is a real world Aztec barcode test."),
            (include_bytes!("testdata/zxing/aztec-2_18.png"), Format::Aztec, "http://code.google.com/p/zxing/source/browse/trunk/android/src/com/google/zxing/client/android/result/URIResultHandler.java"),
            (include_bytes!("testdata/zxing/pdf417-2_01.png"), Format::Pdf417, "1234567890"),
            (include_bytes!("testdata/zxing/pdf417-3_01.png"), Format::Pdf417, "A larger PDF417 barcode with text and error correction level 8."),
            (include_bytes!("testdata/zxing/code128-1_1.png"), Format::Code128, "168901"),
            (include_bytes!("testdata/zxing/ean13-1_3-gray.png"), Format::Ean13, "8480000823274"),
        ];
        for (i, (bytes, format, text)) in cases.iter().enumerate() {
            let img = read_image(bytes).unwrap();
            let d = decode(&img, None).unwrap_or_else(|e| panic!("case {}: {}", i, e));
            assert_eq!((d.format, d.text.as_str()), (*format, *text), "case {}", i);
        }
    }

    /// ZXing's rectangular Data Matrix blackbox symbols (all six ECC 200 rectangle sizes):
    /// decoded, and re-encoded module-exactly with the same symbol size.
    #[test]
    fn zxing_rectangular_data_matrix() {
        let cases: [(&[u8], usize, usize, &str); 6] = [
            (include_bytes!("testdata/zxing/datamatrix-1_abcd-18x8.png"), 8, 18, "abcde"),
            (include_bytes!("testdata/zxing/datamatrix-1_abcd-32x8.png"), 8, 32, "abcdef"),
            (include_bytes!("testdata/zxing/datamatrix-1_abcd-26x12.png"), 12, 26, "abcdefghijklm"),
            (include_bytes!("testdata/zxing/datamatrix-1_abcd-36x12.png"), 12, 36, "abcdefghijklmnopq"),
            (include_bytes!("testdata/zxing/datamatrix-1_abcd-36x16.png"), 16, 36, "abcdefghijklmnopqrstuvwxyz"),
            (include_bytes!("testdata/zxing/datamatrix-1_abcd-48x16.png"), 16, 48, "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVW"),
        ];
        for (bytes, rows, cols, text) in cases {
            let img = read_image(bytes).unwrap();
            let d = decode(&img, None).unwrap();
            assert_eq!((d.format, d.text.as_str()), (Format::DataMatrix, text));
            let sampled = detect::find_datamatrix(&detect::binarize(&img).unwrap(), &datamatrix::symbol_dims()).unwrap();
            assert_eq!((sampled.height, sampled.width), (rows, cols));
            let entry = datamatrix::entries().into_iter().find(|e| e.rows == rows && e.cols == cols).unwrap();
            let ours = datamatrix::encode_entry(text.as_bytes(), entry).unwrap();
            let (theirs_cw, corrected) = datamatrix::corrected_codewords(&sampled).unwrap();
            assert_eq!(corrected, 0);
            if (rows, cols) == (16, 36) {
                // the generator of this image wrote pad codeword 0 at position 28 where the
                // ISO/IEC 16022 253-state randomisation (and ZXing) gives 254; everything
                // else, including the EC codewords for its data, agrees
                let mut cw = datamatrix::corrected_codewords(&ours).unwrap().0;
                assert_eq!(cw[27], 254);
                cw[27] = 0;
                assert_eq!(cw[..entry.ndata], theirs_cw[..entry.ndata]);
                let fixed = datamatrix::encode_codewords(&cw[..entry.ndata], entry);
                assert_eq!(fixed, sampled);
            } else {
                assert_eq!(ours, sampled, "{}x{}", rows, cols);
            }
        }
    }

    #[test]
    fn zxing_rendering_geometry() {
        // version 1 (21 modules) + 2*4 quiet = 29; 100/29 = 3 -> 63 px symbol, padding 18
        let m = qr::encode("A", &qr::QrOptions::default()).unwrap();
        let g = render_2d(&m, 100, 100, 4);
        assert_eq!((g.width, g.height), (100, 100));
        assert_eq!(g.get(18, 18), 0);
        assert_eq!(g.get(17, 18), 255);
        assert_eq!(g.get(18 + 63 - 1, 18), 0);
        assert_eq!(g.get(18 + 63, 18), 255);
        assert!(decode(&png::Gray { width: 10, height: 10, pixels: vec![255; 100] }, None).is_err());
    }

    #[test]
    fn gif_jpg_output_and_rectangular_data_matrix() {
        for t in [ImageType::Png, ImageType::Gif, ImageType::Jpg] {
            let img = encode("image type round trip", Format::QrCode, qr::Ecc::M, 150, 150).unwrap();
            let bytes = write_image(&img, t);
            assert_eq!(decode(&read_image(&bytes).unwrap(), None).unwrap().text, "image type round trip", "{:?}", t);
            let img = encode_with("RECT1234", Format::DataMatrix, qr::Ecc::L, 200, 80, datamatrix::Shape::Rectangle).unwrap();
            let d = decode(&read_image(&write_image(&img, t)).unwrap(), None).unwrap();
            assert_eq!((d.text.as_str(), d.format), ("RECT1234", Format::DataMatrix), "{:?}", t);
        }
        assert!(write_image(&png::Gray { width: 1, height: 1, pixels: vec![0] }, ImageType::Gif).starts_with(b"GIF89a"));
    }
}
