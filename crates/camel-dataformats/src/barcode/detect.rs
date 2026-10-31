//! Locating symbols in (clean, synthetic) images: binarisation, QR finder pattern detection
//! (1:1:3:1:1 runs confirmed horizontally and vertically, clustering, right-isosceles triple
//! selection, orientation) with affine module sampling, Data Matrix location by its L finder
//! and timing pattern, and scan rows for linear symbologies.

use super::png::Gray;
use super::{qr, Matrix};

/// A binarised image (`true` = dark).
pub struct Bin {
    pub width: usize,
    pub height: usize,
    pub bits: Vec<bool>,
}

impl Bin {
    pub fn get(&self, x: usize, y: usize) -> bool {
        self.bits[y * self.width + x]
    }
    pub fn row(&self, y: usize) -> &[bool] {
        &self.bits[y * self.width..(y + 1) * self.width]
    }
}

/// Global threshold half way between the darkest and lightest pixel.
pub fn binarize(g: &Gray) -> Option<Bin> {
    let min = *g.pixels.iter().min()?;
    let max = *g.pixels.iter().max()?;
    if max - min < 32 {
        return None;
    }
    let thr = ((min as u16 + max as u16) / 2) as u8;
    Some(Bin { width: g.width, height: g.height, bits: g.pixels.iter().map(|p| *p <= thr).collect() })
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
    x: f64,
    y: f64,
    module: f64,
    count: usize,
}

fn ratio_ok(c: &[usize; 5]) -> Option<f64> {
    let total: usize = c.iter().sum();
    if total < 7 {
        return None;
    }
    let m = total as f64 / 7.0;
    let v = m / 2.0;
    let ok = (c[0] as f64 - m).abs() < v && (c[1] as f64 - m).abs() < v && (c[2] as f64 - 3.0 * m).abs() < 3.0 * v && (c[3] as f64 - m).abs() < v && (c[4] as f64 - m).abs() < v;
    ok.then_some(m)
}

/// Vertical check through (x, y): returns (centre y, module).
fn check_vertical(b: &Bin, x: usize, y: usize) -> Option<(f64, f64)> {
    if !b.get(x, y) {
        return None;
    }
    let mut up = y;
    while up > 0 && b.get(x, up - 1) {
        up -= 1;
    }
    let mut down = y;
    while down + 1 < b.height && b.get(x, down + 1) {
        down += 1;
    }
    // runs above: light, dark ; below: light, dark
    let run = |mut p: i64, dir: i64, dark: bool| -> (usize, i64) {
        let mut n = 0;
        while p >= 0 && (p as usize) < b.height && b.get(x, p as usize) == dark {
            n += 1;
            p += dir;
        }
        (n, p)
    };
    let (l1, p) = run(up as i64 - 1, -1, false);
    let (d1, _) = run(p, -1, true);
    let (l2, p) = run(down as i64 + 1, 1, false);
    let (d2, _) = run(p, 1, true);
    let c = [d1, l1, down - up + 1, l2, d2];
    let m = ratio_ok(&c)?;
    Some(((up + down + 1) as f64 / 2.0, m))
}

fn finder_candidates(b: &Bin) -> Vec<Candidate> {
    let mut cands: Vec<Candidate> = Vec::new();
    for y in 0..b.height {
        let row = b.row(y);
        // runs as (start, len, dark)
        let mut runs: Vec<(usize, usize, bool)> = Vec::new();
        let mut start = 0;
        for x in 1..=row.len() {
            if x == row.len() || row[x] != row[start] {
                runs.push((start, x - start, row[start]));
                start = x;
            }
        }
        for w in runs.windows(5) {
            if !w[0].2 {
                continue;
            }
            let c = [w[0].1, w[1].1, w[2].1, w[3].1, w[4].1];
            let Some(mh) = ratio_ok(&c) else { continue };
            let cx = w[2].0 as f64 + w[2].1 as f64 / 2.0;
            let Some((cy, mv)) = check_vertical(b, cx as usize, y) else { continue };
            let module = (mh + mv) / 2.0;
            if let Some(c) = cands.iter_mut().find(|c| (c.x - cx).abs() <= module * 1.5 && (c.y - cy).abs() <= module * 1.5) {
                let n = c.count as f64;
                c.x = (c.x * n + cx) / (n + 1.0);
                c.y = (c.y * n + cy) / (n + 1.0);
                c.module = (c.module * n + module) / (n + 1.0);
                c.count += 1;
            } else {
                cands.push(Candidate { x: cx, y: cy, module, count: 1 });
            }
        }
    }
    cands.sort_by(|a, b| b.count.cmp(&a.count));
    cands
}

/// Pick (top-left, top-right, bottom-left) among candidates forming a right isosceles triangle.
fn select_triple(c: &[Candidate]) -> Option<(Candidate, Candidate, Candidate)> {
    let n = c.len().min(8);
    let d2 = |a: &Candidate, b: &Candidate| (a.x - b.x).powi(2) + (a.y - b.y).powi(2);
    let mut best: Option<(usize, (Candidate, Candidate, Candidate))> = None;
    for i in 0..n {
        for j in i + 1..n {
            for k in j + 1..n {
                let t = [c[i], c[j], c[k]];
                let ms = [t[0].module, t[1].module, t[2].module];
                let (mmin, mmax) = (ms.iter().cloned().fold(f64::MAX, f64::min), ms.iter().cloned().fold(0.0, f64::max));
                if mmax > mmin * 1.4 {
                    continue;
                }
                for corner in 0..3 {
                    let (a, p, q) = (t[corner], t[(corner + 1) % 3], t[(corner + 2) % 3]);
                    let (s1, s2, hyp) = (d2(&a, &p), d2(&a, &q), d2(&p, &q));
                    if s1 < (7.0 * mmin).powi(2) || (s1 - s2).abs() > 0.2 * s1.max(s2) || (hyp - s1 - s2).abs() > 0.15 * hyp {
                        continue;
                    }
                    let cross = (p.x - a.x) * (q.y - a.y) - (p.y - a.y) * (q.x - a.x);
                    let (tr, bl) = if cross > 0.0 { (p, q) } else { (q, p) };
                    let score = a.count + p.count + q.count;
                    if best.as_ref().is_none_or(|b| score > b.0) {
                        best = Some((score, (a, tr, bl)));
                    }
                }
            }
        }
    }
    best.map(|b| b.1)
}

fn sample(b: &Bin, tl: (f64, f64), u: (f64, f64), v: (f64, f64), dim: usize) -> Option<Matrix> {
    let mut m = Matrix::new(dim, dim);
    for r in 0..dim {
        for c in 0..dim {
            let x = tl.0 + (c as f64 - 3.0) * u.0 + (r as f64 - 3.0) * v.0;
            let y = tl.1 + (c as f64 - 3.0) * u.1 + (r as f64 - 3.0) * v.1;
            if x < 0.0 || y < 0.0 || x >= b.width as f64 || y >= b.height as f64 {
                return None;
            }
            m.set(c, r, b.get(x as usize, y as usize));
        }
    }
    Some(m)
}

/// Locate and decode a QR Code.
pub fn find_qr(b: &Bin) -> Option<qr::Decoded> {
    let cands = finder_candidates(b);
    let (tl, tr, bl) = select_triple(&cands)?;
    let module = (tl.module + tr.module + bl.module) / 3.0;
    let dist = |a: &Candidate, c: &Candidate| ((a.x - c.x).powi(2) + (a.y - c.y).powi(2)).sqrt();
    let est = ((dist(&tl, &tr) + dist(&tl, &bl)) / 2.0 / module).round() as i64 + 7;
    let base = match est.rem_euclid(4) {
        0 => est + 1,
        2 => est - 1,
        3 => est - 2,
        _ => est,
    };
    let mut dims: Vec<i64> = vec![base, base + 4, base - 4];
    let mut tried = Vec::new();
    while let Some(dim) = dims.first().copied() {
        dims.remove(0);
        if !(21..=177).contains(&dim) || tried.contains(&dim) {
            continue;
        }
        tried.push(dim);
        let n = (dim - 7) as f64;
        let u = ((tr.x - tl.x) / n, (tr.y - tl.y) / n);
        let v = ((bl.x - tl.x) / n, (bl.y - tl.y) / n);
        let Some(m) = sample(b, (tl.x, tl.y), u, v, dim as usize) else { continue };
        if dim >= 45 {
            // version information decides the dimension for v >= 7
            if let Some(ver) = qr::read_version(&m) {
                let vd = (ver * 4 + 17) as i64;
                if vd != dim {
                    dims.insert(0, vd);
                    continue;
                }
            }
        }
        if let Ok(d) = qr::decode_matrix(&m) {
            return Some(d);
        }
        // a mirrored symbol samples as the transpose
        let mut t = Matrix::new(m.width, m.height);
        for y in 0..m.height {
            for x in 0..m.width {
                t.set(y, x, m.get(x, y));
            }
        }
        if let Ok(d) = qr::decode_matrix(&t) {
            return Some(d);
        }
    }
    None
}

/// Locate an upright Data Matrix symbol and sample its modules.
pub fn find_datamatrix(b: &Bin, sizes: &[usize]) -> Option<Matrix> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..b.height {
        for x in 0..b.width {
            if b.get(x, y) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x0 == usize::MAX {
        return None;
    }
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    // the left column must be solid (L finder)
    if (y0..=y1).any(|y| !b.get(x0, y)) || (x0..=x1).any(|x| !b.get(x, y1)) {
        return None;
    }
    // top-left timing module width
    let mut module = 0;
    while x0 + module <= x1 && b.get(x0 + module, y0) {
        module += 1;
    }
    if module == 0 {
        return None;
    }
    let dim = (w as f64 / module as f64).round() as usize;
    if !sizes.contains(&dim) || ((h as f64 / module as f64).round() as usize) != dim {
        return None;
    }
    let (sx, sy) = (w as f64 / dim as f64, h as f64 / dim as f64);
    let mut m = Matrix::new(dim, dim);
    for r in 0..dim {
        for c in 0..dim {
            let x = x0 + ((c as f64 + 0.5) * sx) as usize;
            let y = y0 + ((r as f64 + 0.5) * sy) as usize;
            m.set(c, r, b.get(x.min(x1), y.min(y1)));
        }
    }
    Some(m)
}

/// Rows to scan for linear symbologies (centre first), each also reversed.
pub fn scan_rows(b: &Bin) -> Vec<Vec<bool>> {
    let mut out = Vec::new();
    for f in [0.5, 0.4, 0.6, 0.25, 0.75] {
        let y = ((b.height as f64 * f) as usize).min(b.height - 1);
        let row = b.row(y).to_vec();
        let mut rev = row.clone();
        rev.reverse();
        out.push(row);
        out.push(rev);
    }
    out
}
