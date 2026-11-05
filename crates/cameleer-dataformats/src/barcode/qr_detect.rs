//! QR Code detection after ZXing's `FinderPatternFinder`, `AlignmentPatternFinder` and
//! `Detector`: 1:1:3:1:1 finder patterns cross-checked vertically, horizontally and
//! diagonally (any rotation, scale and moderate skew), best-triangle selection, module size
//! from black-white-black runs, dimension estimate, alignment-pattern search for versions
//! >= 2 and a perspective transform (finder centres + alignment pattern) for grid sampling.

use super::detect::Bin;
use super::vision::{bit_at, Homography};
use super::{qr, Matrix};

#[derive(Clone, Copy, Debug)]
pub struct Pattern {
    pub x: f64,
    pub y: f64,
    pub size: f64,
    pub count: usize,
}

impl Pattern {
    fn about_equals(&self, size: f64, i: f64, j: f64) -> bool {
        if (i - self.y).abs() <= size && (j - self.x).abs() <= size {
            let d = (size - self.size).abs();
            return d <= 1.0 || d <= self.size;
        }
        false
    }
    fn combine(&self, i: f64, j: f64, size: f64) -> Pattern {
        let n = self.count as f64;
        Pattern { x: (n * self.x + j) / (n + 1.0), y: (n * self.y + i) / (n + 1.0), size: (n * self.size + size) / (n + 1.0), count: self.count + 1 }
    }
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn found_pattern_cross(sc: &[usize; 5]) -> bool {
    let total: usize = sc.iter().sum();
    if total < 7 {
        return false;
    }
    let m = total as f64 / 7.0;
    let v = m / 2.0;
    (m - sc[0] as f64).abs() < v && (m - sc[1] as f64).abs() < v && (3.0 * m - sc[2] as f64).abs() < 3.0 * v && (m - sc[3] as f64).abs() < v && (m - sc[4] as f64).abs() < v
}

fn found_pattern_diagonal(sc: &[usize; 5]) -> bool {
    let total: usize = sc.iter().sum();
    if total < 7 {
        return false;
    }
    let m = total as f64 / 7.0;
    let v = m / 1.333;
    (m - sc[0] as f64).abs() < v && (m - sc[1] as f64).abs() < v && (3.0 * m - sc[2] as f64).abs() < 3.0 * v && (m - sc[3] as f64).abs() < v && (m - sc[4] as f64).abs() < v
}

fn center_from_end(sc: &[usize; 5], end: usize) -> f64 {
    end as f64 - sc[4] as f64 - sc[3] as f64 - sc[2] as f64 / 2.0
}

struct FinderSearch<'a> {
    b: &'a Bin,
    centers: Vec<Pattern>,
}

impl FinderSearch<'_> {
    /// Count runs along a line through (cx, cy) with step (dx, dy): returns the state counts
    /// and the end coordinate (along the main axis) of the last run.
    fn cross_check(&self, ci: usize, cj: usize, max_count: usize, total0: usize, vertical: bool) -> Option<f64> {
        let b = self.b;
        let lim = if vertical { b.height } else { b.width } as isize;
        let get = |k: isize| if vertical { b.get(cj, k as usize) } else { b.get(k as usize, ci) };
        let start = if vertical { ci } else { cj } as isize;
        let mut sc = [0usize; 5];
        let mut i = start;
        while i >= 0 && get(i) {
            sc[2] += 1;
            i -= 1;
        }
        if i < 0 {
            return None;
        }
        while i >= 0 && !get(i) && sc[1] <= max_count {
            sc[1] += 1;
            i -= 1;
        }
        if i < 0 || sc[1] > max_count {
            return None;
        }
        while i >= 0 && get(i) && sc[0] <= max_count {
            sc[0] += 1;
            i -= 1;
        }
        if sc[0] > max_count {
            return None;
        }
        let mut i = start + 1;
        while i < lim && get(i) {
            sc[2] += 1;
            i += 1;
        }
        if i == lim {
            return None;
        }
        while i < lim && !get(i) && sc[3] < max_count {
            sc[3] += 1;
            i += 1;
        }
        if i == lim || sc[3] >= max_count {
            return None;
        }
        while i < lim && get(i) && sc[4] < max_count {
            sc[4] += 1;
            i += 1;
        }
        if sc[4] >= max_count {
            return None;
        }
        let total: usize = sc.iter().sum();
        if 5 * total.abs_diff(total0) >= 2 * total0 {
            return None;
        }
        found_pattern_cross(&sc).then(|| center_from_end(&sc, i as usize))
    }

    fn cross_check_diagonal(&self, ci: usize, cj: usize) -> bool {
        let b = self.b;
        let mut sc = [0usize; 5];
        let mut i = 0;
        while ci >= i && cj >= i && b.get(cj - i, ci - i) {
            sc[2] += 1;
            i += 1;
        }
        if sc[2] == 0 {
            return false;
        }
        while ci >= i && cj >= i && !b.get(cj - i, ci - i) {
            sc[1] += 1;
            i += 1;
        }
        if sc[1] == 0 {
            return false;
        }
        while ci >= i && cj >= i && b.get(cj - i, ci - i) {
            sc[0] += 1;
            i += 1;
        }
        if sc[0] == 0 {
            return false;
        }
        let (mi, mj) = (b.height, b.width);
        let mut i = 1;
        while ci + i < mi && cj + i < mj && b.get(cj + i, ci + i) {
            sc[2] += 1;
            i += 1;
        }
        while ci + i < mi && cj + i < mj && !b.get(cj + i, ci + i) {
            sc[3] += 1;
            i += 1;
        }
        if sc[3] == 0 {
            return false;
        }
        while ci + i < mi && cj + i < mj && b.get(cj + i, ci + i) {
            sc[4] += 1;
            i += 1;
        }
        if sc[4] == 0 {
            return false;
        }
        found_pattern_diagonal(&sc)
    }

    fn handle_possible_center(&mut self, sc: &[usize; 5], i: usize, j: usize) -> bool {
        let total: usize = sc.iter().sum();
        let cj = center_from_end(sc, j);
        if cj < 0.0 {
            return false;
        }
        let Some(ci) = self.cross_check(i, cj as usize, sc[2], total, true) else { return false };
        let Some(cj) = self.cross_check(ci as usize, cj as usize, sc[2], total, false) else { return false };
        if !self.cross_check_diagonal(ci as usize, cj as usize) {
            return false;
        }
        let est = total as f64 / 7.0;
        if let Some(c) = self.centers.iter_mut().find(|c| c.about_equals(est, ci, cj)) {
            *c = c.combine(ci, cj, est);
        } else {
            self.centers.push(Pattern { x: cj, y: ci, size: est, count: 1 });
        }
        true
    }

    fn scan(&mut self) {
        let b = self.b;
        let (h, w) = (b.height, b.width);
        for i in 0..h {
            let mut sc = [0usize; 5];
            let mut state = 0usize;
            let mut j = 0;
            while j < w {
                if b.get(j, i) {
                    if state & 1 == 1 {
                        state += 1;
                    }
                    sc[state] += 1;
                } else if state & 1 == 0 {
                    if state == 4 {
                        if found_pattern_cross(&sc) && self.handle_possible_center(&sc, i, j) {
                            sc = [0; 5];
                            state = 0;
                        } else {
                            sc = [sc[2], sc[3], sc[4], 1, 0];
                            state = 3;
                        }
                    } else {
                        state += 1;
                        sc[state] += 1;
                    }
                } else {
                    sc[state] += 1;
                }
                j += 1;
            }
            if found_pattern_cross(&sc) {
                self.handle_possible_center(&sc, i, w);
            }
        }
    }
}

/// Order three patterns as (bottom-left, top-left, top-right) like ZXing's
/// `ResultPoint.orderBestPatterns`.
fn order(p: [Pattern; 3]) -> (Pattern, Pattern, Pattern) {
    let d = |a: &Pattern, b: &Pattern| dist((a.x, a.y), (b.x, b.y));
    let (d01, d12, d02) = (d(&p[0], &p[1]), d(&p[1], &p[2]), d(&p[0], &p[2]));
    let (mut a, b, mut c) = if d12 >= d01 && d12 >= d02 {
        (p[1], p[0], p[2])
    } else if d02 >= d12 && d02 >= d01 {
        (p[0], p[1], p[2])
    } else {
        (p[0], p[2], p[1])
    };
    let cross = (c.x - b.x) * (a.y - b.y) - (c.y - b.y) * (a.x - b.x);
    if cross < 0.0 {
        std::mem::swap(&mut a, &mut c);
    }
    (a, b, c)
}

/// Candidate (bottom-left, top-left, top-right) triples, best first (ZXing's
/// `selectBestPatterns` distortion measure).
fn best_triples(centers: &[Pattern]) -> Vec<(Pattern, Pattern, Pattern)> {
    let mut c: Vec<Pattern> = centers.iter().filter(|p| p.count >= 2).copied().collect();
    if c.len() < 3 {
        c = centers.to_vec();
    }
    c.sort_by(|a, b| b.count.cmp(&a.count));
    c.truncate(16);
    c.sort_by(|a, b| a.size.partial_cmp(&b.size).unwrap());
    let sq = |a: &Pattern, b: &Pattern| (a.x - b.x).powi(2) + (a.y - b.y).powi(2);
    let mut out: Vec<(f64, [Pattern; 3])> = Vec::new();
    for i in 0..c.len() {
        for j in i + 1..c.len() {
            for k in j + 1..c.len() {
                if c[k].size > c[i].size * 1.4 {
                    continue;
                }
                let mut s = [sq(&c[i], &c[j]), sq(&c[j], &c[k]), sq(&c[i], &c[k])];
                s.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let m = (c[i].size + c[j].size + c[k].size) / 3.0;
                if s[0] < (7.0 * m).powi(2) {
                    continue;
                }
                let d = (s[2] - 2.0 * s[1]).abs() + (s[2] - 2.0 * s[0]).abs();
                // relative distortion
                out.push((d / s[2], [c[i], c[j], c[k]]));
            }
        }
    }
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    out.into_iter().filter(|o| o.0 < 0.5).take(6).map(|o| order(o.1)).collect()
}

/// Find candidate finder pattern triples.
pub fn find_finders(b: &Bin) -> Vec<(Pattern, Pattern, Pattern)> {
    let mut s = FinderSearch { b, centers: Vec::new() };
    s.scan();
    best_triples(&s.centers)
}

// ---------------------------------------------------------------------------------------
// Detector

fn size_of_bwb_run(b: &Bin, from: (i64, i64), to: (i64, i64)) -> f64 {
    let (mut fx, mut fy, mut tx, mut ty) = (from.0, from.1, to.0, to.1);
    let steep = (ty - fy).abs() > (tx - fx).abs();
    if steep {
        std::mem::swap(&mut fx, &mut fy);
        std::mem::swap(&mut tx, &mut ty);
    }
    let dx = (tx - fx).abs();
    let dy = (ty - fy).abs();
    let mut error = -dx / 2;
    let xstep = if fx < tx { 1 } else { -1 };
    let ystep = if fy < ty { 1 } else { -1 };
    let mut state = 0;
    let x_limit = tx + xstep;
    let (mut x, mut y) = (fx, fy);
    while x != x_limit {
        let (rx, ry) = if steep { (y, x) } else { (x, y) };
        if rx < 0 || ry < 0 || rx as usize >= b.width || ry as usize >= b.height {
            break;
        }
        if (state == 1) == b.get(rx as usize, ry as usize) {
            if state == 2 {
                return dist((x as f64, y as f64), (fx as f64, fy as f64));
            }
            state += 1;
        }
        error += dy;
        if error > 0 {
            if y == ty {
                break;
            }
            y += ystep;
            error -= dx;
        }
        x += xstep;
    }
    if state == 2 {
        return dist(((tx + xstep) as f64, ty as f64), (fx as f64, fy as f64));
    }
    f64::NAN
}

fn size_of_bwb_both_ways(b: &Bin, from: (i64, i64), to: (i64, i64)) -> f64 {
    let (w, h) = (b.width as i64, b.height as i64);
    let mut result = size_of_bwb_run(b, from, to);
    let (fx, fy) = from;
    let mut scale = 1.0;
    let mut otx = fx - (to.0 - fx);
    if otx < 0 {
        scale = fx as f64 / (fx - otx) as f64;
        otx = 0;
    } else if otx >= w {
        scale = (w - 1 - fx) as f64 / (otx - fx) as f64;
        otx = w - 1;
    }
    let mut oty = (fy as f64 - (to.1 - fy) as f64 * scale) as i64;
    scale = 1.0;
    if oty < 0 {
        scale = fy as f64 / (fy - oty) as f64;
        oty = 0;
    } else if oty >= h {
        scale = (h - 1 - fy) as f64 / (oty - fy) as f64;
        oty = h - 1;
    }
    otx = (fx as f64 + (otx - fx) as f64 * scale) as i64;
    result += size_of_bwb_run(b, from, (otx, oty));
    result - 1.0
}

fn module_one_way(b: &Bin, p: &Pattern, q: &Pattern) -> f64 {
    let pi = (p.x as i64, p.y as i64);
    let qi = (q.x as i64, q.y as i64);
    let e1 = size_of_bwb_both_ways(b, pi, qi);
    let e2 = size_of_bwb_both_ways(b, qi, pi);
    if e1.is_nan() {
        return e2 / 7.0;
    }
    if e2.is_nan() {
        return e1 / 7.0;
    }
    (e1 + e2) / 14.0
}

fn module_size(b: &Bin, tl: &Pattern, tr: &Pattern, bl: &Pattern) -> f64 {
    let a = module_one_way(b, tl, tr);
    let c = module_one_way(b, tl, bl);
    match (a.is_nan(), c.is_nan()) {
        (false, false) => (a + c) / 2.0,
        (true, false) => c,
        (false, true) => a,
        _ => (tl.size + tr.size + bl.size) / 3.0,
    }
}

/// ZXing `AlignmentPatternFinder` over a region.
fn find_alignment(b: &Bin, module: f64, ex: f64, ey: f64, allowance_factor: f64) -> Option<(f64, f64)> {
    let allowance = (allowance_factor * module) as i64;
    let left = (ex as i64 - allowance).max(0);
    let right = (ex as i64 + allowance).min(b.width as i64 - 1);
    let top = (ey as i64 - allowance).max(0);
    let bottom = (ey as i64 + allowance).min(b.height as i64 - 1);
    if ((right - left) as f64) < module * 3.0 || ((bottom - top) as f64) < module * 3.0 {
        return None;
    }
    let (sx, w, sy, h) = (left as usize, (right - left) as usize, top as usize, (bottom - top) as usize);
    let max_j = sx + w;
    let middle = sy + h / 2;
    let mut possible: Vec<Pattern> = Vec::new();
    let cross_ok = |sc: &[usize; 3]| sc.iter().all(|c| (module - *c as f64).abs() < module / 2.0);
    let check_vertical = |si: usize, cj: usize, max_count: usize, total0: usize| -> Option<f64> {
        let mut sc = [0usize; 3];
        let mut i = si as isize;
        while i >= 0 && b.get(cj, i as usize) && sc[1] <= max_count {
            sc[1] += 1;
            i -= 1;
        }
        if i < 0 || sc[1] > max_count {
            return None;
        }
        while i >= 0 && !b.get(cj, i as usize) && sc[0] <= max_count {
            sc[0] += 1;
            i -= 1;
        }
        if sc[0] > max_count {
            return None;
        }
        let mut i = si + 1;
        while i < b.height && b.get(cj, i) && sc[1] <= max_count {
            sc[1] += 1;
            i += 1;
        }
        if i == b.height || sc[1] > max_count {
            return None;
        }
        while i < b.height && !b.get(cj, i) && sc[2] <= max_count {
            sc[2] += 1;
            i += 1;
        }
        if sc[2] > max_count {
            return None;
        }
        let total: usize = sc.iter().sum();
        if 5 * total.abs_diff(total0) >= 2 * total0 {
            return None;
        }
        cross_ok(&sc).then(|| i as f64 - sc[2] as f64 - sc[1] as f64 / 2.0)
    };
    let handle = |sc: &[usize; 3], i: usize, j: usize, possible: &mut Vec<Pattern>| -> Option<(f64, f64)> {
        let total: usize = sc.iter().sum();
        let cj = j as f64 - sc[2] as f64 - sc[1] as f64 / 2.0;
        let ci = check_vertical(i, cj as usize, 2 * sc[1], total)?;
        let est = total as f64 / 3.0;
        if let Some(p) = possible.iter().find(|p| p.about_equals(est, ci, cj)) {
            let c = p.combine(ci, cj, est);
            return Some((c.x, c.y));
        }
        possible.push(Pattern { x: cj, y: ci, size: est, count: 1 });
        None
    };
    for ig in 0..h {
        let off = (ig + 1) / 2;
        let i = if ig & 1 == 0 { middle + off } else { middle.wrapping_sub(off) };
        if i >= b.height {
            continue;
        }
        let mut sc = [0usize; 3];
        let mut j = sx;
        while j < max_j && !b.get(j, i) {
            j += 1;
        }
        let mut state = 0;
        while j < max_j {
            if b.get(j, i) {
                if state == 1 {
                    sc[1] += 1;
                } else if state == 2 {
                    if cross_ok(&sc) {
                        if let Some(r) = handle(&sc, i, j, &mut possible) {
                            return Some(r);
                        }
                    }
                    sc = [sc[2], 1, 0];
                    state = 1;
                } else {
                    state += 1;
                    sc[state] += 1;
                }
            } else {
                if state == 1 {
                    state += 1;
                }
                sc[state] += 1;
            }
            j += 1;
        }
        if cross_ok(&sc) {
            if let Some(r) = handle(&sc, i, max_j, &mut possible) {
                return Some(r);
            }
        }
    }
    possible.first().map(|p| (p.x, p.y))
}

fn sample_grid(b: &Bin, t: &Homography, dim: usize) -> Matrix {
    let mut m = Matrix::new(dim, dim);
    for y in 0..dim {
        for x in 0..dim {
            m.set(x, y, bit_at(b, t.map(x as f64 + 0.5, y as f64 + 0.5)));
        }
    }
    m
}

fn transpose(m: &Matrix) -> Matrix {
    let mut t = Matrix::new(m.height, m.width);
    for y in 0..m.height {
        for x in 0..m.width {
            t.set(y, x, m.get(x, y));
        }
    }
    t
}

fn try_decode(m: &Matrix) -> Option<qr::Decoded> {
    qr::decode_matrix(m).ok().or_else(|| qr::decode_matrix(&transpose(m)).ok())
}

/// Detect and decode from one finder triple.
fn decode_triple(b: &Bin, bl: &Pattern, tl: &Pattern, tr: &Pattern) -> Option<qr::Decoded> {
    let module = module_size(b, tl, tr, bl);
    if !(module >= 1.0) {
        return None;
    }
    let tltr = (dist((tl.x, tl.y), (tr.x, tr.y)) / module).round() as i64;
    let tlbl = (dist((tl.x, tl.y), (bl.x, bl.y)) / module).round() as i64;
    let mut dim = (tltr + tlbl) / 2 + 7;
    match dim & 3 {
        0 => dim += 1,
        2 => dim -= 1,
        3 => dim -= 2,
        _ => {}
    }
    let mut dims = vec![dim, dim + 4, dim - 4];
    let mut tried = Vec::new();
    while !dims.is_empty() {
        let dim = dims.remove(0);
        if !(21..=177).contains(&dim) || tried.contains(&dim) {
            continue;
        }
        tried.push(dim);
        let d = dim as f64;
        let version = (dim - 17) / 4;
        let mbf = d - 7.0;
        let br_est = (tr.x - tl.x + bl.x, tr.y - tl.y + bl.y);
        let mut aligns = Vec::new();
        if version >= 2 {
            let corr = 1.0 - 3.0 / mbf;
            let ex = tl.x + corr * (br_est.0 - tl.x);
            let ey = tl.y + corr * (br_est.1 - tl.y);
            for f in [4.0, 8.0, 16.0] {
                if let Some(a) = find_alignment(b, module, ex, ey, f) {
                    aligns.push(a);
                    break;
                }
            }
        }
        let mut transforms = Vec::new();
        for a in &aligns {
            transforms.push(Homography::quad_to_quad([(3.5, 3.5), (d - 3.5, 3.5), (d - 6.5, d - 6.5), (3.5, d - 3.5)], [(tl.x, tl.y), (tr.x, tr.y), *a, (bl.x, bl.y)]));
        }
        transforms.push(Homography::quad_to_quad([(3.5, 3.5), (d - 3.5, 3.5), (d - 3.5, d - 3.5), (3.5, d - 3.5)], [(tl.x, tl.y), (tr.x, tr.y), br_est, (bl.x, bl.y)]));
        for t in transforms.into_iter().flatten() {
            let m = sample_grid(b, &t, dim as usize);
            if dim >= 45 {
                if let Some(ver) = qr::read_version(&m).or_else(|| qr::read_version(&transpose(&m))) {
                    let vd = (ver * 4 + 17) as i64;
                    if vd != dim {
                        dims.insert(0, vd);
                        break;
                    }
                }
            }
            if let Some(r) = try_decode(&m) {
                return Some(r);
            }
        }
    }
    None
}

/// Locate and decode a QR Code (ZXing detector pipeline).
pub fn detect_and_decode(b: &Bin) -> Option<qr::Decoded> {
    for (bl, tl, tr) in find_finders(b) {
        if let Some(d) = decode_triple(b, &bl, &tl, &tr) {
            return Some(d);
        }
    }
    None
}
