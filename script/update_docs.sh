#! /bin/bash
set -euo pipefail

echo "updating README.md ..."

markdown-toc --maxdepth 2 --bullets=- -i README.md

( cat <<'EOF'
<!-- help -->

```
$ reclog --help
EOF

script -qc "stty cols 90; target/debug/reclog --help" /dev/null | tr -d '\r'

cat <<'EOF'
```

<!-- helpstop -->
EOF
) | sed -e '/<!-- help -->/{r /dev/stdin' \
        -e 'N' \
        -e '}; /<!-- help -->/,/<!-- helpstop -->/d' \
        -i README.md

echo "updating MANUAL.rst ..."

version=`sed -n 's/^version\s*=\s*"\(.*\)"/\1/p' Cargo.toml | head -1`
date=`date +"%B %Y"`

sed -e "s/^:Footer:.*/:Footer: reclog ${version}/" \
    -e "s/^:Date:.*/:Date: ${date}/" \
    -i MANUAL.rst

pandoc --standalone --to man MANUAL.rst > reclog.1

echo "updating AUTHORS.md ..."

md-authors --format modern --append AUTHORS.md

echo "all done."
