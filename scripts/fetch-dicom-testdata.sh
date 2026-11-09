#!/usr/bin/env bash
# Download the public DICOM test files used by the corpus test
# (crates/cameleer-dicom/tests/corpus.rs) into target/dicom-testdata (not committed).
#
# Source: the pydicom repository at tag v3.0.1 (https://github.com/pydicom/pydicom), MIT
# licence; its test_files/README.txt records where each file came from (NEMA WG-04 sample
# images, David Clunie's character set and compression samples, the DCMTK SR example) and that
# the files were downsized and binary-edited to remove anything resembling a patient name. The
# files and their provenance are listed in crates/cameleer-dicom/TESTDATA.md.
#
# Every file is checked against its pinned SHA-256; a mismatch aborts.
#
#   scripts/fetch-dicom-testdata.sh            # download into target/dicom-testdata
#   CAMEL_DICOM_TESTDATA=target/dicom-testdata cargo test -p cameleer-dicom --test corpus
set -euo pipefail

TAG=v3.0.1
BASE="https://raw.githubusercontent.com/pydicom/pydicom/$TAG/src/pydicom/data"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/target/dicom-testdata}"
mkdir -p "$OUT"

# path under data/ | sha256
FILES="
test_files/CT_small.dcm 3dd31e5cc835b3f2cdd46c9da1982f59251e78518fefa8163d914631c66437d6
test_files/MR_small.dcm 3f27d1c22f1a66e80d7bb7c911e8610fd0bb70325a76746a7adb1c0ddefcf2bb
test_files/MR_small_implicit.dcm 6077442c42a56fc7fcc7db8411a657dded9fc109e6d3275765c4de358292b299
test_files/MR_small_bigendian.dcm 3e4c8c9fe70de4f3be149bbd673fa56f211c8e8e2ff9bac63f70f9dc31b5d108
test_files/ExplVR_BigEndNoMeta.dcm a56be8c8c52f0d1cf55d7c2ced6abc6f22b799cbf556b84b856055a7a7565949
test_files/ExplVR_LitEndNoMeta.dcm 008e9302975d34899d89b4e3f044f8637b16acde25242eb0b36a1ffc034b9b42
test_files/image_dfl.dcm 0029ebbba17e7c6f081408d433cd28b5d1cfee0eeb4cff509b4d972ffa9daf27
test_files/JPEG2000.dcm 5be539024e6803029a7b73c0f8e72e88d032e3a0bc05922c0c047344780aa8e1
test_files/rtplan.dcm 18585dbbd6f7c5d1b7e749d6976d72251802ad89d65bccd31c03006f95aab89b
test_files/rtstruct.dcm 40c41bdf871fd8553396b02476a66024ed23c04927c0dc53fd10ecd3472cd0d3
test_files/rtdose.dcm 1d6cc092146d093e086a6bcccef4ebb7d097941343f5cd3b6395d157b64e37e4
test_files/nested_priv_SQ.dcm 5cc694964c10cb02b501f02585909a7f4ef4656c07a89e603d1833629919c5c6
test_files/priv_SQ.dcm 025ea791cd965b7977a3f1394d64bd3617e73dbc796627c989bdcbb0641270a2
test_files/UN_sequence.dcm 09f033e0d40a18c4ade0b08b8d318ab6deb684a84f503d4ec4f00908883dd431
test_files/test-SR.dcm eebf00a37e97503b5a65022f9c2f89db6e8dac4cc632682aa3456aee1b6c177e
test_files/reportsi.dcm 59ca5f4fbf524bd542a907f8f29028be510e9d907239dbe2f1c82ffc5088538b
test_files/no_meta_group_length.dcm 76c6af82b4246285f6e5b53f150af44b9b7e2afe7ae25e1b604c549ba3d16733
charset_files/chrArab.dcm 7020ecdbb68bdd13264daeb29fabe636a28d79a1a83ecbb65a0d996c68458c1e
charset_files/chrFren.dcm 8363f3d2e55b448a688ed7863675bf9645e77a919486ddfb8faa12227e28b097
charset_files/chrFrenMulti.dcm ea01c20111b638327d6d9c003631a0396167267bfc69c53815f055c76e2dafc1
charset_files/chrGerm.dcm 49a285554a4ef62ae97c31c1c8c15e9fc3289570a0f56f446e6a652c799dae5d
charset_files/chrGreek.dcm cdbdf7820642c13c26b49e496d571f390418d3078e75b2c070e5c668861ed49b
charset_files/chrH31.dcm 37b1165fc2b35cbe12f0b036a439d1c69412adb34ce5a387d23191fc2d285f48
charset_files/chrH32.dcm de42af715ac11d701d493ac34b1cf477d3e1f2d5d05e3441738a68d495f7708f
charset_files/chrHbrw.dcm 5065bb5c8e558ecc85cc48113a1b0bf07701eaf2e9fa1b7b733669c7ad8ef9c4
charset_files/chrI2.dcm 1d2ed1aa27c01ca85ed2482d6ffe97249d3f276661fa65b99c1b1103b78aa0cc
charset_files/chrJapMulti.dcm 9f69e68ba12651b28d0f76047b0f77628e3054cc16461746ad3622fa8bd4a945
charset_files/chrJapMultiExplicitIR6.dcm bd45c304b43e1f894942c30239f463156e0e63986fb1dad4c2625b59aa32e01f
charset_files/chrKoreanMulti.dcm f07d6193310e593fd7f8ecf3e301da01c82c47dc22973896f2a671c69d7fdd4d
charset_files/chrRuss.dcm e82d8856b7d9fb407a80a2824dc7adf8daf7ced265450d698955f754a9af1730
charset_files/chrSQEncoding.dcm b124a74bcf2f258ee8c99c354208eb7ceb3969e7f2e827d9dd6e41905facaa7e
charset_files/chrX1.dcm 133232a666587ee884804cb07aaa4becf36f720bc5919a0781732ce691f5dedc
charset_files/chrX2.dcm c626f310e03012138456589f5167547c5bfecf7adef13bb1ed43a21dcb274ee8
"

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -c1-64; else shasum -a 256 "$1" | cut -c1-64; fi
}

n=0
while read -r path sum; do
    [ -z "$path" ] && continue
    name="$(basename "$path")"
    dest="$OUT/$name"
    if [ -f "$dest" ] && [ "$(sha256 "$dest")" = "$sum" ]; then
        n=$((n + 1))
        continue
    fi
    curl -fsSL -o "$dest.tmp" "$BASE/$path"
    got="$(sha256 "$dest.tmp")"
    if [ "$got" != "$sum" ]; then
        rm -f "$dest.tmp"
        echo "checksum mismatch for $path: expected $sum, got $got" >&2
        exit 1
    fi
    mv "$dest.tmp" "$dest"
    n=$((n + 1))
done <<< "$FILES"
echo "$n files in $OUT"
