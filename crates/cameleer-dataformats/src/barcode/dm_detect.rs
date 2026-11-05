//! Data Matrix detection for camera-like images: ZXing's `WhiteRectangleDetector` (grow a
//! box from a seed until all four sides are white) gives the symbol region, the extreme dark
//! points along the diagonals give rough corners, each side is then fitted as a straight line
//! through the outermost dark edge points (the solid L sides and the dark modules of the
//! timing sides lie on it), and the corners are the line intersections. The symbol size and
//! orientation are chosen by scoring the L finder and timing pattern under the perspective
//! transform for every valid size and all eight orientations, then the grid is sampled.

use super::datamatrix;
use super::detect::Bin;
use super::vision::{bit_at, Homography};
use super::Matrix;

type P = (f64, f64);

/// ZXing `WhiteRectangleDetector.detect` box: (left, top, right, bottom), exclusive white
/// borders.
pub fn white_rectangle(b: &Bin, cx: isize, cy: isize, half: isize) -> Option<(isize, isize, isize, isize)> {
    let (w, h) = (b.width as isize, b.height as isize);
    let (mut left, mut right, mut up, mut down) = (cx - half, cx + half, cy - half, cy + half);
    if left < 0 || up < 0 || right >= w || down >= h {
        return None;
    }
    let col_black = |x: isize, y0: isize, y1: isize| (y0.max(0)..=y1.min(h - 1)).any(|y| b.get(x as usize, y as usize));
    let row_black = |y: isize, x0: isize, x1: isize| (x0.max(0)..=x1.min(w - 1)).any(|x| b.get(x as usize, y as usize));
    let (mut fr, mut fb, mut fl, mut ft) = (false, false, false, false);
    let mut found_any = true;
    while found_any {
        found_any = false;
        let mut nw = true;
        while (nw || !fr) && right < w {
            nw = col_black(right, up, down);
            if nw {
                right += 1;
                found_any = true;
                fr = true;
            } else if !fr {
                right += 1;
            }
        }
        if right >= w {
            return None;
        }
        let mut nw = true;
        while (nw || !fb) && down < h {
            nw = row_black(down, left, right);
            if nw {
                down += 1;
                found_any = true;
                fb = true;
            } else if !fb {
                down += 1;
            }
        }
        if down >= h {
            return None;
        }
        let mut nw = true;
        while (nw || !fl) && left >= 0 {
            nw = col_black(left, up, down);
            if nw {
                left -= 1;
                found_any = true;
                fl = true;
            } else if !fl {
                left -= 1;
            }
        }
        if left < 0 {
            return None;
        }
        let mut nw = true;
        while (nw || !ft) && up >= 0 {
            nw = row_black(up, left, right);
            if nw {
                up -= 1;
                found_any = true;
                ft = true;
            } else if !ft {
                up -= 1;
            }
        }
        if up < 0 {
            return None;
        }
    }
    Some((left, up, right, down))
}

fn dark_with_support(b: &Bin, x: usize, y: usize) -> bool {
    if !b.get(x, y) {
        return false;
    }
    let mut n = 0;
    for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1), (-1, -1), (1, 1), (-1, 1), (1, -1)] {
        let (nx, ny) = (x as i64 + dx, y as i64 + dy);
        if nx >= 0 && ny >= 0 && (nx as usize) < b.width && (ny as usize) < b.height && b.get(nx as usize, ny as usize) {
            n += 1;
        }
    }
    n >= 3
}

/// Rough corners (TL, TR, BR, BL) as the extreme supported dark pixels along the diagonals.
fn rough_corners(b: &Bin, r: (isize, isize, isize, isize)) -> Option<[P; 4]> {
    let dirs = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
    let mut best = [(f64::MIN, (0.0, 0.0)); 4];
    for y in (r.1 + 1)..r.3 {
        for x in (r.0 + 1)..r.2 {
            let (xu, yu) = (x as usize, y as usize);
            if !b.get(xu, yu) {
                continue;
            }
            for (k, d) in dirs.iter().enumerate() {
                let s = d.0 * x as f64 + d.1 * y as f64;
                if s > best[k].0 && dark_with_support(b, xu, yu) {
                    best[k] = (s, (x as f64 + 0.5 + 0.5 * d.0, y as f64 + 0.5 + 0.5 * d.1));
                }
            }
        }
    }
    if best.iter().any(|b| b.0 == f64::MIN) {
        return None;
    }
    Some([best[0].1, best[1].1, best[2].1, best[3].1])
}

/// A line as (point, unit direction).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Line {
    p: P,
    d: P,
}

fn fit_line(pts: &[P]) -> Option<Line> {
    if pts.len() < 2 {
        return None;
    }
    let n = pts.len() as f64;
    let (mx, my) = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 / n, a.1 + p.1 / n));
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    for p in pts {
        let (dx, dy) = (p.0 - mx, p.1 - my);
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    Some(Line { p: (mx, my), d: (theta.cos(), theta.sin()) })
}

pub(crate) fn intersect(a: &Line, b: &Line) -> Option<P> {
    let den = a.d.0 * b.d.1 - a.d.1 * b.d.0;
    if den.abs() < 1e-9 {
        return None;
    }
    let t = ((b.p.0 - a.p.0) * b.d.1 - (b.p.1 - a.p.1) * b.d.0) / den;
    Some((a.p.0 + t * a.d.0, a.p.1 + t * a.d.1))
}

/// Fit one side between rough corners `a` and `b` (outward normal away from `centre`).
pub(crate) fn fit_side(bin: &Bin, a: P, b: P, centre: P) -> Option<Line> {
    let (ex, ey) = (b.0 - a.0, b.1 - a.1);
    let len = (ex * ex + ey * ey).sqrt();
    if len < 6.0 {
        return None;
    }
    let mut n = (-ey / len, ex / len);
    let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    if (mid.0 - centre.0) * n.0 + (mid.1 - centre.1) * n.1 < 0.0 {
        n = (-n.0, -n.1);
    }
    let out = 0.08 * len + 3.0;
    let depth_max = out + 0.3 * len;
    let samples = (len as usize).clamp(16, 80);
    let mut pts: Vec<P> = Vec::new();
    for i in 0..samples {
        let t = 0.06 + 0.88 * i as f64 / (samples - 1) as f64;
        let p = (a.0 + t * ex, a.1 + t * ey);
        let mut d = -out;
        while d < depth_max {
            let q = (p.0 - n.0 * d, p.1 - n.1 * d);
            if q.0 >= 0.0 && q.1 >= 0.0 && (q.0 as usize) < bin.width && (q.1 as usize) < bin.height && bin.get(q.0 as usize, q.1 as usize) {
                pts.push((q.0 + 0.5 * n.0, q.1 + 0.5 * n.1));
                break;
            }
            d += 0.5;
        }
    }
    if pts.len() < 6 {
        return None;
    }
    let mut keep = pts.clone();
    let mut line = fit_line(&keep)?;
    for _ in 0..4 {
        // outward depth of every point relative to the current line
        let depth: Vec<f64> = pts.iter().map(|p| (p.0 - line.p.0) * n.0 + (p.1 - line.p.1) * n.1).collect();
        let mut sorted = depth.clone();
        sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
        let q = sorted[sorted.len() * 3 / 10];
        keep = pts.iter().zip(&depth).filter(|(_, d)| **d >= q - 1.25).map(|(p, _)| *p).collect();
        if keep.len() < 4 {
            break;
        }
        line = fit_line(&keep)?;
    }
    Some(line)
}

/// The refined symbol quadrilateral (TL, TR, BR, BL in image orientation).
fn quad(b: &Bin, rect: (isize, isize, isize, isize)) -> Option<[P; 4]> {
    let rc = rough_corners(b, rect)?;
    let centre = (rc.iter().map(|p| p.0).sum::<f64>() / 4.0, rc.iter().map(|p| p.1).sum::<f64>() / 4.0);
    let mut lines = Vec::with_capacity(4);
    for i in 0..4 {
        lines.push(fit_side(b, rc[i], rc[(i + 1) % 4], centre)?);
    }
    let mut q = [(0.0, 0.0); 4];
    for i in 0..4 {
        q[i] = intersect(&lines[(i + 3) % 4], &lines[i])?;
        // reject wild intersections
        if (q[i].0 - rc[i].0).abs() + (q[i].1 - rc[i].1).abs() > 0.25 * ((rc[0].0 - rc[2].0).abs() + (rc[0].1 - rc[2].1).abs()) + 4.0 {
            return None;
        }
    }
    Some(q)
}

fn expected_border(x: usize, y: usize, rows: usize, cols: usize) -> Option<bool> {
    if x == 0 || y == rows - 1 {
        Some(true)
    } else if y == 0 {
        Some(x % 2 == 0)
    } else if x == cols - 1 {
        Some(y % 2 == 1)
    } else {
        None
    }
}

fn border_score(b: &Bin, t: &Homography, rows: usize, cols: usize) -> f64 {
    let (mut ok, mut n) = (0usize, 0usize);
    let mut check = |x: usize, y: usize| {
        if let Some(e) = expected_border(x, y, rows, cols) {
            n += 1;
            if bit_at(b, t.map(x as f64 + 0.5, y as f64 + 0.5)) == e {
                ok += 1;
            }
        }
    };
    for x in 0..cols {
        check(x, 0);
        check(x, rows - 1);
    }
    for y in 1..rows - 1 {
        check(0, y);
        check(cols - 1, y);
    }
    ok as f64 / n as f64
}

fn sample(b: &Bin, t: &Homography, rows: usize, cols: usize) -> Matrix {
    let mut m = Matrix::new(cols, rows);
    for y in 0..rows {
        for x in 0..cols {
            m.set(x, y, bit_at(b, t.map(x as f64 + 0.5, y as f64 + 0.5)));
        }
    }
    m
}

/// Candidate module matrices for the Data Matrix symbol around the seed points, best first.
pub fn detect(b: &Bin) -> Vec<Matrix> {
    let (w, h) = (b.width as isize, b.height as isize);
    let mut seeds = vec![(w / 2, h / 2)];
    let (mut sx, mut sy, mut n) = (0f64, 0f64, 0f64);
    for y in (0..b.height).step_by(2) {
        for x in (0..b.width).step_by(2) {
            if b.get(x, y) {
                sx += x as f64;
                sy += y as f64;
                n += 1.0;
            }
        }
    }
    if n > 0.0 {
        seeds.push(((sx / n) as isize, (sy / n) as isize));
    }
    let dims = datamatrix::symbol_dims();
    let mut out = Vec::new();
    let mut quads: Vec<[P; 4]> = Vec::new();
    for (cx, cy) in seeds {
        let half = (w.min(h) / 20).max(5);
        let Some(rect) = white_rectangle(b, cx, cy, half) else { continue };
        let Some(q) = quad(b, rect) else { continue };
        if quads.iter().any(|o| (0..4).all(|i| (o[i].0 - q[i].0).abs() < 2.0 && (o[i].1 - q[i].1).abs() < 2.0)) {
            continue;
        }
        quads.push(q);
        let side = |i: usize, j: usize| ((q[i].0 - q[j].0).powi(2) + (q[i].1 - q[j].1).powi(2)).sqrt();
        let (wa, ha) = ((side(0, 1) + side(3, 2)) / 2.0, (side(0, 3) + side(1, 2)) / 2.0);
        let mut cands: Vec<(f64, Homography, usize, usize)> = Vec::new();
        for &(rows, cols) in &dims {
            for rot in 0..4 {
                for mirror in [false, true] {
                    let idx: Vec<usize> = (0..4).map(|j| if mirror { (rot + 4 - j) % 4 } else { (rot + j) % 4 }).collect();
                    // canonical width runs along image side idx[0] -> idx[1]
                    let horizontal = matches!((idx[0].min(idx[1]), idx[0].max(idx[1])), (0, 1) | (2, 3));
                    let (img_w, img_h) = if horizontal { (wa, ha) } else { (ha, wa) };
                    let module_w = img_w / cols as f64;
                    let module_h = img_h / rows as f64;
                    if module_w < 1.2 || module_h < 1.2 || (module_w / module_h).ln().abs() > 0.6 {
                        continue;
                    }
                    let canon = [(0.0, 0.0), (cols as f64, 0.0), (cols as f64, rows as f64), (0.0, rows as f64)];
                    let dst = [q[idx[0]], q[idx[1]], q[idx[2]], q[idx[3]]];
                    let Some(t) = Homography::quad_to_quad(canon, dst) else { continue };
                    let s = border_score(b, &t, rows, cols);
                    if s >= 0.8 {
                        cands.push((s, t, rows, cols));
                    }
                }
            }
        }
        cands.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        for (_, t, rows, cols) in cands.into_iter().take(6) {
            out.push(sample(b, &t, rows, cols));
        }
    }
    out
}
