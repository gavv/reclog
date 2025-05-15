# Changelog

## [v0.1.4][v0.1.4] - 16 May 2025

* Fix macOS support:
  * remove gettid() usage ([4161962][4161962])
  * add build-time detection of presence of libc functions ([c8d3c41][c8d3c41])
  * simulate sigtimedwait() on platforms that don't have it ([5111f18][5111f18])
  * fix handling of SIGCHILD on macOS ([0890639][0890639])
* Handle errors when writing to stdout/stderr ([0de51b2][0de51b2])
* Set panic mode to "abort"
* Improve CI: add macOS, cover both debug & release, cover (un)install
* Improve scripts

[v0.1.4]: https://github.com/gavv/reclog/releases/tag/v0.1.4

[4161962]: https://github.com/gavv/reclog/commit/4161962c826af0022bb973ef967725bc2413d5e6
[c8d3c41]: https://github.com/gavv/reclog/commit/c8d3c412d580fc84c714b11c58f2317b07a49a7e
[5111f18]: https://github.com/gavv/reclog/commit/5111f1895cd964f9cee2507e0726d483d2220286
[0890639]: https://github.com/gavv/reclog/commit/089063954d8a1694b05c253561282b6079d55822
[0de51b2]: https://github.com/gavv/reclog/commit/0de51b2634f5cacf7b4ab6d5f9af8af33abca32b

## [v0.1.3][v0.1.3] - 14 May 2025

* Fix hang when blocked on full pty buffer
* Allow to interrupt final phase of graceful termination
* Add `--debug` logs
* Improve documentation

[v0.1.3]: https://github.com/gavv/reclog/releases/tag/v0.1.3

## [v0.1.2][v0.1.2] - 13 May 2025

* Update documentation
* Add scripts

[v0.1.2]: https://github.com/gavv/reclog/releases/tag/v0.1.2

## [v0.1.1][v0.1.1] - 12 May 2025

* Auto publish on crates.io

[v0.1.1]: https://github.com/gavv/reclog/releases/tag/v0.1.1

## [v0.1.0][v0.1.0] - 12 May 2025

* Initial release

[v0.1.0]: https://github.com/gavv/reclog/releases/tag/v0.1.0
