#! /bin/bash
# -*- mode: sh; indent-tabs-mode: nil; sh-basic-offset: 2; -*-
set -euo pipefail

cd "`dirname "$0"`/.."

force=0
push=0
while getopts "f" opt; do
  case $opt in
    f)
      force=1
      ;;
    p)
      push=1
      ;;
    \?)
      echo "error: invalid option '$OPTARG'"
      exit 1
      ;;
  esac
done

shift $((OPTIND-1))
version="${1:-}"
if [ -z "$version" ]; then
  echo "usage: $0 version"
  exit 1
fi

tag=v${version}
date=`date +"%B %Y"`

if [[ "$force" == 0 ]]; then
  if ! git diff-index --quiet HEAD --; then
    echo "error: there are uncomitted changes"
    exit 1
  fi

  if git rev-parse --verify refs/tags/$tag >/dev/null 2>&1; then
    echo "error: git tag $tag already exists"
    exit 1
  fi

  if ! grep -qF $tag CHANGES.md; then
    echo "error: no changelog found for $tag"
    exit 1
  fi
fi

echo "Updating Cargo.toml"
cargo bump $version

echo "Updating MANUAL.rst"
sed -e "s/^:Footer:.*/:Footer: reclog ${version}/" \
    -e "s/^:Date:.*/:Date: ${date}/" \
    -i MANUAL.rst

echo "Updating reclog.1"
pandoc --standalone --to man MANUAL.rst > reclog.1

echo "Rebuilding"
cargo build -q

echo "Making git commit"
git add Cargo.toml Cargo.lock MANUAL.rst reclog.1
git commit -m"Release $version"

echo "Making git tag"
git tag $tag

echo "Veryfing release"
./script/check_release.sh

if [[ "$push" == 1 ]]; then
  echo "Pushing git tag"
  git push origin $tag:$tag
fi
