# Character set tables

`src/charset/*.bin` holds the code tables used by `src/charset.rs` for Specific Character
Set (0008,0005). They are generated, never hand-edited:

```sh
python3 scripts/gen-dicom-charsets.py
```

The script prints the Python version it ran with; the committed tables were generated with
CPython 3.12.10.

| file | size | contents |
|---|---|---|
| `sb.bin` | 2,112 B | upper halves (0xA0-0xFF) of ISO 8859-1, -2, -3, -4, -5, -6, -7, -8, -9, -15 and TIS 620 (ISO-IR 100, 101, 109, 110, 144, 127, 126, 138, 148, 203, 166) |
| `jis0208.bin` | 17,672 B | JIS X 0208 (ISO-IR 87), 94 x 94 |
| `jis0212.bin` | 17,672 B | JIS X 0212 (ISO-IR 159), 94 x 94 |
| `ksx1001.bin` | 17,672 B | KS X 1001 (ISO-IR 149), 94 x 94 |
| `gb2312.bin` | 17,672 B | GB 2312 (ISO-IR 58), 94 x 94 |
| `gbk.bin` | 48,132 B | GBK two-byte area |
| `gb18030.bin` | 48,132 B | GB 18030 two-byte area |
| `gb18030r.bin` | 2,484 B | GB 18030 four-byte BMP ranges (207 runs); the supplementary planes are computed |

Total about 171 KB. JIS X 0201 (ISO-IR 13 and 14) is computed, with no table.

## Source and licence

The mappings are read from CPython's standard-library codecs (`iso8859_*`, `tis_620`,
`iso2022_jp`, `iso2022_jp_1`, `euc_kr`, `gb2312`, `gbk`, `gb18030`), distributed under the
Python Software Foundation License. CPython built those codecs from the Unicode Consortium
mapping files and the national standards (ISO/IEC 8859, TIS 620, JIS X 0208/0212,
KS X 1001, GB 2312, GBK, GB 18030); a character mapping is a fact of those standards. Using
the same codecs as pydicom also means Cameleer decodes these sets as pydicom does.

Known differences from other mappings: GB 18030 follows CPython's codec (GB 18030-2000 with
CPython's updates), so a few private-use code points that GB 18030-2005/2022 moved may map
differently; JIS X 0208 follows CPython's `iso2022_jp` (for example 0x2140 is U+FF3C).
