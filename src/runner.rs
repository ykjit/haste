use crate::BenchKey;
use crate::{ResultFile, config::*};
use std::hint::black_box;
use std::path::Path;
use std::process::{self, Command, Stdio};

fn get_progress_percentage(config: &Config, completed_pexecs: usize) -> f64 {
    let mut total_pexecs = 0;
    for suite in &config.suites {
        total_pexecs += suite.1.benchmarks.len();
    }
    total_pexecs *= config.executors.len();
    total_pexecs *= config.proc_execs;

    let completed_pexecs = f64::from(u32::try_from(completed_pexecs).unwrap());
    let total_pexecs = f64::from(u32::try_from(total_pexecs).unwrap());
    completed_pexecs / total_pexecs * 100.
}

/// Run all benchmarks from the configuration.
pub(crate) fn run(config: &Config) -> ResultFile {
    let mut results = ResultFile::default();
    let mut completed_pexecs = 0;
    for (executor_name, executor) in &config.executors {
        for suite in &config.suites {
            run_suite(
                &mut results,
                config,
                &mut completed_pexecs,
                executor_name,
                executor,
                suite.1,
            );
        }
    }
    results
}

/// Run a suite with the specified executor.
fn run_suite(
    results: &mut ResultFile,
    config: &Config,
    completed_pexecs: &mut usize,
    executor_name: &str,
    executor: &Path,
    suite: &Suite,
) {
    for (bench_name, bench) in &suite.benchmarks {
        let key = BenchKey {
            benchmark: bench_name.into(),
            executor: executor_name.into(),
            extra_args: bench.extra_args.clone(),
        };
        for _ in 0..(config.proc_execs) {
            let progress = get_progress_percentage(config, *completed_pexecs);
            println!(">>> haste: ({progress:3.0}%) Running {key}");
            run_benchmark(
                results,
                config,
                executor_name,
                executor,
                suite,
                bench_name,
                bench,
            );
            *completed_pexecs += 1;
        }
    }
}

/// Run an individual benchmark.
fn run_benchmark(
    results: &mut ResultFile,
    config: &Config,
    executor_name: &str,
    executor: &Path,
    suite: &Suite,
    bench_name: &str,
    bench: &Benchmark,
) {
    let harness = suite.harness.to_str().unwrap();
    let inproc_iters = config.inproc_iters.to_string();
    let mut args = vec![harness, bench_name, &inproc_iters];
    args.extend(bench.extra_args.iter().map(String::as_str));

    let mut cmd = Command::new(executor);
    cmd.current_dir(&suite.dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in &suite.env {
        cmd.env(k, v);
    }
    cmd.args(&args);

    let t = std::time::Instant::now();
    // We are careful to use `output()` and not `spawn()` here so as to avoid deadlocks for
    // benchmarks that make a lot of output.
    let Ok(output) = black_box(cmd.output()) else {
        eprintln!("error: failed to spawn benchmark!");
        eprintln!("args: {cmd:?}");
        process::exit(1)
    };

    let elapsed = f64::from(u32::try_from(t.elapsed().as_millis()).unwrap());

    if !output.status.success() {
        eprintln!("error: benchmark command exited non-zero!");
        eprintln!("args: {cmd:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        eprintln!("--- Begin stdout ---");
        eprint!("{stdout}");
        eprintln!("--- End stdout ---");
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("--- Begin stderr ---");
        eprint!("{stderr}");
        eprintln!("--- End stderr ---");
        process::exit(1)
    }

    println!(">>> haste: {elapsed}ms");

    let bench_key = BenchKey {
        benchmark: bench_name.to_owned(),
        executor: executor_name.to_owned(),
        extra_args: bench.extra_args.to_owned(),
    };
    results
        .data
        .entry(bench_key.to_string())
        .or_default()
        .push(elapsed);
}
