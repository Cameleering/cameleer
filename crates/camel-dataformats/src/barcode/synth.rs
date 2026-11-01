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
    /// Lighting gradient: brightness falls to this factor across the image (1 = even).
    pub light: f64,
    /// Additive noise sigma.
    pub noise: f64,
    /// JPEG quality (None = lossless).
    pub jpeg: Option<u8>,
}

impl Default for Camera {
    fn default() -> Camera {
        Camera { width: 400, height: 400, rotate: 0.0, scale: 0.7, keystone: 0.0, keystone_side: false, blur: 0.0, light: 1.0, noise: 0.0, jpeg: None }
    }
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
    let mut g = gaussian_blur(&Gray { width: cam.width, height: cam.height, pixels: out }, cam.blur);
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
        ("everything", Camera { rotate: 17.0, keystone: 0.15, blur: 0.9, light: 0.6, noise: 7.0, jpeg: Some(60), ..base.clone() }),
    ]
}

/// Render a symbol with small modules (quiet zone included) as the "printed" source.
pub fn printed(text: &str, f: Format) -> Gray {
    let pdf = pdf417::Pdf417Options::default();
    let (w, h) = match f {
        Format::Code128 | Format::Ean13 | Format::UpcA | Format::Code39 => (600, 200),
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

/// Decode every distortion; returns the names that failed.
pub fn failures(text: &str, f: Format, only: Option<Format>) -> Vec<String> {
    let src = printed(text, f);
    let mut failed = Vec::new();
    for (i, (name, cam)) in distortions().into_iter().enumerate() {
        let cam = match f {
            Format::Code128 | Format::Ean13 | Format::Pdf417 | Format::Code39 | Format::UpcA => Camera { width: 640, height: 440, scale: 0.75, ..cam },
            _ => cam,
        };
        let img = photograph(&src, &cam, 1000 + i as u64);
        match decode_camera_path(&img, only) {
            Ok(d) if d.text == text => {
                // the public entry point must agree
                assert_eq!(decode(&img, only).unwrap().text, text, "{}", name);
            }
            Ok(d) => failed.push(format!("{}: wrong text {:?}", name, d.text)),
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
}

#[cfg(test)]
mod debug {
    use super::*;
    use crate::barcode::{aztec_detect, vision};

    #[test]
    #[ignore]
    fn debug_dir() {
        let dir = std::env::var("BARCODE_DIR").unwrap();
        let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        names.sort();
        for p in names {
            let b = std::fs::read(&p).unwrap();
            let t = std::time::Instant::now();
            let r = crate::barcode::read_image(&b).and_then(|g| decode(&g, None));
            eprintln!("{:?} {:?} {:?}", p.file_name().unwrap(), t.elapsed(), r.map(|d| (d.format, d.text)));
        }
    }

    #[test]
    #[ignore]
    fn debug_aztec_file() {
        let b = std::fs::read(std::env::var("BARCODE_FILE").unwrap()).unwrap();
        let g = crate::barcode::read_image(&b).unwrap();
        eprintln!("sigma {}", vision::noise_sigma(&g));
        let bin = vision::hybrid(&g).unwrap();
        aztec_detect::debug(&bin);
    }

    #[test]
    #[ignore]
    fn debug_aztec() {
        let src = printed("Aztec camera test 0123456789", Format::Aztec);
        for rot in [0.0, 17.0] {
            let img = photograph(&src, &Camera { rotate: rot, ..Camera::default() }, 1);
            let bin = vision::hybrid(&img).unwrap();
            aztec_detect::debug(&bin);
        }
    }
}
