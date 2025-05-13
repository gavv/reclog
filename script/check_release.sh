#! /bin/bash
# -*- mode: sh; indent-tabs-mode: nil; sh-basic-offset: 2; -*-
set -euo pipefail

cd "`dirname "$0"`/.."

git_version=`git describe --tags HEAD | sed -e 's/^v//'`
toml_version=`sed -n 's/^version\s*=\s*"\(.*\)"/\1/p' Cargo.toml | head -1`
rst_version=`sed -n 's/^:Footer: reclog \(.*\)/\1/p' MANUAL.rst`
troff_version=`sed -n 's/.TH .* "reclog \(.*\)"/\1/p' reclog.1`

echo "Detected versions:"
echo "  Git tag:     $git_version"
echo "  Cargo.toml:  $toml_version"
echo "  MANUAL.rst:  $rst_version"
echo "  reclog.1:    $troff_version"

if [ "${1:-}" != "-n" ]; then
  if [[ "$git_version" != "$toml_version" || "$git_version" != "$rst_version" \
          || "$git_version" != "$troff_version" ]]; then
    echo
    echo "Version mismatch detected!"
    exit 1
  else
    echo
    echo "All good!"
    exit 0
  fi
fi
