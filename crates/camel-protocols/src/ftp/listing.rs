//! Parsing of FTP directory listings: Unix `ls -l` style and DOS style `LIST` output,
//! and RFC 3659 `MLSD` facts.

use camel_core::util_time::{civil_from_epoch, epoch_from_civil, now_millis};

/// An entry of a remote directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFile {
    /// The entry name (no directory part).
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    /// Last modification time in epoch millis (0 = unknown).
    pub modified: i64,
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

/// Month name (`Jan`) to 1..=12.
pub fn month_number(s: &str) -> Option<u32> {
    let l = s.to_ascii_lowercase();
    MONTHS.iter().position(|m| *m == l).map(|i| i as u32 + 1)
}

/// Three letter English month name for 1..=12.
pub fn month_name(m: u32) -> &'static str {
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(m.clamp(1, 12) - 1) as usize]
}

/// Whitespace separated tokens with their byte spans.
fn spans(line: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                out.push((s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push((s, line.len()));
    }
    out
}

/// Parse one line of a `LIST` response (Unix or DOS format). Returns `None` for lines
/// that are not entries (e.g. `total 12`) or that cannot be parsed.
pub fn parse_list_line(line: &str) -> Option<RemoteFile> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() || line.starts_with("total ") {
        return None;
    }
    parse_unix(line).or_else(|| parse_dos(line))
}

fn parse_unix(line: &str) -> Option<RemoteFile> {
    let sp = spans(line);
    let tok = |i: usize| &line[sp[i].0..sp[i].1];
    if sp.len() < 6 {
        return None;
    }
    let perms = tok(0);
    let kind = perms.chars().next()?;
    if !"-dlbcps".contains(kind) || perms.len() < 10 {
        return None;
    }
    // locate "<size> <Mon> <day> <time|year> <name...>"
    let mi = (3..sp.len().saturating_sub(3)).find(|&i| month_number(tok(i)).is_some() && tok(i + 1).parse::<u32>().is_ok())?;
    let size = tok(mi - 1).parse::<u64>().unwrap_or(0);
    let month = month_number(tok(mi))?;
    let day = tok(mi + 1).parse::<u32>().ok()?;
    let ty = tok(mi + 2);
    let (year, hour, minute) = if let Some((h, m)) = ty.split_once(':') {
        let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
        let now_secs = now_millis() / 1000;
        let (cy, ..) = civil_from_epoch(now_secs);
        // a date "in the future" (beyond a day of clock skew) belongs to the previous year
        let y = if epoch_from_civil(cy, month, day, h, m, 0) > now_secs + 86_400 { cy - 1 } else { cy };
        (y, h, m)
    } else {
        (ty.parse::<i64>().ok()?, 0, 0)
    };
    let mut name = line[sp[mi + 3].0..].to_string();
    if kind == 'l' {
        if let Some(i) = name.find(" -> ") {
            name.truncate(i);
        }
    }
    if name == "." || name == ".." {
        return None;
    }
    Some(RemoteFile {
        name,
        is_dir: kind == 'd',
        is_link: kind == 'l',
        size,
        modified: epoch_from_civil(year, month, day, hour, minute, 0) * 1000,
    })
}

fn parse_dos(line: &str) -> Option<RemoteFile> {
    // 01-15-23  02:30PM       <DIR>          name
    let sp = spans(line);
    if sp.len() < 4 {
        return None;
    }
    let tok = |i: usize| &line[sp[i].0..sp[i].1];
    let date: Vec<&str> = tok(0).split('-').collect();
    if date.len() != 3 {
        return None;
    }
    let (mo, d, y) = (date[0].parse::<u32>().ok()?, date[1].parse::<u32>().ok()?, date[2].parse::<i64>().ok()?);
    let y = if y < 70 { 2000 + y } else if y < 100 { 1900 + y } else { y };
    let t = tok(1).to_ascii_uppercase();
    let pm = t.ends_with("PM");
    let t = t.trim_end_matches("AM").trim_end_matches("PM");
    let (h, m) = t.split_once(':')?;
    let (mut h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
    if pm && h < 12 {
        h += 12;
    } else if !pm && h == 12 && tok(1).to_ascii_uppercase().ends_with("AM") {
        h = 0;
    }
    let is_dir = tok(2).eq_ignore_ascii_case("<DIR>");
    let size = if is_dir { 0 } else { tok(2).parse::<u64>().ok()? };
    Some(RemoteFile { name: line[sp[3].0..].to_string(), is_dir, is_link: false, size, modified: epoch_from_civil(y, mo, d, h, m, 0) * 1000 })
}

/// Parse an RFC 3659 time value `YYYYMMDDHHMMSS[.sss]` (UTC) to epoch millis.
pub fn parse_mdtm(s: &str) -> Option<i64> {
    let s = s.trim();
    let (main, frac) = match s.split_once('.') {
        Some((a, b)) => (a, b),
        None => (s, ""),
    };
    if main.len() != 14 || !main.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |a: usize, b: usize| main[a..b].parse::<u32>().ok();
    let secs = epoch_from_civil(main[0..4].parse().ok()?, n(4, 6)?, n(6, 8)?, n(8, 10)?, n(10, 12)?, n(12, 14)?);
    let ms = format!("{:0<3}", &frac[..frac.len().min(3)]).parse::<i64>().unwrap_or(0);
    Some(secs * 1000 + ms)
}

/// Format epoch millis as an RFC 3659 time value `YYYYMMDDHHMMSS`.
pub fn format_mdtm(millis: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_from_epoch(millis.div_euclid(1000));
    format!("{:04}{:02}{:02}{:02}{:02}{:02}", y, mo, d, h, mi, s)
}

/// Parse one line of an `MLSD` response: `fact=value;fact=value; name`. Current and
/// parent directory entries (`type=cdir`/`pdir`) are skipped.
pub fn parse_mlsd_line(line: &str) -> Option<RemoteFile> {
    let line = line.trim_end_matches(['\r', '\n']);
    let (facts, name) = line.split_once(' ')?;
    let mut f = RemoteFile { name: name.to_string(), is_dir: false, is_link: false, size: 0, modified: 0 };
    for fact in facts.split(';').filter(|s| !s.is_empty()) {
        let (k, v) = fact.split_once('=')?;
        match k.to_ascii_lowercase().as_str() {
            "type" => match v.to_ascii_lowercase().as_str() {
                "cdir" | "pdir" => return None,
                "dir" => f.is_dir = true,
                t if t.contains("symlink") || t.contains("slink") => f.is_link = true,
                _ => {}
            },
            "size" | "sizd" => f.size = v.parse().unwrap_or(0),
            "modify" => f.modified = parse_mdtm(v).unwrap_or(0),
            _ => {}
        }
    }
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_listing() {
        let f = parse_list_line("-rw-r--r--    1 ftp      ftp          1234 Mar 05  2021 report 2021.txt").unwrap();
        assert_eq!(f.name, "report 2021.txt");
        assert_eq!(f.size, 1234);
        assert!(!f.is_dir);
        assert_eq!(f.modified, epoch_from_civil(2021, 3, 5, 0, 0, 0) * 1000);
        let d = parse_list_line("drwxr-xr-x 2 owner group 4096 Jan 1 10:20 sub").unwrap();
        assert!(d.is_dir);
        assert_eq!(d.name, "sub");
        let l = parse_list_line("lrwxrwxrwx 1 root root 7 Feb 2 2020 link -> target").unwrap();
        assert!(l.is_link);
        assert_eq!(l.name, "link");
        // no group column
        let f = parse_list_line("-rw-r--r-- 1 user 77 Dec 24 2019 x.csv").unwrap();
        assert_eq!((f.name.as_str(), f.size), ("x.csv", 77));
        assert!(parse_list_line("total 8").is_none());
        assert!(parse_list_line("drwxr-xr-x 2 o g 4096 Jan 1 2020 .").is_none());
    }

    #[test]
    fn dos_listing() {
        let d = parse_list_line("01-15-23  02:30PM       <DIR>          My Dir").unwrap();
        assert!(d.is_dir);
        assert_eq!(d.name, "My Dir");
        let f = parse_list_line("12-31-99  12:05AM                 42 old.txt").unwrap();
        assert_eq!((f.size, f.name.as_str()), (42, "old.txt"));
        assert_eq!(f.modified, epoch_from_civil(1999, 12, 31, 0, 5, 0) * 1000);
    }

    #[test]
    fn mlsd() {
        let f = parse_mlsd_line("type=file;size=11;modify=20240102030405.5;perm=r; hello world.txt").unwrap();
        assert_eq!(f.name, "hello world.txt");
        assert_eq!(f.size, 11);
        assert_eq!(f.modified, epoch_from_civil(2024, 1, 2, 3, 4, 5) * 1000 + 500);
        assert!(parse_mlsd_line("type=dir;modify=20240102030405; sub").unwrap().is_dir);
        assert!(parse_mlsd_line("type=cdir; .").is_none());
        assert_eq!(format_mdtm(parse_mdtm("20200229235959").unwrap()), "20200229235959");
    }
}
