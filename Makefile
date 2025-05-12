export PATH := dist/bin:$(PATH)
SHELL := bash

DESTDIR ?= /usr/local

build:
	mkdir -p dist
	cargo install -f --root dist --path .

install:
	mkdir -p $(DESTDIR)/bin
	mkdir -p $(DESTDIR)/share/man/man1
	mkdir -p $(DESTDIR)/share/doc/reclog
	cp dist/bin/reclog $(DESTDIR)/bin/reclog
	cp reclog.1 $(DESTDIR)/share/man/man1/reclog.1
	cp AUTHORS.md CHANGES.md LICENSE $(DESTDIR)/share/doc/reclog

uninstall:
	rm -f $(DESTDIR)/bin/reclog
	rm -f $(DESTDIR)/share/man/man1/reclog.1
	rm -rf $(DESTDIR)/share/doc/reclog

dev:
	cargo build
	cargo clippy

docs:
	version=`sed -n 's/^version\s*=\s*"\(.*\)"/\1/p' Cargo.toml | head -1`; \
	date=`date +"%B %Y"`; \
		sed -e "s/^:Footer:.*/:Footer: reclog $$version/" \
			-e "s/^:Date:.*/:Date: $$date/" \
			-i MANUAL.rst
	pandoc --standalone --to man MANUAL.rst > reclog.1
	md-authors --format modern --append AUTHORS.md

clean:
	cargo clean
	rm -rf dist

fmt:
	cargo fmt
