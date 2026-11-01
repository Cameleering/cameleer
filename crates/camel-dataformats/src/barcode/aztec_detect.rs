//! Aztec detection for camera-like images (after ZXing's Aztec `Detector`): the bull's-eye
//! centre is found by runs of nine roughly equal alternating rings confirmed horizontally
//! and vertically; rays cast in eight directions locate the ring boundaries, a least-squares
//! perspective transform is fitted to those points (and refined by re-casting the rays at the
//! predicted ring corners), and the mode message is read in all eight orientations before the
//! whole symbol is sampled and decoded.

use super::aztec;
use super::detect::Bin;
use super::dm_detect;
use super::vision::{bit_at, Homography};
use super::Matrix;

type P = (f64, f64);

fn runs_of(get: &dyn Fn(usize) -> bool, len: usize) -> Vec<(bool, usize, usize)> {
    let mut out: Vec<(bool, usize, usize)> = Vec::new();
    for i in 0..len {
        let v = get(i);
        match out.last_mut() {
            Some(r) if r.0 == v => r.2 += 1,
            _ => out.push((v, i, 1)),
        }
    }
    out
}

/// Nine runs through the bull's-eye: the inner seven about equal, the outer two (the dark
/// ring 4, possibly merged with dark mode-message modules) at least half a module.
fn equal_runs(r: &[(bool, usize, usize)]) -> Option<f64> {
    let inner = &r[1..8];
    let total: usize = inner.iter().map(|x| x.2).sum();
    let m = total as f64 / inner.len() as f64;
    if m < 1.0 {
        return None;
    }
    (inner.iter().all(|x| (x.2 as f64 - m).abs() < 0.6 * m) && r[0].2 as f64 >= 0.5 * m && r[8].2 as f64 >= 0.5 * m).then_some(m)
}

/// The rotation of the bull's-eye squares: rays in 64 directions, the distances to the
/// ring boundaries 2.5 and 3.5 should follow a square's polar profile.
fn ring_angle(b: &Bin, c: P, module: f64) -> f64 {
    let max = module * 12.0;
    let mut rays: Vec<(f64, f64, f64)> = Vec::new();
    for j in 0..64 {
        let phi = j as f64 * std::f64::consts::PI / 32.0;
        if let Some(ts) = transitions(b, c, (phi.cos(), phi.sin()), 4, max) {
            rays.push((phi, ts[2], ts[3]));
        }
    }
    if rays.len() < 16 {
        return 0.0;
    }
    let mut best = (f64::MAX, 0.0);
    for step in 0..180 {
        let th = (step as f64 * 0.5).to_radians();
        let mut err = 0.0;
        for k in 0..2 {
            let vals: Vec<f64> = rays
                .iter()
                .map(|r| {
                    let (s, c) = (r.0 - th).sin_cos();
                    (if k == 0 { r.1 } else { r.2 }) * s.abs().max(c.abs())
                })
                .collect();
            let mean = vals.iter().sum::<f64>() / vals.len() as f64;
            err += vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (mean * mean);
        }
        if err < best.0 {
            best = (err, th);
        }
    }
    best.1
}

/// Bull's-eye centre candidates (x, y, module), most confirmed first.
fn centres(b: &Bin) -> Vec<(f64, f64, f64)> {
    let mut cands: Vec<(f64, f64, f64, usize)> = Vec::new();
    for y in 0..b.height {
        let runs = runs_of(&|x| b.get(x, y), b.width);
        if runs.len() < 9 {
            continue;
        }
        for i in 0..runs.len() - 8 {
            let win = &runs[i..i + 9];
            if !win[0].0 {
                continue;
            }
            let Some(m) = equal_runs(win) else { continue };
            let cx = win[4].1 as f64 + win[4].2 as f64 / 2.0;
            let x = cx as usize;
            // vertical confirmation
            let col = runs_of(&|yy| b.get(x, yy), b.height);
            let Some(k) = col.iter().position(|r| r.1 <= y && y < r.1 + r.2) else { continue };
            if k < 4 || k + 4 >= col.len() {
                continue;
            }
            let Some(mv) = equal_runs(&col[k - 4..=k + 4]) else { continue };
            if (mv / m).ln().abs() > 0.5 {
                continue;
            }
            let cy = col[k].1 as f64 + col[k].2 as f64 / 2.0;
            let mm = (m + mv) / 2.0;
            if let Some(c) = cands.iter_mut().find(|c| (c.0 - cx).abs() < mm && (c.1 - cy).abs() < mm) {
                let n = c.3 as f64;
                *c = ((c.0 * n + cx) / (n + 1.0), (c.1 * n + cy) / (n + 1.0), (c.2 * n + mm) / (n + 1.0), c.3 + 1);
            } else {
                cands.push((cx, cy, mm, 1));
            }
        }
    }
    cands.sort_by(|a, b| b.3.cmp(&a.3));
    // the centre module is a small isolated dark blob: its centroid is the bull's-eye
    // centre (row hits off the centre line confirm nearby points instead)
    let mut out: Vec<(f64, f64, f64, usize)> = Vec::new();
    for c in cands.into_iter().take(12) {
        let Some((x, y)) = blob_centroid(b, c.0 as usize, c.1 as usize, c.2) else { continue };
        if let Some(o) = out.iter_mut().find(|o| (o.0 - x).abs() < c.2 / 2.0 && (o.1 - y).abs() < c.2 / 2.0) {
            o.3 += c.3;
        } else {
            out.push((x, y, c.2, c.3));
        }
    }
    out.sort_by(|a, b| b.3.cmp(&a.3));
    out.into_iter().take(4).map(|c| (c.0, c.1, c.2)).collect()
}

/// Centroid of the dark blob at (x, y) if it is no larger than about one module.
fn blob_centroid(b: &Bin, x: usize, y: usize, module: f64) -> Option<(f64, f64)> {
    if !b.get(x, y) {
        return None;
    }
    let limit = (2.5 * module * module).max(4.0) as usize;
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![(x, y)];
    seen.insert((x, y));
    let (mut sx, mut sy) = (0.0, 0.0);
    while let Some((px, py)) = stack.pop() {
        sx += px as f64;
        sy += py as f64;
        if seen.len() > limit {
            return None;
        }
        for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
            let (nx, ny) = (px as i64 + dx, py as i64 + dy);
            if nx < 0 || ny < 0 || nx as usize >= b.width || ny as usize >= b.height {
                continue;
            }
            let q = (nx as usize, ny as usize);
            if b.get(q.0, q.1) && seen.insert(q) {
                stack.push(q);
            }
        }
    }
    let n = seen.len() as f64;
    Some((sx / n + 0.5, sy / n + 0.5))
}

/// Distances of the first `k` colour changes along a ray from `c` in direction `d`.
fn transitions(b: &Bin, c: P, d: P, k: usize, max: f64) -> Option<Vec<f64>> {
    let mut out = Vec::with_capacity(k);
    let start = bit_at(b, c);
    let mut cur = start;
    let mut t = 0.0;
    while out.len() < k {
        t += 0.25;
        if t > max {
            return None;
        }
        let p = (c.0 + d.0 * t, c.1 + d.1 * t);
        if p.0 < 0.0 || p.1 < 0.0 || p.0 >= b.width as f64 || p.1 >= b.height as f64 {
            return None;
        }
        let v = bit_at(b, p);
        if v != cur {
            out.push(t - 0.125);
            cur = v;
        }
    }
    Some(out)
}

const DIRS: [(f64, f64); 8] = [(1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (-1.0, 1.0), (-1.0, 0.0), (-1.0, -1.0), (0.0, -1.0), (1.0, -1.0)];

/// Fit the bull's-eye transform (module coordinates centred on the middle module) using
/// `rings` ring boundaries.
fn fit(b: &Bin, c: P, module: f64, rings: usize) -> Option<(Homography, Vec<P>, Vec<P>)> {
    let max = module * (rings as f64 + 2.0) * 2.5;
    let (sn, cs) = ring_angle(b, c, module).sin_cos();
    let mut src = Vec::new();
    let mut dst = Vec::new();
    for d in DIRS {
        let n = (d.0 * d.0 + d.1 * d.1).sqrt();
        let u = ((d.0 * cs - d.1 * sn) / n, (d.0 * sn + d.1 * cs) / n);
        let Some(ts) = transitions(b, c, u, rings, max) else { continue };
        for (k, t) in ts.iter().enumerate() {
            let r = k as f64 + 0.5;
            src.push((d.0 * r, d.1 * r));
            dst.push((c.0 + u.0 * t, c.1 + u.1 * t));
        }
    }
    if src.len() < 6 * rings {
        return None;
    }
    let mut h = Homography::from_points(&src, &dst)?;
    let (mut best_src, mut best_dst) = (src, dst);
    // refine: cast rays towards the predicted boundary points
    for _ in 0..3 {
        let centre = h.map(0.0, 0.0);
        let mut s2 = Vec::new();
        let mut d2 = Vec::new();
        for d in DIRS {
            for k in 0..rings {
                let r = k as f64 + 0.5;
                let target = h.map(d.0 * r, d.1 * r);
                let (vx, vy) = (target.0 - centre.0, target.1 - centre.1);
                let len = (vx * vx + vy * vy).sqrt();
                if len < 0.5 {
                    continue;
                }
                let u = (vx / len, vy / len);
                let Some(ts) = transitions(b, centre, u, k + 1, max) else { continue };
                let t = ts[k];
                // only accept boundaries close to the prediction
                if (t - len).abs() > module * 0.75 {
                    continue;
                }
                s2.push((d.0 * r, d.1 * r));
                d2.push((centre.0 + u.0 * t, centre.1 + u.1 * t));
            }
        }
        if s2.len() < 6 * rings {
            break;
        }
        match Homography::from_points(&s2, &d2) {
            Some(n) => {
                h = n;
                best_src = s2;
                best_dst = d2;
            }
            None => break,
        }
    }
    Some((h, best_src, best_dst))
}

/// Fit the four outer sides of the symbol (outermost dark module edges, like the Data
/// Matrix detector) around the corners predicted by `h` and add the side intersections as
/// far correspondences: the bull's-eye alone extrapolates poorly to the edge of larger
/// symbols under perspective.
fn refine_edges(b: &Bin, h: &Homography, src: &[P], dst: &[P], n: usize) -> Option<(Homography, Vec<P>, Vec<P>)> {
    let e = (n / 2) as f64 + 0.5;
    let canon = [(-e, -e), (e, -e), (e, e), (-e, e)];
    let rc: Vec<P> = canon.iter().map(|c| h.map(c.0, c.1)).collect();
    let centre = h.map(0.0, 0.0);
    let mut lines = Vec::with_capacity(4);
    for i in 0..4 {
        lines.push(dm_detect::fit_side(b, rc[i], rc[(i + 1) % 4], centre)?);
    }
    let (mut s2, mut d2) = (src.to_vec(), dst.to_vec());
    let module = ((rc[0].0 - rc[2].0).powi(2) + (rc[0].1 - rc[2].1).powi(2)).sqrt() / (2.0 * e * std::f64::consts::SQRT_2);
    for i in 0..4 {
        let q = dm_detect::intersect(&lines[(i + 3) % 4], &lines[i])?;
        if ((q.0 - rc[i].0).powi(2) + (q.1 - rc[i].1).powi(2)).sqrt() > 2.5 * module {
            return None;
        }
        // weight the far points like several near ones
        for _ in 0..4 {
            s2.push(canon[i]);
            d2.push(q);
        }
    }
    let h2 = Homography::from_points(&s2, &d2)?;
    Some((h2, s2, d2))
}

/// Full-range symbols: the central row and column belong to the reference grid, so they
/// alternate module by module from the centre to the edge. Rays along their images (straight
/// lines through the centre under a perspective transform) give transitions out to the
/// symbol edge, which pin down the transform far better than the bull's-eye alone.
fn refine_axes(b: &Bin, h: &Homography, src: &[P], dst: &[P], n: usize, module: f64) -> Homography {
    let half = (n / 2) as f64;
    let mut h = *h;
    for _ in 0..2 {
        let centre = h.map(0.0, 0.0);
        let (mut s2, mut d2) = (src.to_vec(), dst.to_vec());
        for (ax, ay) in [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)] {
            let far = h.map(ax * half, ay * half);
            let (vx, vy) = (far.0 - centre.0, far.1 - centre.1);
            let len = (vx * vx + vy * vy).sqrt();
            if len < 1.0 {
                continue;
            }
            let u = (vx / len, vy / len);
            let Some(ts) = transitions(b, centre, u, n / 2 + 1, len * 1.3 + 2.0 * module) else { continue };
            for (k, t) in ts.iter().enumerate().skip(4) {
                let r = k as f64 + 0.5;
                let pred = h.map(ax * r, ay * r);
                let pt = (centre.0 + u.0 * t, centre.1 + u.1 * t);
                if ((pred.0 - pt.0).powi(2) + (pred.1 - pt.1).powi(2)).sqrt() > 0.6 * module {
                    break;
                }
                s2.push((ax * r, ay * r));
                d2.push(pt);
            }
        }
        match Homography::from_points(&s2, &d2) {
            Some(n) => h = n,
            None => break,
        }
    }
    h
}

fn orient(o: usize, u: f64, v: f64) -> P {
    let (u, v) = if o >= 4 { (v, u) } else { (u, v) };
    match o % 4 {
        0 => (u, v),
        1 => (-v, u),
        2 => (-u, -v),
        _ => (v, -u),
    }
}

fn sample(b: &Bin, h: &Homography, o: usize, n: usize) -> Matrix {
    let c = (n / 2) as f64;
    let mut m = Matrix::new(n, n);
    for y in 0..n {
        for x in 0..n {
            let (u, v) = orient(o, x as f64 - c, y as f64 - c);
            m.set(x, y, bit_at(b, h.map(u, v)));
        }
    }
    m
}

/// Locate and decode an Aztec symbol.
pub fn detect_and_decode(b: &Bin) -> Option<String> {
    for (cx, cy, module) in centres(b) {
        for compact in [true, false] {
            let rings = if compact { 4 } else { 6 };
            let Some((h, src, dst)) = fit(b, (cx, cy), module, rings) else { continue };
            for o in 0..8 {
                let core = sample(b, &h, o, if compact { 15 } else { 19 });
                let Some((layers, _)) = aztec::read_mode(&core, compact) else { continue };
                if (compact && layers > 4) || layers == 0 {
                    continue;
                }
                let n = aztec::matrix_size(layers, compact);
                let m = sample(b, &h, o, n);
                if let Ok(t) = aztec::decode_matrix(&m) {
                    return Some(t);
                }
                let edges = refine_edges(b, &h, &src, &dst, n);
                if let Some((h2, _, _)) = &edges {
                    if let Ok(t) = aztec::decode_matrix(&sample(b, h2, o, n)) {
                        return Some(t);
                    }
                }
                if !compact {
                    let (hs, ss, ds) = edges.unwrap_or((h, src.clone(), dst.clone()));
                    let h3 = refine_axes(b, &hs, &ss, &ds, n, module);
                    if let Ok(t) = aztec::decode_matrix(&sample(b, &h3, o, n)) {
                        return Some(t);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
pub fn debug(b: &Bin) {
    let cs = centres(b);
    eprintln!("centres {:?}", cs);
    for (cx, cy, module) in cs {
        for compact in [true, false] {
            let rings = if compact { 4 } else { 6 };
            let h = fit(b, (cx, cy), module, rings).map(|f| f.0);
            eprintln!("compact {} fit {:?}", compact, h.map(|h| (h.map(0.0, 0.0), h.map(1.0, 0.0), h.map(0.0, 1.0))));
            if let Some(h) = h {
                for o in 0..8 {
                    let core = sample(b, &h, o, if compact { 15 } else { 19 });
                    if o == 0 {
                        for y in 0..core.height {
                            eprintln!("{}", (0..core.width).map(|x| if core.get(x, y) { '#' } else { '.' }).collect::<String>());
                        }
                    }
                    let md = aztec::read_mode(&core, compact);
                    eprintln!("o {} mode {:?}", o, md);
                    if let Some((layers, _)) = md {
                        let n = aztec::matrix_size(layers, compact);
                        let m = sample(b, &h, o, n);
                        for y in 0..n {
                            eprintln!("{}", (0..n).map(|x| if m.get(x, y) { '#' } else { '.' }).collect::<String>());
                        }
                        eprintln!("{:?}", aztec::decode_matrix(&m));
                    }
                }
            }
        }
    }
}
