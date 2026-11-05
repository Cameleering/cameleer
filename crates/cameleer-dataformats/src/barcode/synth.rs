//! Synthetic "camera" images for the decoder tests: a rendered symbol is projected onto a
//! sheet of paper with a perspective warp (rotation, scale, keystone), optically blurred,
//! lit unevenly, given sensor noise and JPEG-compressed with our own encoder.

use super::png::Gray;
use super::vision::{bilinear, Homography};
use super::{decode, decode_camera_path, encode_full, jpeg, pdf417, qr, datamatrix, Format};

/// Deterministic xorshift PRNG.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Approximately normal (Irwin-Hall with 12 terms).
    pub fn gauss(&mut self) -> f64 {
        (0..12).map(|_| self.unit()).sum::<f64>() - 6.0
    }
}

#[derive(Clone, Debug)]
pub struct Camera {
    /// Output image size.
    pub width: usize,
    pub height: usize,
    /// Rotation in degrees.
    pub rotate: f64,
    /// Symbol (incl. its quiet zone) width as a fraction of the output width.
    pub scale: f64,
    /// Keystone strength (fraction of the half size the far edge shrinks by).
    pub keystone: f64,
    /// Keystone direction: false = top edge shorter, true = right edge shorter.
    pub keystone_side: bool,
    /// Gaussian blur sigma in output pixels.
    pub blur: f64,
    /// Box blur radius in pixels (a (2r+1)^2 mean filter, like a defocused lens).
    pub box_blur: usize,
    /// Lighting gradient: brightness falls to this factor across the image (1 = even).
    pub light: f64,
    /// Additive noise sigma.
    pub noise: f64,
    /// JPEG quality (None = lossless).
    pub jpeg: Option<u8>,
}

impl Default for Camera {
    fn default() -> Camera {
        Camera { width: 400, height: 400, rotate: 0.0, scale: 0.7, keystone: 0.0, keystone_side: false, blur: 0.0, box_blur: 0, light: 1.0, noise: 0.0, jpeg: None }
    }
}

fn box_blur(g: &Gray, r: usize) -> Gray {
    if r == 0 {
        return g.clone();
    }
    let (w, h) = (g.width as i64, g.height as i64);
    let r = r as i64;
    let mut out = vec![0u8; g.pixels.len()];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0u32;
            for dy in -r..=r {
                for dx in -r..=r {
                    let (xx, yy) = ((x + dx).clamp(0, w - 1), (y + dy).clamp(0, h - 1));
                    s += g.pixels[(yy * w + xx) as usize] as u32;
                }
            }
            out[(y * w + x) as usize] = (s / ((2 * r + 1) * (2 * r + 1)) as u32) as u8;
        }
    }
    Gray { width: g.width, height: g.height, pixels: out }
}

fn gaussian_blur(g: &Gray, sigma: f64) -> Gray {
    if sigma <= 0.0 {
        return g.clone();
    }
    let r = (sigma * 3.0).ceil() as i64;
    let k: Vec<f64> = (-r..=r).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp()).collect();
    let ks: f64 = k.iter().sum();
    let (w, h) = (g.width as i64, g.height as i64);
    let mut tmp = vec![0f64; g.pixels.len()];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for (i, kv) in k.iter().enumerate() {
                let xx = (x + i as i64 - r).clamp(0, w - 1);
                s += kv * g.pixels[(y * w + xx) as usize] as f64;
            }
            tmp[(y * w + x) as usize] = s / ks;
        }
    }
    let mut out = vec![0u8; g.pixels.len()];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for (i, kv) in k.iter().enumerate() {
                let yy = (y + i as i64 - r).clamp(0, h - 1);
                s += kv * tmp[(yy * w + x) as usize];
            }
            out[(y * w + x) as usize] = (s / ks).round().clamp(0.0, 255.0) as u8;
        }
    }
    Gray { width: g.width, height: g.height, pixels: out }
}

/// Photograph a rendered symbol.
pub fn photograph(sym: &Gray, cam: &Camera, seed: u64) -> Gray {
    let (w, h) = (cam.width as f64, cam.height as f64);
    let sw = cam.scale * w / 2.0;
    let sh = sw * sym.height as f64 / sym.width as f64;
    let mut corners = [(-sw, -sh), (sw, -sh), (sw, sh), (-sw, sh)];
    if cam.keystone != 0.0 {
        let k = cam.keystone;
        if cam.keystone_side {
            corners[1].1 *= 1.0 - k;
            corners[2].1 *= 1.0 - k;
            corners[1].0 *= 1.0 - k / 3.0;
            corners[2].0 *= 1.0 - k / 3.0;
        } else {
            corners[0].0 *= 1.0 - k;
            corners[1].0 *= 1.0 - k;
            corners[0].1 *= 1.0 - k / 3.0;
            corners[1].1 *= 1.0 - k / 3.0;
        }
    }
    let (s, c) = cam.rotate.to_radians().sin_cos();
    let (ox, oy) = (w / 2.0 + 0.031 * w, h / 2.0 - 0.017 * h);
    let dst: Vec<(f64, f64)> = corners.iter().map(|p| (ox + c * p.0 - s * p.1, oy + s * p.0 + c * p.1)).collect();
    let src = [(0.0, 0.0), (sym.width as f64, 0.0), (sym.width as f64, sym.height as f64), (0.0, sym.height as f64)];
    let inv = Homography::from_points(&dst, &src).unwrap();
    let paper = 238.0;
    let mut out = vec![0u8; cam.width * cam.height];
    for y in 0..cam.height {
        for x in 0..cam.width {
            // 2x2 supersampling
            let mut acc = 0.0;
            for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let (u, v) = inv.map(x as f64 + dx, y as f64 + dy);
                acc += if u < 0.0 || v < 0.0 || u >= sym.width as f64 || v >= sym.height as f64 {
                    paper
                } else {
                    // printed ink is not perfectly black, paper not perfectly white
                    let l = bilinear(sym, u - 0.5, v - 0.5);
                    30.0 + l * (paper - 30.0) / 255.0
                };
            }
            out[y * cam.width + x] = (acc / 4.0).round() as u8;
        }
    }
    let mut g = box_blur(&gaussian_blur(&Gray { width: cam.width, height: cam.height, pixels: out }, cam.blur), cam.box_blur);
    let mut rng = Rng::new(seed);
    for y in 0..cam.height {
        for x in 0..cam.width {
            let i = y * cam.width + x;
            let t = (x as f64 / w * 0.7 + y as f64 / h * 0.3).clamp(0.0, 1.0);
            let light = 1.0 - (1.0 - cam.light) * t;
            let v = g.pixels[i] as f64 * light + cam.noise * rng.gauss();
            g.pixels[i] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    if let Some(q) = cam.jpeg {
        g = jpeg::decode(&jpeg::encode(&g, q)).unwrap();
    }
    g
}

/// The distortion matrix used by the tests: (name, camera).
pub fn distortions() -> Vec<(&'static str, Camera)> {
    let base = Camera::default();
    vec![
        ("rotate 17", Camera { rotate: 17.0, ..base.clone() }),
        ("rotate -38 small", Camera { rotate: -38.0, scale: 0.5, ..base.clone() }),
        ("rotate 123", Camera { rotate: 123.0, ..base.clone() }),
        ("keystone top", Camera { keystone: 0.22, rotate: 4.0, ..base.clone() }),
        ("keystone side + rotate 17", Camera { keystone: 0.2, keystone_side: true, rotate: 17.0, ..base.clone() }),
        ("blur 1.2", Camera { blur: 1.2, rotate: 7.0, ..base.clone() }),
        ("uneven light", Camera { light: 0.45, rotate: -11.0, ..base.clone() }),
        ("noise 12", Camera { noise: 12.0, rotate: 21.0, ..base.clone() }),
        ("jpeg 60", Camera { jpeg: Some(60), rotate: 13.0, keystone: 0.1, ..base.clone() }),
        ("box blur 1 + rotate 61", Camera { box_blur: 1, rotate: 61.0, ..base.clone() }),
        ("scaled down 0.4 + rotate -24", Camera { scale: 0.4, rotate: -24.0, noise: 4.0, ..base.clone() }),
        ("noise 18 + light", Camera { noise: 18.0, light: 0.7, rotate: 8.0, ..base.clone() }),
        ("everything", Camera { rotate: 17.0, keystone: 0.15, blur: 0.9, light: 0.6, noise: 7.0, jpeg: Some(60), ..base.clone() }),
    ]
}

/// Render a symbol with small modules (quiet zone included) as the "printed" source.
pub fn printed(text: &str, f: Format) -> Gray {
    let pdf = pdf417::Pdf417Options::default();
    let (w, h) = match f {
        f if is_linear(f) => (600, 200),
        Format::Pdf417 => (600, 300),
        _ => (240, 240),
    };
    let img = encode_full(text, f, qr::Ecc::M, w, h, datamatrix::Shape::Any, &pdf).unwrap();
    // 2D symbols without quiet zone (Aztec, Data Matrix) get a margin like a printed label
    let m = match f {
        Format::Aztec | Format::DataMatrix => img.width / 8,
        _ => 0,
    };
    let mut out = Gray { width: img.width + 2 * m, height: img.height + 2 * m, pixels: vec![255; (img.width + 2 * m) * (img.height + 2 * m)] };
    for y in 0..img.height {
        for x in 0..img.width {
            out.pixels[(y + m) * out.width + x + m] = img.pixels[y * img.width + x];
        }
    }
    out
}

/// Whether `f` is a linear (1D) symbology (photographed on a wide image).
pub fn is_linear(f: Format) -> bool {
    use Format::*;
    matches!(f, Code128 | Code39 | Code93 | Codabar | Itf | Ean13 | Ean8 | UpcA | UpcE | UpcEanExtension | Rss14 | RssExpanded)
}

/// Render linear modules like the writers do (for symbologies without a writer).
pub fn printed_modules(m: &[bool]) -> Gray {
    super::render_1d(m, 600, 200, 10)
}

/// Decode every distortion; returns the names that failed.
pub fn failures(text: &str, f: Format, only: Option<Format>) -> Vec<String> {
    failures_of(&printed(text, f), is_linear(f) || f == Format::Pdf417, text, f, only)
}

/// Decode every distortion of a printed source image; the expected format is checked too.
pub fn failures_of(src: &Gray, wide: bool, text: &str, f: Format, only: Option<Format>) -> Vec<String> {
    let mut failed = Vec::new();
    for (i, (name, cam)) in distortions().into_iter().enumerate() {
        let cam = if wide { Camera { width: 640, height: 440, scale: 0.75, ..cam } } else { cam };
        let img = photograph(src, &cam, 1000 + i as u64);
        match decode_camera_path(&img, only) {
            Ok(d) if d.text == text && (d.format == f || (f == Format::UpcA && d.format == Format::Ean13)) => {
                // the public entry point must agree
                assert_eq!(decode(&img, only).unwrap().text, text, "{}", name);
            }
            Ok(d) => failed.push(format!("{}: wrong result {:?} {:?}", name, d.format, d.text)),
            Err(e) => failed.push(format!("{}: {}", name, e)),
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_qr() {
        let f = failures("https://camel.apache.org/components/latest/dataformats/barcode-dataformat.html", Format::QrCode, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_qr_large_version() {
        // version 10 (alignment patterns, version information) with ECC Q
        let text: String = (0..150).map(|i| (b'A' + (i % 26) as u8) as char).collect::<String>() + " camel";
        let f = failures(&text, Format::QrCode, Some(Format::QrCode));
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_data_matrix_multi_region() {
        let f = failures("Data Matrix with four data regions: 0123456789 ABCDEFGHIJ", Format::DataMatrix, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_aztec_full_range() {
        let text = "Full-range Aztec symbol with more layers: 0123456789 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789 the quick brown fox";
        assert!(crate::barcode::aztec::encode(text, 33).unwrap().width > 27, "full range expected");
        let f = failures(text, Format::Aztec, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    /// Every 2D symbology through a sweep of arbitrary rotation angles with mild keystone.
    #[test]
    fn rotation_sweep() {
        for (text, f) in [("rotation sweep QR", Format::QrCode), ("rotation sweep DM", Format::DataMatrix), ("rotation sweep Aztec", Format::Aztec)] {
            let src = printed(text, f);
            for k in 0..14 {
                let angle = k as f64 * 13.0 + 2.0;
                let cam = Camera { rotate: angle, keystone: 0.08, blur: 0.6, noise: 3.0, ..Camera::default() };
                let img = photograph(&src, &cam, k);
                let d = decode_camera_path(&img, None).unwrap_or_else(|e| panic!("{:?} at {}: {}", f, angle, e));
                assert_eq!((d.format, d.text.as_str()), (f, text), "{}", angle);
            }
        }
    }

    #[test]
    fn camera_data_matrix() {
        let f = failures("Camel Data Matrix 2026", Format::DataMatrix, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_aztec() {
        let f = failures("Aztec camera test 0123456789", Format::Aztec, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_pdf417() {
        let f = failures("PDF417 camera 2026", Format::Pdf417, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_code128() {
        let f = failures("CAMEL-128-x9", Format::Code128, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_ean13() {
        let f = failures("5901234123457", Format::Ean13, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_codabar() {
        let f = failures("31117013206375", Format::Codabar, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_code93() {
        let f = failures("CAMEL-93/x", Format::Code93, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_ean8() {
        let f = failures("96385074", Format::Ean8, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_itf() {
        let f = failures("30712345000010", Format::Itf, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_upce() {
        let f = failures("01234565", Format::UpcE, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_upc_ean_extension() {
        let mut m = crate::barcode::oned::encode_ean13("978073520044").unwrap();
        m.extend(vec![false; 9]);
        m.extend(crate::barcode::linear::encode_extension("51299").unwrap());
        let src = printed_modules(&m);
        let f = failures_of(&src, true, "9780735200449", Format::Ean13, Some(Format::UpcEanExtension));
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_maxicode() {
        use crate::barcode::maxicode::encode;
        let text = "MaxiCode camera test 0123456789";
        let m = encode::matrix(&encode::codewords(&encode::datawords(text, 4)));
        let src = encode::render(&m, 8, 24);
        let f = failures_of(&src, false, text, Format::MaxiCode, None);
        // ZXing reads MaxiCode only from pure images; our photograph detector handles
        // rotation, scale, blur, noise, lighting, JPEG and mild perspective (keystone 0.1),
        // not the strong keystone cases (0.15 and more)
        let strong_keystone = ["keystone top", "keystone side + rotate 17", "everything"];
        let unexpected: Vec<&String> = f.iter().filter(|e| !strong_keystone.iter().any(|k| e.starts_with(&format!("{}:", k)))).collect();
        assert!(unexpected.is_empty(), "{:#?}", unexpected);
    }

    #[test]
    fn camera_rss14() {
        let src = printed_modules(&crate::barcode::rss::encode::rss14("2001234567890"));
        let f = failures_of(&src, true, "20012345678909", Format::Rss14, None);
        assert!(f.is_empty(), "{:#?}", f);
    }

    #[test]
    fn camera_rss_expanded() {
        let src = printed_modules(&crate::barcode::rss::encode::rss_expanded_3103("001234567890", 1750));
        let f = failures_of(&src, true, "(01)90012345678908(3103)001750", Format::RssExpanded, None);
        assert!(f.is_empty(), "{:#?}", f);
    }
}
