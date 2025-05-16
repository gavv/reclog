=========
reclog(1)
=========
:Manual section: 1
:Manual group: User Commands
:Date: May 2025
:Footer: reclog 0.1.4

NAME
====

reclog - Command-line tool to capture command output to a file.

SYNOPSIS
========

**reclog** [*OPTIONS*] *COMMAND*...

DESCRIPTION
===========

**reclog** runs specified command in a pty, connecting its own stdin and stdout with pty's input and output, without blocking the command. In addition, it duplicates pty's output to a file, stripping out ANSI escape codes.

It is similar to **unbuffer(1)** combined with **tee(1)** and **ts(1)**, but provides better user experience and robustness.

OPTIONS
=======

**-H, --header**
    Before start, print header line (hostname, os, time, command).

    Useful when you collect logs from different machines or invocations and want to make them self-describing.

**-t, --ts**
    Prepend each line of the command output with current time.

    This option enables behavior similar to **ts(1)**. It is useful when original program output does not include time information.

    How timestamps are calculated and formatted is defined by **--ts-fmt** and **-ts-src** options.

**--ts-fmt** *FMT*
    If **--ts** is used, defines **strftime(3)** format string.

    Default format is **"%T%.3f"**, which produces timestamps like "01:02:03.123".

    Documentation for the format specifiers can be found on docs.rs page of Rust crate "chrono" (*https://docs.rs/chrono/latest/chrono/format/strftime/*).

**--ts-src** *SRC*
    If **--ts** is used, defines what timestamps to use: wallclock (*wall*), elapsed time since program start (*elapsed*), or delta between subsequent timestamps (*delta*).

    Default source is *wall*.

    *wall*, *elapsed*, and *delta* values are similar to *ts*, *ts -s*, and *ts -i* modes of **ts(1)** command, respectively.

**-o, --output** *PATH*
    Output file path.

    If omitted, output path is generated automatically based on the command basename (unless **--null** is given). E.g. for *`reclog ls -l'*, the output file is *ls.log*.

    Unless **--force** or **--append** option is given, output file should not exist, otherwise an error is reported.

    If **--output** is omitted and **--force** is not specified as well, and generated path already exist, a numeric suffix is automatically added to the path. E.g. if *ls.log* already exists, reclog will try *ls-1.log*, *ls-2.log*, and so on.

**-f, --force**
    Overwrite output file if it already exists.

    See description for **--output** option for details.

**-a, --append**
    Append to output file if it already exists.

    See description for **--output** option for details.

**-N, --null**
    Don't write output file at all.

    Has same effect as *--output=/dev/null*. The output is still printed to stdout, unless **--silent** is specified.

**-R, --raw**
    Don't strip ANSI escape codes when writing to output file.

    By default, reclog writes raw output to stdout and stripped output to the **--output** file. With this option, this stripping is disabled. This will preserve colors in the saved file, but makes it harder to grep.

    Stripping is performed via Rust crate "vte", a Rust implementation of Paul Williams' ANSI parser state machine (*https://docs.rs/vte/latest/vte/*).

**-s, --silent**
    Don't print anything to stdout.

    Has same effect as *`reclog ... > /dev/null'*. The output is still printed to file, unless **--null** is specified.

**-q, --quit** *MILLISECONDS*
    How long to wait for buffered data after getting EOF. When child process exits, reclog continues reading pending output from the pty until there is no data during the specified timeout. This allows to reliably fetch all buffered data before exiting.

    Also how long to wait for child to exit voluntarily until killing it forcibly. When emergency termination signal is received, like SIGQUIT, reclog forwards it to the child and waits until it exits or timeout expires. Then, if the child is still running, it forcibly kills the child with SIGKILL.

**-b, --buffer** *LINES*
    When stdout is slower than command output, buffer at max the specified number of lines.

    When command produces output faster than it can be written to reclog's stdout (typically if it is a terminal or pipe), reclog starts buffering lines until the specified limit is reached. When the buffer is full, the oldest lines are removed.

    This allows to ensure that the command is never slowed down by displaying logs, and hence even verbose logs don't affect testing.

    This option has no effect writing to **--output** file, only writing to reclog's stdout. Output file always receives the full output.

**-D, --debug**
    Enable debug logging to stderr.

**--man**
    Print man page in troff format to stdout and exit.

**-h, --help**
    Print help to stdout and exit.

**-V, --version**
    Print version information to stdout and exit.

STDIN / STDOUT
==============

reclog connects its own *stdin* with the pty input (from which the command reads), and connects its *stdout* with the pty output (to which the command writes).

Note that *stdout* gets interleaved output from command's stdout and stderr (they both are redirected to the same pty). Also note that stdout is rate-limited, as defined by **--buffer** option.

reclog reads from stdin and writes to stdout one line at a time, with a flush after each line. If stdin is a tty, it switches it to the *canonical mode*.

When reclog reads EOF from stdin, it propagates it to the child by sending **VEOF** character to the master pty, which triggers EOF condition on the slave pty after all pending input is read.

reclog does not exit after getting EOF from stdin or pty. It initiates termination only when the child process exits, even if it already finished I/O in both directions after getting EOFs.

SESSION
=======

The child process gets a separate SID (session ID) and PGID (process group ID). It becomes both a session leader and process group leader, and its SID and PGID are equal to its PID.

The slave pty is set as the controlling tty of the child process.

SID, PGID, and controlling tty are automatically inherited by grand-children (unless they explicitly detach from them). During termination, reclog (in cause of graceful termination) or kernel (if reclog aborts or crashes) send signals to the whole process group. This ensures that grand-children, if present, are properly cleaned up too.

SIGNALS
=======

All standard job control and termination signals are propagated to the child PGID: *SIGTERM*, *SIGINT*, *SIGHUP*, *SIGQUIT*, *SIGTSTP*, *SIGTTIN*, *SIGTTOU*, *SIGCONT*, *SIGWINCH*.

Handled signals can be divided into three categories:

- Graceful termination: Hit **^C** (or send *SIGINT* or *SIGTERM*) to terminate the child process gracefully and flush pending logs. Hit **^C** second time to forcibly kill the child (with *SIGKILL*) if it's stuck.

- Emergency termination: Hit **^\\** (or send *SIGQUIT* or *SIGHUP*) for emergency termination without flushing the logs. The child is given some short time to terminate properly, then is killed forcibly with *SIGKILL*.

- Pause/resume: Hit **^Z** (or send *SIGTSTP*, *SIGTTIN*, or *SIGTTOU*) to pause. Hit **^Z** second time to forcibly pause the child (with *SIGSTOP*) if it's stuck. Then type **fg** (or send *SIGCONT*) to resume.

When you close the terminal to which reclog is writing/reading (e.g. you close the terminal emulator window when reclog is running), kernel automatically generates *SIGHUP*. reclog propagates the signal to the child PGID and waits until child exits or **-q** timeout expires. If the child didn't exit, it is killed forcibly with *SIGKILL*.

If reclog crashes or aborts due to unexpected error, it does not attempt to perform graceful termination. However, once reclog is killed, kernel closes the master pty and sends *SIGHUP* to processes which use the slave pty. Unless child handles *SIGHUP* specially or explicitly changes controlling tty, it will be killed by this signal.

EXIT STATUS
===========

- If system error happens (like file can't be opened), reclog exits with status *1*.
- If usage error happens (like invalid option value), reclog exits with status *2*.
- If the specified command can't be launched, reclog exits with status *126*.
- If the command exits with status *N*, reclog exits with the same status *N*.
- If the command is killed by signal *N*, reclog exits with the status *128 + N*.

CAVEATS
=======

reclog makes several assumptions about the command it runs:

- The output and input should be textual and line-oriented, otherwise errors are possible.
- The command should be a non-interactive program that uses terminal in canonical mode, otherwise data corruption and freezes are possible.
- The command should keep its child processes (if any) in the same process group and with the same controlling TTY, otherwise they may remain dangling.

EXAMPLES
========

Specify output file:

.. code::

    $ reclog -o test.log ping -c3 8.8.8.8
    PING 8.8.8.8 (8.8.8.8) 56(84) bytes of data.
    64 bytes from 8.8.8.8: icmp_seq=1 ttl=111 time=24.9 ms
    64 bytes from 8.8.8.8: icmp_seq=2 ttl=111 time=24.5 ms
    64 bytes from 8.8.8.8: icmp_seq=3 ttl=111 time=34.3 ms

    --- 8.8.8.8 ping statistics ---
    3 packets transmitted, 3 received, 0% packet loss, time 2002ms
    rtt min/avg/max/mdev = 24.464/27.870/34.295/4.545 ms

    $ cat test.log
    ...

Overwrite file:

.. code::

    $ reclog -f -o test.log ping -c3 8.8.8.8
    ...

    $ cat test.log
    ...

Append to file:

.. code::

    $ reclog -a -o test.log ping -c3 8.8.8.8
    ...

    $ cat test.log
    ...

Automatic file name:

.. code::

    $ reclog ping -c3 8.8.8.8
    ...

    $ cat ping.log
    ...

    $ reclog ping -c3 8.8.8.8
    ...

    $ cat ping-1.log
    ...

Enable header and timestamps:

.. code::

    $ reclog -Ht ping -c3 8.8.8.8
    # HOST=[example] OS=[linux_x86_64] TIME=[2025-01-01 12:30:00 +0000] CMD=[ping -c3 8.8.8.8]
    12:30:00.022 PING 8.8.8.8 (8.8.8.8) 56(84) bytes of data.
    12:30:00.023 64 bytes from 8.8.8.8: icmp_seq=1 ttl=111 time=25.5 ms
    12:30:00.048 64 bytes from 8.8.8.8: icmp_seq=2 ttl=111 time=24.7 ms
    12:30:01.048 64 bytes from 8.8.8.8: icmp_seq=3 ttl=111 time=24.3 ms
    12:30:02.049
    12:30:02.049 --- 8.8.8.8 ping statistics ---
    12:30:02.049 3 packets transmitted, 3 received, 0% packet loss, time 2002ms
    12:30:02.049 rtt min/avg/max/mdev = 24.340/24.841/25.484/0.477 ms

Process stdin:

.. code::

    $ ls /usr/local | reclog cat -n
         1  bin
         2  etc
         3  games
         4  include
         5  lib
         6  libexec
         7  man
         8  sbin
         9  share
        10  src

REPORTING BUGS
==============

Please report any bugs found via GitHub (*https://github.com/gavv/reclog/*).

HISTORY
=======

See `CHANGES.md <CHANGES.md>`_ file for the release history.

AUTHORS
=======

See `AUTHORS.md <AUTHORS.md>`_ file for the list of authors and contributors.

COPYRIGHT
=========

2025, Victor Gaydov and contributors.

Licensed under MIT license, see `LICENSE <LICENSE>`_ file for details.

SEE ALSO
========

**unbuffer(1)**, **tee(1)**, **ts(1)**
