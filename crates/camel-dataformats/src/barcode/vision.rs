//! Image processing for camera-like barcode images: ZXing's `HybridBinarizer` (8x8 block
//! black points, 5x5 block neighbourhood averaging) with the `GlobalHistogramBinarizer`
//! fallback for small images, noise estimation and noise-adaptive smoothing, dominant edge
//! orientation (to deskew linear and stacked symbols), bilinear rotation, perspective
//! transforms (ZXing `PerspectiveTransform`, here as a least-squares homography that also
//! accepts more than four point pairs) and locally thresholded scan-line profiles.

use super::detect::Bin;
use super::png::Gray;

// ---------------------------------------------------------------------------------------
// Binarisation

const BLOCK: usize = 8;
const MIN_DIMENSION: usize = BLOCK * 5;
const MIN_DYNAMIC_RANGE: u32 = 24;

/// ZXing `GlobalHistogramBinarizer.estimateBlackPoint` over 32 luminance buckets.
fn estimate_black_point(buckets: &[u32; 32]) -> Option<u32> {
    let n = buckets.len();
    let (mut max_count, mut first_peak, mut first_size) = (0u32, 0usize, 0u32);
    for (x, &b) in buckets.iter().enumerate() {
        if b > first_size {
            first_peak = x;
            first_size = b;
        }
        max_count = max_count.max(b);
    }
    let (mut second_peak, mut second_score) = (0usize, 0u64);
    for (x, &b) in buckets.iter().enumerate() {
        let d = x.abs_diff(first_peak) as u64;
        let score = b as u64 * d * d;
        if score > second_score {
            second_peak = x;
            second_score = score;
        }
    }
    let (lo, hi) = if first_peak > second_peak { (second_peak, first_peak) } else { (first_peak, second_peak) };
    if hi - lo <= n / 16 {
        return None;
    }
    let (mut best_valley, mut best_score) = (hi - 1, -1i64);
    let mut x = hi - 1;
    while x > lo {
        let from_first = (x - lo) as i64;
        let score = from_first * from_first * (hi - x) as i64 * (max_count - buckets[x]) as i64;
        if score > best_score {
            best_valley = x;
            best_score = score;
        }
        x -= 1;
    }
    Some((best_valley as u32) << 3)
}

/// ZXing `GlobalHistogramBinarizer.getBlackMatrix` (histogram of five sampled rows).
pub fn global_histogram(g: &Gray) -> Option<Bin> {
    let (w, h) = (g.width, g.height);
    if w == 0 || h == 0 {
        return None;
    }
    let mut buckets = [0u32; 32];
    for y in 1..5 {
        let row = h * y / 5;
        for x in w / 5..(w * 4 / 5).max(w / 5 + 1).min(w) {
            buckets[(g.get(x, row.min(h - 1)) >> 3) as usize] += 1;
        }
    }
    let bp = estimate_black_point(&buckets)?;
    Some(Bin { width: w, height: h, bits: g.pixels.iter().map(|p| (*p as u32) < bp).collect() })
}

/// ZXing `HybridBinarizer.getBlackMatrix`; falls back to the global histogram binarizer
/// below 40x40 pixels. `None` when the image has no usable contrast.
pub fn hybrid(g: &Gray) -> Option<Bin> {
    let (w, h) = (g.width, g.height);
    if w < MIN_DIMENSION || h < MIN_DIMENSION {
        return global_histogram(g);
    }
    let (min, max) = g.pixels.iter().fold((255u8, 0u8), |(a, b), p| (a.min(*p), b.max(*p)));
    if max - min < 32 {
        return None;
    }
    let sub_w = w.div_ceil(BLOCK);
    let sub_h = h.div_ceil(BLOCK);
    let lum = &g.pixels;
    // black points
    let mut bp = vec![0u32; sub_w * sub_h];
    let (max_yo, max_xo) = (h - BLOCK, w - BLOCK);
    for y in 0..sub_h {
        let yo = (y * BLOCK).min(max_yo);
        for x in 0..sub_w {
            let xo = (x * BLOCK).min(max_xo);
            let (mut sum, mut mn, mut mx) = (0u32, 255u32, 0u32);
            for yy in 0..BLOCK {
                let off = (yo + yy) * w + xo;
                for &p in &lum[off..off + BLOCK] {
                    let p = p as u32;
                    sum += p;
                    mn = mn.min(p);
                    mx = mx.max(p);
                }
            }
            let mut average = sum >> 6;
            if mx - mn <= MIN_DYNAMIC_RANGE {
                // a flat block: assume white unless the neighbours say it is dark
                average = mn / 2;
                if y > 0 && x > 0 {
                    let nb = (bp[(y - 1) * sub_w + x] + 2 * bp[y * sub_w + x - 1] + bp[(y - 1) * sub_w + x - 1]) / 4;
                    if mn < nb {
                        average = nb;
                    }
                }
            }
            bp[y * sub_w + x] = average;
        }
    }
    // thresholds from 5x5 block neighbourhoods
    let cap = |v: usize, m: usize| if v < 2 { 2 } else { v.min(m) };
    let mut bits = vec![false; w * h];
    for y in 0..sub_h {
        let yo = (y * BLOCK).min(max_yo);
        let top = cap(y, sub_h - 3);
        for x in 0..sub_w {
            let xo = (x * BLOCK).min(max_xo);
            let left = cap(x, sub_w - 3);
            let mut sum = 0u32;
            for z in 0..5 {
                let row = (top + z - 2) * sub_w;
                sum += bp[row + left - 2..=row + left + 2].iter().sum::<u32>();
            }
            let thr = sum / 25;
            for yy in 0..BLOCK {
                let off = (yo + yy) * w + xo;
                for xx in 0..BLOCK {
                    bits[off + xx] = (lum[off + xx] as u32) <= thr;
                }
            }
        }
    }
    Some(Bin { width: w, height: h, bits })
}

// ---------------------------------------------------------------------------------------
// Noise and smoothing

/// Estimate the standard deviation of additive noise (Immerkær's Laplacian-difference
/// operator, taken at the 25th percentile so that edges do not count).
pub fn noise_sigma(g: &Gray) -> f64 {
    let (w, h) = (g.width, g.height);
    if w < 3 || h < 3 {
        return 0.0;
    }
    let mut v: Vec<u32> = Vec::with_capacity((w - 2) * (h - 2) / 4 + 1);
    let p = |x: usize, y: usize| g.pixels[y * w + x] as i32;
    for y in (1..h - 1).step_by(2) {
        for x in (1..w - 1).step_by(2) {
            let s = p(x - 1, y - 1) + p(x + 1, y - 1) + p(x - 1, y + 1) + p(x + 1, y + 1) - 2 * (p(x, y - 1) + p(x, y + 1) + p(x - 1, y) + p(x + 1, y)) + 4 * p(x, y);
            v.push(s.unsigned_abs());
        }
    }
    if v.is_empty() {
        return 0.0;
    }
    let k = v.len() / 4;
    let (_, q, _) = v.select_nth_unstable(k);
    // the 25th percentile of |N(0, 6 sigma)| is 0.3186 * 6 sigma
    *q as f64 / 1.9116
}

/// Separable [1 2 1] / 4 smoothing (edges replicated).
pub fn smooth(g: &Gray) -> Gray {
    let (w, h) = (g.width, g.height);
    if w < 3 || h < 3 {
        return g.clone();
    }
    let mut tmp = vec![0u16; w * h];
    for y in 0..h {
        let row = &g.pixels[y * w..(y + 1) * w];
        for x in 0..w {
            let a = row[x.saturating_sub(1)] as u16;
            let c = row[(x + 1).min(w - 1)] as u16;
            tmp[y * w + x] = a + 2 * row[x] as u16 + c;
        }
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let (ya, yc) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for x in 0..w {
            let s = tmp[ya * w + x] + 2 * tmp[y * w + x] + tmp[yc * w + x];
            out[y * w + x] = ((s + 8) / 16) as u8;
        }
    }
    Gray { width: w, height: h, pixels: out }
}

// ---------------------------------------------------------------------------------------
// Orientation and rotation

/// Bilinear sample with edge replication.
pub fn bilinear(g: &Gray, x: f64, y: f64) -> f64 {
    let (w, h) = (g.width as f64, g.height as f64);
    let x = x.clamp(0.0, w - 1.0);
    let y = y.clamp(0.0, h - 1.0);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(g.width - 1), (y0 + 1).min(g.height - 1));
    let (fx, fy) = (x - x0 as f64, y - y0 as f64);
    let p = |x: usize, y: usize| g.pixels[y * g.width + x] as f64;
    let top = p(x0, y0) * (1.0 - fx) + p(x1, y0) * fx;
    let bot = p(x0, y1) * (1.0 - fx) + p(x1, y1) * fx;
    top * (1.0 - fy) + bot * fy
}

/// The dominant edge orientation in degrees (Scharr gradients of a smoothed copy, histogram
/// of gradient directions modulo 90° weighted by magnitude), in (-45, 45]: the image content is rotated by
/// this angle (modulo 90°) relative to the pixel axes. `None` for images without edges.
pub fn dominant_angle(g: &Gray) -> Option<f64> {
    let (w, h) = (g.width, g.height);
    if w < 8 || h < 8 {
        return None;
    }
    let g = &smooth(g);
    let mut hist = [0f64; 90];
    let p = |x: usize, y: usize| g.pixels[y * w + x] as f64;
    let mut total = 0.0;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            // Scharr kernels (better rotational symmetry than Sobel)
            let gx = 3.0 * (p(x + 1, y - 1) + p(x + 1, y + 1) - p(x - 1, y - 1) - p(x - 1, y + 1)) + 10.0 * (p(x + 1, y) - p(x - 1, y));
            let gy = 3.0 * (p(x - 1, y + 1) + p(x + 1, y + 1) - p(x - 1, y - 1) - p(x + 1, y - 1)) + 10.0 * (p(x, y + 1) - p(x, y - 1));
            let m2 = gx * gx + gy * gy;
            if m2 < 400.0 * 400.0 {
                continue;
            }
            let a = gy.atan2(gx).to_degrees().rem_euclid(90.0);
            let m = m2.sqrt();
            // split between the two nearest bins
            let f = a - 0.5;
            let b0 = f.floor();
            let t = f - b0;
            let i0 = (b0 as i64).rem_euclid(90) as usize;
            hist[i0] += m * (1.0 - t);
            hist[(i0 + 1) % 90] += m * t;
            total += m;
        }
    }
    if total == 0.0 {
        return None;
    }
    let mut sm = [0f64; 90];
    for (i, s) in sm.iter_mut().enumerate() {
        for (k, wgt) in [(-2i64, 1.0), (-1, 2.0), (0, 3.0), (1, 2.0), (2, 1.0)] {
            *s += wgt * hist[((i as i64 + k).rem_euclid(90)) as usize];
        }
    }
    let (best, _) = sm.iter().enumerate().fold((0, f64::MIN), |a, (i, v)| if *v > a.1 { (i, *v) } else { a });
    let (l, c, r) = (sm[(best + 89) % 90], sm[best], sm[(best + 1) % 90]);
    let den = l - 2.0 * c + r;
    let off = if den.abs() > 1e-9 { (0.5 * (l - r) / den).clamp(-0.5, 0.5) } else { 0.0 };
    let mut a = best as f64 + 0.5 + off;
    if a > 45.0 {
        a -= 90.0;
    }
    Some(a)
}

/// Rotate an image by `deg` degrees (content turns clockwise on screen for positive angles,
/// pixel y pointing down) into its bounding box, bilinear, edges replicated.
pub fn rotate(g: &Gray, deg: f64) -> Gray {
    let (s, c) = deg.to_radians().sin_cos();
    let (w, h) = (g.width as f64, g.height as f64);
    let nw = (w * c.abs() + h * s.abs()).ceil() as usize;
    let nh = (w * s.abs() + h * c.abs()).ceil() as usize;
    let (cx, cy) = (w / 2.0, h / 2.0);
    let (ncx, ncy) = (nw as f64 / 2.0, nh as f64 / 2.0);
    let mut pixels = vec![0u8; nw * nh];
    for y in 0..nh {
        for x in 0..nw {
            let (dx, dy) = (x as f64 + 0.5 - ncx, y as f64 + 0.5 - ncy);
            // inverse rotation
            let sx = c * dx + s * dy + cx - 0.5;
            let sy = -s * dx + c * dy + cy - 0.5;
            pixels[y * nw + x] = bilinear(g, sx, sy).round() as u8;
        }
    }
    Gray { width: nw, height: nh, pixels }
}

/// Rotate by a multiple of 90° (clockwise quarter turns).
pub fn rotate90(g: &Gray, quarter: usize) -> Gray {
    match quarter % 4 {
        0 => g.clone(),
        2 => Gray { width: g.width, height: g.height, pixels: g.pixels.iter().rev().copied().collect() },
        q => {
            let (w, h) = (g.width, g.height);
            let mut p = vec![0u8; w * h];
            for y in 0..h {
                for x in 0..w {
                    let (nx, ny) = if q == 1 { (h - 1 - y, x) } else { (y, w - 1 - x) };
                    p[ny * h + nx] = g.pixels[y * w + x];
                }
            }
            Gray { width: h, height: w, pixels: p }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Perspective transform

/// A plane projective transform (homography) `(x, y) -> ((a x + b y + c) / (g x + h y + 1),
/// (d x + e y + f) / (g x + h y + 1))`.
#[derive(Clone, Copy, Debug)]
pub struct Homography {
    m: [f64; 9],
}

fn normalisation(p: &[(f64, f64)]) -> [f64; 9] {
    let n = p.len() as f64;
    let (mx, my) = p.iter().fold((0.0, 0.0), |a, q| (a.0 + q.0 / n, a.1 + q.1 / n));
    let d = p.iter().map(|q| ((q.0 - mx).powi(2) + (q.1 - my).powi(2)).sqrt()).sum::<f64>() / n;
    let s = if d > 1e-12 { std::f64::consts::SQRT_2 / d } else { 1.0 };
    [s, 0.0, -s * mx, 0.0, s, -s * my, 0.0, 0.0, 1.0]
}

fn mul3(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    let mut r = [0.0; 9];
    for i in 0..3 {
        for j in 0..3 {
            r[i * 3 + j] = (0..3).map(|k| a[i * 3 + k] * b[k * 3 + j]).sum();
        }
    }
    r
}

fn inv3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let det = m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6]) + m[2] * (m[3] * m[7] - m[4] * m[6]);
    if det.abs() < 1e-15 {
        return None;
    }
    let i = 1.0 / det;
    Some([
        (m[4] * m[8] - m[5] * m[7]) * i,
        (m[2] * m[7] - m[1] * m[8]) * i,
        (m[1] * m[5] - m[2] * m[4]) * i,
        (m[5] * m[6] - m[3] * m[8]) * i,
        (m[0] * m[8] - m[2] * m[6]) * i,
        (m[2] * m[3] - m[0] * m[5]) * i,
        (m[3] * m[7] - m[4] * m[6]) * i,
        (m[1] * m[6] - m[0] * m[7]) * i,
        (m[0] * m[4] - m[1] * m[3]) * i,
    ])
}

/// Solve an n x n linear system (row-major augmented matrix) by Gaussian elimination.
fn solve(mut a: Vec<Vec<f64>>) -> Option<Vec<f64>> {
    let n = a.len();
    for col in 0..n {
        let piv = (col..n).max_by(|i, j| a[*i][col].abs().partial_cmp(&a[*j][col].abs()).unwrap())?;
        if a[piv][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, piv);
        for r in 0..n {
            if r != col {
                let f = a[r][col] / a[col][col];
                if f != 0.0 {
                    for c in col..=n {
                        a[r][c] -= f * a[col][c];
                    }
                }
            }
        }
    }
    Some((0..n).map(|i| a[i][n] / a[i][i]).collect())
}

impl Homography {
    /// The transform mapping `src[i]` to `dst[i]` (exact for four pairs, least squares in
    /// the algebraic error for more; coordinates are normalised first).
    pub fn from_points(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<Homography> {
        if src.len() < 4 || src.len() != dst.len() {
            return None;
        }
        let ts = normalisation(src);
        let td = normalisation(dst);
        let ap = |t: &[f64; 9], p: (f64, f64)| (t[0] * p.0 + t[2], t[4] * p.1 + t[5]);
        let mut ata = vec![vec![0.0; 9]; 8];
        for (s, d) in src.iter().zip(dst) {
            let (x, y) = ap(&ts, *s);
            let (u, v) = ap(&td, *d);
            for (row, rhs) in [([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u), ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v)] {
                for i in 0..8 {
                    for j in 0..8 {
                        ata[i][j] += row[i] * row[j];
                    }
                    ata[i][8] += row[i] * rhs;
                }
            }
        }
        let h = solve(ata)?;
        let hn = [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0];
        let m = mul3(&inv3(&td)?, &mul3(&hn, &ts));
        Some(Homography { m })
    }

    /// ZXing `PerspectiveTransform.quadrilateralToQuadrilateral`.
    pub fn quad_to_quad(src: [(f64, f64); 4], dst: [(f64, f64); 4]) -> Option<Homography> {
        Homography::from_points(&src, &dst)
    }

    pub fn map(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.m;
        let d = m[6] * x + m[7] * y + m[8];
        ((m[0] * x + m[1] * y + m[2]) / d, (m[3] * x + m[4] * y + m[5]) / d)
    }

    pub fn inverse(&self) -> Option<Homography> {
        inv3(&self.m).map(|m| Homography { m })
    }
}

/// Sample a binarised image at a point (rounded down like ZXing's grid sampler, clamped).
pub fn bit_at(b: &Bin, p: (f64, f64)) -> bool {
    let x = (p.0.max(0.0) as usize).min(b.width - 1);
    let y = (p.1.max(0.0) as usize).min(b.height - 1);
    b.get(x, y)
}

// ---------------------------------------------------------------------------------------
// Scan-line profiles for linear symbologies

/// A horizontal profile averaged over `2 * half + 1` rows around `y`.
pub fn row_profile(g: &Gray, y: usize, half: usize) -> Vec<f64> {
    let (w, h) = (g.width, g.height);
    let (y0, y1) = (y.saturating_sub(half), (y + half).min(h - 1));
    let n = (y1 - y0 + 1) as f64;
    (0..w).map(|x| (y0..=y1).map(|yy| g.pixels[yy * w + x] as f64).sum::<f64>() / n).collect()
}

/// Threshold a profile locally (midpoint of the min/max in a sliding window); flat parts
/// (contrast below a quarter of the whole profile's) are light.
pub fn threshold_profile(p: &[f64]) -> Vec<bool> {
    let n = p.len();
    if n == 0 {
        return Vec::new();
    }
    let (gmin, gmax) = p.iter().fold((f64::MAX, f64::MIN), |a, v| (a.0.min(*v), a.1.max(*v)));
    let floor = (gmax - gmin) / 4.0;
    let win = (n / 12).max(12);
    let mut out = vec![false; n];
    // sliding window extremes (naive with step reuse: windows are small)
    for (i, o) in out.iter_mut().enumerate() {
        let (a, b) = (i.saturating_sub(win), (i + win).min(n - 1));
        let (mn, mx) = p[a..=b].iter().fold((f64::MAX, f64::MIN), |a, v| (a.0.min(*v), a.1.max(*v)));
        if mx - mn < floor {
            *o = p[i] < gmin + (gmax - gmin) * 0.4;
        } else {
            *o = p[i] < (mn + mx) / 2.0;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn homography_maps_points() {
        let src = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let dst = [(5.0, 7.0), (40.0, 3.0), (45.0, 50.0), (2.0, 41.0)];
        let h = Homography::quad_to_quad(src, dst).unwrap();
        for (s, d) in src.iter().zip(dst) {
            let p = h.map(s.0, s.1);
            assert!((p.0 - d.0).abs() < 1e-6 && (p.1 - d.1).abs() < 1e-6, "{:?} {:?}", p, d);
        }
        let inv = h.inverse().unwrap();
        let q = inv.map(45.0, 50.0);
        assert!((q.0 - 10.0).abs() < 1e-6 && (q.1 - 10.0).abs() < 1e-6);
    }

    #[test]
    fn angle_and_rotation() {
        // vertical stripes rotated by 17 degrees
        let mut g = Gray { width: 120, height: 120, pixels: vec![255; 14400] };
        for y in 0..120 {
            for x in 0..120 {
                if (x / 6) % 2 == 0 {
                    g.pixels[y * 120 + x] = 0;
                }
            }
        }
        let r = rotate(&g, 17.0);
        let a = dominant_angle(&r).unwrap();
        assert!((a - 17.0).abs() < 1.5, "{}", a);
        let a = dominant_angle(&rotate(&g, -30.0)).unwrap();
        assert!((a + 30.0).abs() < 1.5, "{}", a);
        assert!(dominant_angle(&g).unwrap().abs() < 1.0);
    }

    #[test]
    fn noise_estimate() {
        let mut g = Gray { width: 100, height: 100, pixels: vec![128; 10000] };
        assert!(noise_sigma(&g) < 0.5);
        let mut s = 12345u64;
        for p in g.pixels.iter_mut() {
            let mut acc = 0.0;
            for _ in 0..12 {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                acc += (s >> 11) as f64 / (1u64 << 53) as f64;
            }
            *p = (128.0 + (acc - 6.0) * 10.0).round() as u8;
        }
        let n = noise_sigma(&g);
        assert!((n - 10.0).abs() < 2.5, "{}", n);
    }
}
