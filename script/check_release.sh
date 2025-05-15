#! /bin/bash
# -*- mode: sh; indent-tabs-mode: nil; sh-basic-offset: 2; -*-
set -euo pipefail

cd "`dirname "$0"`/.."

git_version=`git describe --tags HEAD | sed -e 's/^v//'`
cargo_toml_version=`sed -n 's/^version\s*=\s*"\(.*\)"/\1/p' Cargo.toml | head -1`
changes_md_version=`sed -n 's/^##\s*\[v\([0-9.]*\)\].*/\1/p' CHANGES.md | head -1`
man_rst_version=`sed -n 's/^:Footer: reclog \(.*\)/\1/p' MANUAL.rst`
man_troff_version=`sed -n 's/.TH .* "reclog \(.*\)"/\1/p' reclog.1`

echo "Detected versions:"
echo "  Git tag:     $git_version"
echo "  Cargo.toml:  $cargo_toml_version"
echo "  CHANGES.md:  $changes_md_version"
echo "  MANUAL.rst:  $man_rst_version"
echo "  reclog.1:    $man_troff_version"

if [ "${1:-}" != "-n" ]; then
  if [[ "$git_version" != "$cargo_toml_version" \
          || "$git_version" != "$changes_md_version" \
          || "$git_version" != "$man_rst_version" \
          || "$git_version" != "$man_troff_version" ]]; then
    echo
    echo "Version mismatch detected!"
    exit 1
  else
    echo
    echo "All good!"
    exit 0
  fi
fi
