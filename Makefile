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

docs: dev
	./script/update_docs.sh

clean:
	cargo clean
	rm -rf dist

fmt:
	cargo fmt
