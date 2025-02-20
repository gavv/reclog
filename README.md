# reclog [![Build](https://github.com/gavv/reclog/workflows/build/badge.svg)](https://github.com/gavv/reclog/actions)

`reclog` is a command-line tool to capture command output to a file.

It runs the specified command in a pseudo-terminal (pty), connecting its own stdin and stdout with pty's input and output, and in addition duplicates pty's output to a file.

Features
--------

This tool is similar to [unbuffer(1)](https://linux.die.net/man/1/unbuffer), [tee(1)](https://linux.die.net/man/1/tee), and [ts(1)](https://linux.die.net/man/1/ts), but is handier to use and provides a few features that are specifically useful when recording live logs:

* **runs command in a pty (unlike `tee`, and like `unbuffer`)**

    It makes recording transparent to the invoked command. E.g. if the command supports colors, they will work out of the box.

* **ensures that a slow stdout doesn't block the command**

    If the command produces output faster than stdout (e.g. terminal or pipe) can handle it, `reclog` will drop some output. Terminals are often slow, and this feature allows not to bother that live logs can affect testing.

    At the same time, the recorded file always gets the full output.

* **handles graceful termination and pause/resume**

    Hit `^C` for graceful termination (ask child to exit and wait for pending logs), `^\` for emergency termination (quickly kill child and exit), `^Z` for graceful pause (you can then type `fg` to resume).

    If the child process is stuck during graceful termination or pause, you can hit `^C` or `^Z` second time to terminate or pause forcibly.

* **can strip ANSI escape codes from the output file**

    E.g. if the command supports colors, colors will be present in the console, but not in the recorded file.

* **can prepend timestamps to the output lines (like `ts`)**

    This is useful when the command itself does not include timestamp in its logs. Both absolute and relative timestamps are supported.

* **can add a header with meta-information**

    This is useful when you collect logs from different machines or invocations, making recorded files self-describing. Header contains info like hostname, OS, current time, and the command being run.

Limitations
-----------

* The invoked command should be a non-interactive program that uses terminal in canonical mode (i.e. with line-buffered input, control characters, etc.)

    If the command just reads lines from stdin and writes lines to stdout, probably with ANSI escape codes, that's perfectly fine. If the command performs some non-trivial configuration of the TTY, things may happen.

* The invoked command should keeps its child processes (if any) in the same process group and with the same controlling TTY

    If the command spawns background processes with double-fork or daemon(3), those processes may not be automatically terminated when reclog exits.

Platforms
---------

Any UNIX-like OS should be fine, including Linux, BSD, and macOS.

However, only Linux was tested so far.

Prerequisites
-------------

You need to install Rust and Cargo (Rust's package manager).

One way is to use `rustup`, as suggested by the [official instructions](https://doc.rust-lang.org/cargo/getting-started/installation.html).

Another way is to use you distro's package manager.

For example, on Ubuntu:

```
sudo apt install rustc cargo
```

And on macOS:

```
brew install rust
```

Install from git
----------------

Clone repo:

```
git clone https://github.com/gavv/reclog.git
cd reclog
```

Build:

```
make
```

Install for all users into /usr/local:

```
sudo make install
```

Or install for current user into ~/.local:

```
make install DESTDIR=$HOME/.local
```

(In this case, ensure that `~/.local/bin` is in PATH and `~/.local/share/man` is in MANPATH).

Install from crate
------------------

Download, build and install executable:

```
cargo install reclog
```

(Ensure that `~/.cargo/bin` is added to PATH).

Optionally, install man page:

```
mkdir -p ~/.local/share/man/man1
reclog --man > ~/.local/share/man/man1/reclog.1
```

(Ensure that `~/.local/share/man` is added to MANPATH).

Manual page
-----------

Manual page is available after installation via `man reclog` and online here: [MANUAL.rst](MANUAL.rst).

You can also read it by running:

```
reclog --man | man -l -
```

Usage examples
--------------

Basic usage. The output will be saved to `tshark.log`, with colors stripped out.

```
$ reclog tshark --color -i lo -f tcp
Capturing on 'Loopback: lo'
 ** (tshark:1503378) 21:35:13.392151 [Main MESSAGE] -- Capture started.
 ** (tshark:1503378) 21:35:13.392197 [Main MESSAGE] -- File: "/tmp/wireshark_loIWPI62.pcapng"
    1 0.000000000          ::1 → ::1          TCP 93 55450 → 6600 [PSH, ACK] Seq=1 Ack=1 Win=9206 Len=7 TSval=3494902416 TSecr=3494897415
    2 0.000405985          ::1 → ::1          TCP 350 6600 → 55450 [PSH, ACK] Seq=1 Ack=8 Win=512 Len=264 TSval=3494902417 TSecr=3494902416
    3 0.000412305          ::1 → ::1          TCP 86 55450 → 6600 [ACK] Seq=8 Ack=265 Win=9205 Len=0 TSval=3494902417 TSecr=3494902417
...
^Ctshark: 
10 packets captured

$ cat tshark.log
<same content as printed above, but without colors>
```

On next invocation, a new output file will be selected (`tshark-1.log`):

```
$ reclog tshark --color -i lo -f tcp
...

$ cat tshark-1.log
...
```

Explicitly specify output file name (but refuse to overwrite it):

```
$ reclog -o test.log tshark --color -i lo -f tcp
...

$ cat test.log
...
```

Overwrite exiting file:

```
$ reclog -f -o test.log tshark --color -i lo -f tcp
...
```

Append to exiting file:

```
$ reclog -a -o test.log tshark --color -i lo -f tcp
...
```

Enable header and timestamps. The very first line (`# HOST=...`) and timestamps in the beginning of every other line (like `21:39:28.437`) are injected to the output by reclog.

```
$ reclog -Ht tshark --color -i lo -f tcp
# HOST=[example] OS=[linux_x86_64] TIME=[2025-05-12 21:39:28 +0900] CMD=[tshark --color -i lo -f tcp]
21:39:28.437 Capturing on 'Loopback: lo'
21:39:28.548  ** (tshark:1504434) 21:39:28.583860 [Main MESSAGE] -- Capture started.
21:39:28.584  ** (tshark:1504434) 21:39:28.583896 [Main MESSAGE] -- File: "/tmp/wireshark_loCMPP62.pcapng"
21:39:28.584     1 0.000000000          ::1 → ::1          TCP 93 55450 → 6600 [PSH, ACK] Seq=1 Ack=1 Win=9206 Len=7 TSval=3495157416 TSecr=3495152415
21:39:31.112     2 0.000383212          ::1 → ::1          TCP 350 6600 → 55450 [PSH, ACK] Seq=1 Ack=8 Win=512 Len=264 TSval=3495157417 TSecr=3495157416
21:39:31.112     3 0.000388264          ::1 → ::1          TCP 86 55450 → 6600 [ACK] Seq=8 Ack=265 Win=9205 Len=0 TSval=3495157417 TSecr=3495157417
...
```

Pass something to command's stdin:

```
$ ls /usr/local | reclog tr '[:lower:]' '[:upper:]'
BIN
ETC
GAMES
INCLUDE
LIB
LIBEXEC
MAN
SBIN
SHARE
SRC

$ cat tr.log
...
```

History
-------

Changelog file can be found here: [CHANGES.md](CHANGES.md).

Authors
-------

See [AUTHORS.md](AUTHORS.md).

License
-------

[MIT](LICENSE)
