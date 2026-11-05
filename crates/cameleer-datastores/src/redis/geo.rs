//! Redis geo encoding: 52-bit interleaved geohash scores (as stored in the sorted set
//! behind `GEOADD`), the standard base32 geohash text of `GEOHASH`, and haversine
//! distances (Redis' earth radius) — used by the embedded server.

/// Latitude limits of the Redis geo index (EPSG:900913 / Web Mercator).
pub const LAT_MIN: f64 = -85.05112878;
pub const LAT_MAX: f64 = 85.05112878;
pub const LON_MIN: f64 = -180.0;
pub const LON_MAX: f64 = 180.0;
/// Bits per coordinate.
pub const STEP: u32 = 26;
/// Earth radius used by Redis (meters).
pub const EARTH_RADIUS_M: f64 = 6372797.560856;

fn spread(v: u32) -> u64 {
    let mut x = v as u64;
    x = (x | (x << 16)) & 0x0000_FFFF_0000_FFFF;
    x = (x | (x << 8)) & 0x00FF_00FF_00FF_00FF;
    x = (x | (x << 4)) & 0x0F0F_0F0F_0F0F_0F0F;
    x = (x | (x << 2)) & 0x3333_3333_3333_3333;
    (x | (x << 1)) & 0x5555_5555_5555_5555
}

fn squash(v: u64) -> u32 {
    let mut x = v & 0x5555_5555_5555_5555;
    x = (x | (x >> 1)) & 0x3333_3333_3333_3333;
    x = (x | (x >> 2)) & 0x0F0F_0F0F_0F0F_0F0F;
    x = (x | (x >> 4)) & 0x00FF_00FF_00FF_00FF;
    x = (x | (x >> 8)) & 0x0000_FFFF_0000_FFFF;
    ((x | (x >> 16)) & 0x0000_0000_FFFF_FFFF) as u32
}

fn encode_range(lon: f64, lat: f64, lat_min: f64, lat_max: f64) -> u64 {
    let scale = (1u64 << STEP) as f64;
    let lat_off = (((lat - lat_min) / (lat_max - lat_min)) * scale) as u32;
    let lon_off = (((lon - LON_MIN) / (LON_MAX - LON_MIN)) * scale) as u32;
    // latitude bits at even positions, longitude bits at odd positions (Redis interleave64)
    spread(lat_off) | (spread(lon_off) << 1)
}

/// Whether a coordinate pair is accepted by `GEOADD`.
pub fn valid(lon: f64, lat: f64) -> bool {
    (LON_MIN..=LON_MAX).contains(&lon) && (LAT_MIN..=LAT_MAX).contains(&lat)
}

/// The 52-bit score of a position.
pub fn encode(lon: f64, lat: f64) -> u64 {
    encode_range(lon, lat, LAT_MIN, LAT_MAX)
}

/// Center of the cell of a score: `(longitude, latitude)`.
pub fn decode(hash: u64) -> (f64, f64) {
    let scale = (1u64 << STEP) as f64;
    let lat_off = squash(hash) as f64;
    let lon_off = squash(hash >> 1) as f64;
    let lat_lo = LAT_MIN + (lat_off / scale) * (LAT_MAX - LAT_MIN);
    let lat_hi = LAT_MIN + ((lat_off + 1.0) / scale) * (LAT_MAX - LAT_MIN);
    let lon_lo = LON_MIN + (lon_off / scale) * (LON_MAX - LON_MIN);
    let lon_hi = LON_MIN + ((lon_off + 1.0) / scale) * (LON_MAX - LON_MIN);
    (((lon_lo + lon_hi) / 2.0).clamp(LON_MIN, LON_MAX), ((lat_lo + lat_hi) / 2.0).clamp(LAT_MIN, LAT_MAX))
}

/// The 11-character standard geohash (`GEOHASH`), computed like Redis from the stored score.
pub fn geohash_text(score: u64) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789bcdefghjkmnpqrstuvwxyz";
    let (lon, lat) = decode(score);
    let bits = encode_range(lon, lat, -90.0, 90.0);
    (0..11)
        .map(|i| {
            let idx = if i == 10 { 0 } else { (bits >> (52 - (i + 1) * 5)) & 0x1f };
            ALPHABET[idx as usize] as char
        })
        .collect()
}

/// Great-circle distance in meters.
pub fn distance(lon1: f64, lat1: f64, lon2: f64, lat2: f64) -> f64 {
    let (lat1r, lat2r) = (lat1.to_radians(), lat2.to_radians());
    let u = ((lat2r - lat1r) / 2.0).sin();
    let v = ((lon2 - lon1).to_radians() / 2.0).sin();
    2.0 * EARTH_RADIUS_M * (u * u + lat1r.cos() * lat2r.cos() * v * v).sqrt().asin()
}

/// Meters per unit (`m`, `km`, `mi`, `ft`).
pub fn unit_factor(unit: &str) -> Option<f64> {
    Some(match unit.to_ascii_lowercase().as_str() {
        "m" => 1.0,
        "km" => 1000.0,
        "mi" => 1609.34,
        "ft" => 0.3048,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_reference_values() {
        // GEOADD Sicily 13.361389 38.115556 "Palermo" 15.087269 37.502669 "Catania" (redis.io docs)
        let p = encode(13.361389, 38.115556);
        let c = encode(15.087269, 37.502669);
        assert_eq!(p, 3479099956230698);
        assert_eq!(c, 3479447370796909);
        let (lon, lat) = decode(p);
        assert!((lon - 13.36138933897018433).abs() < 1e-6 && (lat - 38.11555639549629859).abs() < 1e-6, "{} {}", lon, lat);
        let (lo1, la1) = decode(p);
        let (lo2, la2) = decode(c);
        // GEODIST Sicily Palermo Catania -> "166274.1516"
        assert!((distance(lo1, la1, lo2, la2) - 166274.1516).abs() < 0.01);
        // GEOHASH Sicily Palermo Catania -> "sqc8b49rny0" "sqdtr74hyu0"
        assert_eq!(geohash_text(p), "sqc8b49rny0");
        assert_eq!(geohash_text(c), "sqdtr74hyu0");
        assert!(!valid(0.0, 86.0));
    }
}
