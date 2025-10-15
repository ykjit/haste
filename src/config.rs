//! The haste config file, using serde.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The number of process executions (repetitions using fresh processes).
    pub(crate) proc_execs: usize,
    /// The number of in-process iterations (iterations inside each process).
    pub(crate) inproc_iters: usize,
    /// The binaries to benchmark with.
    ///
    /// Each entry in the `HashMap` is a name mapping to the path to the binary.
    pub(crate) executors: HashMap<String, PathBuf>,
    /// The benchmark suites to use.
    pub(crate) suites: HashMap<String, Suite>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    /// The directory to change into for this suite.
    pub(crate) dir: PathBuf,
    /// The harness to use for this suite.
    ///
    /// The harness should accept arguments of the form:
    /// ```
    /// <harness> <benchmark-name> <inproc-iters> [<extra-arg0> ... <extra_argN>]
    /// ```
    pub(crate) harness: PathBuf,
    /// Extra environment to apply when running benchmarks in this suite (if any).
    #[serde(default)]
    pub(crate) env: HashMap<String, String>,
    /// Benchmarks in this suite.
    pub(crate) benchmarks: HashMap<String, Benchmark>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Benchmark {
    /// Extra arguments to pass to this benchmark (if any).
    #[serde(default)]
    pub(crate) extra_args: Vec<String>,
}
