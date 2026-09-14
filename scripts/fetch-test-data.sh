#!/usr/bin/env bash
# Downloads the small real-world ZIM files the tests read, from openZIM's
# zim-testing-suite. They are not committed: the suite declares no license.
set -euo pipefail

cd "$(dirname "$0")/.."
mkdir -p testdata
base=https://github.com/openzim/zim-testing-suite/raw/main/data
for f in nons/small.zim withns/wikibooks_be_all_nopic_2017-02.zim nons/wikipedia_en_climate_change_mini_2024-06.zim; do
  out="testdata/${f//\//_}"
  [ -s "$out" ] || curl -fsSL -o "$out" "$base/$f"
done
ls -l testdata
