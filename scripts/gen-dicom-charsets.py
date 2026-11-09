#!/usr/bin/env python3
"""Generate the DICOM character set tables in crates/cameleer-dicom/src/charset/.

The mappings come from CPython's codecs (standard library, PSF licence); those codecs
were built from the Unicode Consortium mapping files and the national standards
(ISO 8859, TIS 620, JIS X 0208/0212, KS X 1001, GB 2312, GBK, GB 18030). Run with the
same Python version to get identical output (the generator prints the version used).

Output files (all little-endian, committed):
  sb.bin        11 single-byte G1 tables x 96 u16 (code 0xA0..0xFF -> code point, 0 = unmapped)
                order: ISO-IR 100, 101, 109, 110, 144, 127, 126, 138, 148, 203, 166
  jis0208.bin   94 x 94 u16 (row 0x21.., cell 0x21..), JIS X 0208 (ISO-IR 87)
  jis0212.bin   94 x 94 u16, JIS X 0212 (ISO-IR 159)
  ksx1001.bin   94 x 94 u16, KS X 1001 (ISO-IR 149)
  gb2312.bin    94 x 94 u16, GB 2312 (ISO-IR 58)
  gbk.bin       126 x 191 u16 (lead 0x81..0xFE, trail 0x40..0xFE), GBK
  gb18030.bin   126 x 191 u16, the two-byte area of GB 18030
  gb18030r.bin  (u32 linear, u32 code point, u32 length) runs of the four-byte BMP area of GB 18030;
                linear = ((b1-0x81)*10 + (b2-0x30))*1260 + (b3-0x81)*10 + (b4-0x30)
Standard library only.
"""

import os
import struct
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "crates", "cameleer-dicom", "src", "charset")

SINGLE = ["iso8859_1", "iso8859_2", "iso8859_3", "iso8859_4", "iso8859_5", "iso8859_6",
          "iso8859_7", "iso8859_8", "iso8859_9", "iso8859_15", "tis_620"]


def cp(b, codec):
    try:
        s = b.decode(codec)
    except UnicodeDecodeError:
        return 0
    if len(s) != 1 or ord(s) > 0xFFFF or s == "�":
        return 0
    return ord(s)


def write(name, data):
    with open(os.path.join(OUT, name), "wb") as f:
        f.write(data)
    print(f"{name}: {len(data)} bytes")


def table94(prefix, codec, high):
    out = []
    for r in range(0x21, 0x7F):
        for c in range(0x21, 0x7F):
            b = bytes([r | 0x80, c | 0x80]) if high else bytes([r, c])
            out.append(cp(prefix + b, codec))
    return struct.pack("<%dH" % len(out), *out)


def table2(codec):
    out = []
    for lead in range(0x81, 0xFF):
        for trail in range(0x40, 0xFF):
            out.append(0 if trail == 0x7F else cp(bytes([lead, trail]), codec))
    return struct.pack("<%dH" % len(out), *out)


def main():
    os.makedirs(OUT, exist_ok=True)
    print("python", sys.version.split()[0])
    sb = []
    for codec in SINGLE:
        for b in range(0xA0, 0x100):
            sb.append(cp(bytes([b]), codec))
    write("sb.bin", struct.pack("<%dH" % len(sb), *sb))
    write("jis0208.bin", table94(b"\x1b$B", "iso2022_jp", False))
    write("jis0212.bin", table94(b"\x1b$(D", "iso2022_jp_1", False))
    write("ksx1001.bin", table94(b"", "euc_kr", True))
    write("gb2312.bin", table94(b"", "gb2312", True))
    write("gbk.bin", table2("gbk"))
    write("gb18030.bin", table2("gb18030"))
    runs = []
    for lin in range(0, 39420):
        b1, r = divmod(lin, 12600)
        b2, r = divmod(r, 1260)
        b3, b4 = divmod(r, 10)
        b = bytes([0x81 + b1, 0x30 + b2, 0x81 + b3, 0x30 + b4])
        c = cp(b, "gb18030")
        if c == 0:
            continue
        if runs and runs[-1][0] + runs[-1][2] == lin and runs[-1][1] + runs[-1][2] == c:
            runs[-1][2] += 1
        else:
            runs.append([lin, c, 1])
    data = b"".join(struct.pack("<III", l, c, n) for l, c, n in runs)
    write("gb18030r.bin", data)


if __name__ == "__main__":
    main()
