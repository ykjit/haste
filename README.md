# Haste

`haste` is a benchmarking result differ for suites using
[rebench](https://github.com/smarr/rebench).

## Basic workflow

 - Record a baseline with `haste bench`. This runs `rebench` and stashes the
   results away in a "datum" under `.haste` in `$CWD`. The ID of the datum is
   printed to stdout.
 - Make changes to whatever you are optimising, then run `haste bench` again to
   make a second datum.
 - Run `haste diff <id1> <id2>` to compare the datums.
