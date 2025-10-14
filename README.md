# Haste

`haste` is a benchmarking result differ.

## Basic workflow

 - Make a config file describing your benchmarks.
 - Record a baseline with `haste bench`. This runs the benchmarks and stashes the
   results away in a "datum" under `.haste` in `$CWD`. The ID of the datum is
   printed to stdout.
 - Make changes to whatever you are optimising, then run `haste bench` again to
   make a second datum.
 - Run `haste diff <id1> <id2>` to compare the datums.

## Config file

The config file is in TOML format, specified [here](src/config.rs).
