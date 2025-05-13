#! /bin/bash
# -*- mode: sh; indent-tabs-mode: nil; sh-basic-offset: 2; -*-
set -euo pipefail

cd "`dirname "$0"`/.."

echo "Updating README.md"

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

echo "Updating AUTHORS.md"

md-authors --format modern --append AUTHORS.md

echo "Updating reclog.1"

pandoc --standalone --to man MANUAL.rst > reclog.1
