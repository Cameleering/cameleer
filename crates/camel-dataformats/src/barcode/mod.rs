//! `barcode` data format (Camel `camel-barcode`, ZXing based in Java): marshal text into a
//! barcode image, unmarshal an image back to text.
//!
//! Symbologies (`barcodeFormat`, default `QR_CODE`):
//!
//! | format | encode | decode |
//! |---|---|---|
//! | `QR_CODE` | versions 1-40, ECC L/M/Q/H (`errorCorrectionLevel`, default L like ZXing), numeric / alphanumeric / byte (UTF-8 with ECI 26 for non-ASCII), RS, mask by penalty rules | finder patterns, sampling, format/version info, unmasking, RS correction |
//! | `DATA_MATRIX` | ECC 200 square 10x10 .. 144x144, ASCII encodation | upright symbols; ASCII, C40, Text, X12, EDIFACT, Base 256 |
//! | `CODE_128` | code sets A/B/C, automatic switching | yes |
//! | `CODE_39` | yes | yes |
//! | `EAN_13`, `UPC_A` | 12/13 (11/12) digits, check digit computed or verified | yes (an EAN-13 starting with 0 is reported as `UPC_A` unless `barcodeFormat=EAN_13`) |
//!
//! Options: `width` / `height` (default 100, ZXing rendering: the symbol is scaled by the
//! largest integer factor that fits, with a 4-module quiet zone for QR and 10 modules of side
//! margin for linear codes, centred), `imageType` (`PNG`; `GIF` / `JPG` are rejected),
//! `barcodeFormat`, `errorCorrectionLevel` (`L`, `M`, `Q`, `H`).
//!
//! Unmarshal accepts any PNG (grayscale, RGB(A), palette, all bit depths, interlaced), finds the
//! symbol (when `barcodeFormat` is the default QR_CODE every supported format is tried) and sets
//! the body to the text and the `BarcodeFormat` header (plus `ERROR_CORRECTION_LEVEL` for QR).
//! Detection targets clean, axis-aligned images such as generated ones (QR also in 90° steps
//! and mirrored); it is not a camera-image reader (no perspective correction, local
//! thresholding or damaged finder recovery).

pub mod datamatrix;
pub mod detect;
pub mod oned;
pub mod png;
pub mod qr;
pub mod rs;

use std::sync::Arc;

use camel_core::dataformat::DataFormat;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::Value;

use crate::{body_bytes, body_text, df_err, Opts};

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
    Code128,
    Code39,
    Ean13,
    UpcA,
}

impl Format {
    pub fn parse(s: &str) -> Result<Format> {
        let n: String = s.chars().filter(|c| *c != '_' && *c != '-').collect::<String>().to_ascii_uppercase();
        Ok(match n.as_str() {
            "QRCODE" | "QR" => Format::QrCode,
            "DATAMATRIX" => Format::DataMatrix,
            "CODE128" => Format::Code128,
            "CODE39" => Format::Code39,
            "EAN13" => Format::Ean13,
            "UPCA" => Format::UpcA,
            _ => return Err(CamelError::illegal_argument(format!("Unsupported barcodeFormat: {} (supported: QR_CODE, DATA_MATRIX, CODE_128, CODE_39, EAN_13, UPC_A)", s))),
        })
    }
    pub fn name(&self) -> &'static str {
        match self {
            Format::QrCode => "QR_CODE",
            Format::DataMatrix => "DATA_MATRIX",
            Format::Code128 => "CODE_128",
            Format::Code39 => "CODE_39",
            Format::Ean13 => "EAN_13",
            Format::UpcA => "UPC_A",
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
    Ok(match format {
        Format::QrCode => render_2d(&qr::encode(text, &qr::QrOptions { ecc, version: None, mask: None })?, width, height, 4),
        Format::DataMatrix => render_2d(&datamatrix::encode(&latin1_or_utf8(text), None)?, width, height, 0),
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

/// Decode an image; `only` restricts the symbology.
pub fn decode(img: &png::Gray, only: Option<Format>) -> Result<Decoded> {
    let bin = detect::binarize(img).ok_or_else(|| CamelError::new("NotFoundException", "No barcode found: the image has no contrast"))?;
    let want = |f: Format| only.is_none_or(|o| o == f || (o == Format::Ean13 && f == Format::UpcA) || (o == Format::UpcA && f == Format::Ean13));
    if want(Format::QrCode) {
        if let Some(d) = detect::find_qr(&bin) {
            return Ok(Decoded { text: d.text, format: Format::QrCode, ecc: Some(d.ecc) });
        }
    }
    if want(Format::DataMatrix) {
        if let Some(m) = detect::find_datamatrix(&bin, &datamatrix::symbol_sizes()) {
            if let Ok((b, _)) = datamatrix::decode_matrix(&m) {
                return Ok(Decoded { text: dm_text(&b), format: Format::DataMatrix, ecc: None });
            }
        }
    }
    for row in detect::scan_rows(&bin) {
        if want(Format::Code128) {
            if let Some(t) = oned::decode_code128(&row) {
                return Ok(Decoded { text: t, format: Format::Code128, ecc: None });
            }
        }
        if want(Format::Ean13) {
            if let Some(t) = oned::decode_ean13(&row) {
                if t.starts_with('0') && only != Some(Format::Ean13) {
                    return Ok(Decoded { text: t[1..].to_string(), format: Format::UpcA, ecc: None });
                }
                return Ok(Decoded { text: t, format: Format::Ean13, ecc: None });
            }
        }
        if want(Format::Code39) {
            if let Some(t) = oned::decode_code39(&row) {
                return Ok(Decoded { text: t, format: Format::Code39, ecc: None });
            }
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
}

impl DataFormat for BarcodeDataFormat {
    fn name(&self) -> &str {
        "barcode"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let text = body_text(&ex.message.body)?;
        let img = encode(&text, self.format, self.ecc, self.width, self.height)?;
        ex.message.body = Value::Bytes(png::encode(&img));
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let b = body_bytes(&ex.message.body);
        let img = png::decode(&b)?;
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
    let image_type = o.string(&["imageType"], "PNG").to_ascii_uppercase();
    match image_type.as_str() {
        "PNG" => {}
        "GIF" | "JPG" | "JPEG" => return Err(df_err(format!("Barcode imageType {} is not supported (only PNG)", image_type))),
        other => return Err(CamelError::illegal_argument(format!("Unknown barcode imageType: {}", other))),
    }
    let fmt = o.get(&["barcodeFormat"]);
    let ecc = match o.get(&["errorCorrectionLevel"]) {
        Some(e) => qr::Ecc::parse(e).ok_or_else(|| CamelError::illegal_argument(format!("Invalid errorCorrectionLevel: {} (L, M, Q or H)", e)))?,
        None => qr::Ecc::L,
    };
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
        format: fmt.map(Format::parse).transpose()?.unwrap_or(Format::QrCode),
        format_explicit: fmt.is_some(),
        ecc,
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
}
