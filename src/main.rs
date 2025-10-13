use clap::{Parser, Subcommand};
use comfy_table::{Cell, Color, Table};
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::{
    collections::{HashMap, HashSet},
    env, fmt, fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process,
};

/// The `extra.toml` file for a datum
#[derive(Default, Serialize, Deserialize)]
struct ExtraToml {
    comment: Option<String>,
}

/// The name of the hidden directory we store state inside.
const DOT_DIR: &str = ".haste";

/// Uniquely identifies a benchmark.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct BenchKey {
    benchmark: String,
    executor: String,
    extra_args: String,
}

impl fmt::Display for BenchKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}/{}/{}",
            self.benchmark, self.executor, self.extra_args
        )
    }
}

#[allow(dead_code)]
struct BenchStats {
    mean: f64,
    std_dev: f64,
    min: f64,
    max: f64,
    median: f64,
    confidence_interval: (f64, f64),
    n_samples: usize,
}

/// Rebench data file parser.
#[derive(Debug)]
struct ResultFile {
    data: HashMap<BenchKey, Vec<Vec<f64>>>,
}

impl ResultFile {
    fn new(p: &Path) -> Self {
        let f = fs::File::open(p).unwrap_or_else(|_| {
            eprintln!("Couldn't open results file {}", p.to_str().unwrap());
            std::process::exit(1);
        });
        let rdr = BufReader::new(f);
        let mut first = true;
        let mut col_indices = HashMap::new();
        let mut data: HashMap<BenchKey, Vec<Vec<f64>>> = HashMap::new();

        let mut last_key = None;
        let mut last_invoc = 0;
        let mut last_iter = 0;

        for l in rdr.lines().map(|x| x.unwrap()) {
            let l = l.trim();
            if l.starts_with("#") {
                continue;
            }

            if first {
                // Cache the column headings.
                for (i, name) in l.split_whitespace().enumerate() {
                    col_indices.insert(name.to_string(), i);
                }
                first = false;
            } else {
                let row = l.split_whitespace().collect::<Vec<&str>>();

                // Skip non-total measurements (we only care about total time, not user/sys)
                if col_indices.contains_key("criterion") {
                    if row[col_indices["criterion"]] != "total" {
                        continue;
                    }
                }

                // extract the columns we care about.
                let benchmark = row[col_indices["benchmark"]].to_owned();
                let executor = row[col_indices["executor"]].to_owned();
                let extra_args = row[col_indices["extraArgs"]].to_owned();
                let invoc = row[col_indices["invocation"]].parse::<usize>().unwrap();
                let iter = row[col_indices["iteration"]].parse::<usize>().unwrap();
                let value = row[col_indices["value"]].parse::<f64>().unwrap();
                assert_eq!(row[col_indices["unit"]], "ms"); // expect miliseconds.

                let key = BenchKey {
                    benchmark,
                    executor,
                    extra_args,
                };

                // We assume the rows come in sequential invocation and iteration order.
                assert!(
                    invoc == last_invoc && iter == last_iter + 1
                        || invoc == last_invoc + 1 && iter == 1
                        || last_key.is_some()
                            && key != last_key.unwrap()
                            && invoc == 1
                            && iter == 1
                );

                if !data.contains_key(&key) {
                    data.insert(key.clone(), Vec::new());
                }
                let invocs = data.get_mut(&key).unwrap();
                if invocs.len() < invoc {
                    invocs.push(Vec::new());
                }
                assert_eq!(invocs.len(), invoc);
                let iters = &mut invocs[invoc - 1];
                iters.push(value);
                assert_eq!(iters.len(), iter);

                last_key = Some(key.to_owned());
                last_invoc = invoc;
                last_iter = iter;
            }
        }

        // Check all invocations contain the same number of iterations.
        for (k, invocs) in &data {
            let mut count = None;
            for invoc in invocs {
                if let Some(c) = count {
                    if c != invoc.len() {
                        eprintln!("error: not all invocations have the same number of iterations!");
                        eprintln!("  in file {} for benchmark {}", p.to_str().unwrap(), k);
                        process::exit(1);
                    }
                } else {
                    count = Some(invoc.len());
                }
            }
        }

        Self { data }
    }

    /// Check the results files have the same data dimensionality.
    ///
    /// Returns `Ok(())` iff the same set of benchmarks were run and the same number of invocations
    /// and iterations were run (on a per-benchmark basis).
    ///
    /// Each result file is assumed to be consistent in isolation.
    fn same_dims(&self, other: &ResultFile) -> Result<(), String> {
        let self_keys: HashSet<&BenchKey> = HashSet::from_iter(self.data.keys());
        let other_keys: HashSet<&BenchKey> = HashSet::from_iter(other.data.keys());
        if self_keys != other_keys {
            return Err("results files contain different benchmarks".into());
        }
        for (k, v1) in &self.data {
            let v2 = &other.data[k];
            if v1.len() != v2.len() {
                return Err(format!("different number of invocations for {k}"));
            }
            if v1[0].len() != v2[0].len() {
                return Err(format!("different number of iterations for {k}"));
            }
        }
        Ok(())
    }

    /// Produce summary statistics for each benchmark in the results file.
    ///
    /// Returns a HashMap mapping each benchmark key to an arithmetic mean wallclock time for one
    /// iteration.
    fn summarise(&self) -> HashMap<BenchKey, BenchStats> {
        let mut summary = HashMap::new();
        for (k, invocs) in &self.data {
            let mut invoc_means: Vec<f64> = Vec::new();
            for invoc in invocs {
                invoc_means.push(invoc.iter().sum::<f64>() / invoc.len() as f64);
            }

            let n = invoc_means.len();
            let mean = invoc_means.iter().sum::<f64>() / n as f64;

            // Calculate std dev of invocation means
            let variance = invoc_means.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
            let std_dev = variance.sqrt();

            // Calculate min and max
            let min = invoc_means.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = invoc_means
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max);

            // Calculate median
            let mut sorted = invoc_means.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = if n % 2 == 0 {
                (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
            } else {
                sorted[n / 2]
            };

            // Total samples (all measurements)
            let n_samples = invocs.iter().map(|inv| inv.len()).sum();

            // For confidence interval, use t-distribution approximation
            let t_value = if n >= 30 { 1.96 } else { 2.0 };
            let margin = t_value * std_dev / (n as f64).sqrt();
            let confidence_interval = (mean - margin, mean + margin);

            summary.insert(
                k.to_owned(),
                BenchStats {
                    mean,
                    std_dev,
                    min,
                    max,
                    median,
                    confidence_interval,
                    n_samples,
                },
            );
        }
        summary
    }

    /// Produce flat-mean summary statistics for each benchmark.
    ///
    /// This flattens all measurements (across invocations and iterations)
    /// and calculates comprehensive statistics, similar to multitime's approach.
    fn summarise_flat(&self) -> HashMap<BenchKey, BenchStats> {
        let mut summary = HashMap::new();
        for (k, invocs) in &self.data {
            // Flatten all measurements
            let mut all_values: Vec<f64> = Vec::new();
            for invoc in invocs {
                all_values.extend(invoc.iter().cloned());
            }

            let n = all_values.len();
            if n == 0 {
                continue;
            }

            // Calculate mean
            let mean = all_values.iter().sum::<f64>() / n as f64;

            // Calculate standard deviation
            let variance = all_values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
            let std_dev = variance.sqrt();

            // Calculate min and max
            let min = all_values.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = all_values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

            // Calculate median
            let mut sorted = all_values.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = if n % 2 == 0 {
                (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
            } else {
                sorted[n / 2]
            };

            // Calculate 95% confidence interval using t-distribution approximation
            // For large samples, t-distribution approaches normal distribution
            let t_value = if n >= 30 {
                1.96
            } else {
                // Approximate t-values for small samples at 95% confidence
                match n {
                    1 => 12.706,
                    2 => 4.303,
                    3 => 3.182,
                    4 => 2.776,
                    5 => 2.571,
                    6 => 2.447,
                    7 => 2.365,
                    8 => 2.306,
                    9 => 2.262,
                    10 => 2.228,
                    _ if n < 20 => 2.1, // Approximate for 11-19
                    _ => 2.0,           // Approximate for 20-29
                }
            };
            let margin = t_value * std_dev / (n as f64).sqrt();
            let confidence_interval = (mean - margin, mean + margin);

            summary.insert(
                k.to_owned(),
                BenchStats {
                    mean,
                    std_dev,
                    min,
                    max,
                    median,
                    confidence_interval,
                    n_samples: n,
                },
            );
        }
        summary
    }
}

struct App {
    /// The directory where persistent state is stored.
    state_dir: PathBuf,
}

impl App {
    fn new() -> Self {
        let state_dir = [env::current_dir().unwrap().to_str().unwrap(), DOT_DIR]
            .iter()
            .collect();
        if !fs::exists(&state_dir).unwrap() {
            fs::create_dir(&state_dir).unwrap();
        }
        Self { state_dir }
    }

    /// Determine the next available datum ID.
    ///
    /// The first ID issued is zero.
    fn next_id(&self) -> usize {
        let mut max: isize = -1;
        for d in fs::read_dir(&self.state_dir).unwrap() {
            let num = d
                .unwrap()
                .path()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .parse::<isize>()
                .unwrap();
            if num > max {
                max = num;
            }
        }
        usize::try_from(max + 1).unwrap()
    }

    /// Store a new datum and return the ID.
    fn store_datum(&self, comment: Option<String>) -> usize {
        let id = self.next_id();
        let datum_dir = self.get_datum_dir(id);
        fs::create_dir(&datum_dir).unwrap();
        let copy_to = self.get_datum_results_path(id);
        fs::rename("benchmark.data", copy_to).unwrap();

        // Write out the extra metadata.
        let extra_data = toml::to_string(&ExtraToml { comment }).unwrap();
        let extra_path = self.get_datum_extra_path(id);
        std::fs::write(extra_path, extra_data).unwrap();

        id
    }

    fn get_datum_dir(&self, id: usize) -> PathBuf {
        [DOT_DIR, &id.to_string()].iter().collect::<PathBuf>()
    }

    fn get_datum_results_path(&self, id: usize) -> PathBuf {
        let mut p = self.get_datum_dir(id);
        p.push("benchmark.data");
        p
    }

    fn get_datum_extra_path(&self, id: usize) -> PathBuf {
        let mut p = self.get_datum_dir(id);
        p.push("extra.toml");
        p
    }

    fn load_extra(&self, id: usize) -> ExtraToml {
        let path = self.get_datum_extra_path(id);
        if let Ok(data) = std::fs::read_to_string(path) {
            toml::from_str(&data).unwrap()
        } else {
            ExtraToml::default()
        }
    }

    /// Run benchmarks and store the results as a new datum.
    ///
    /// If successful, the new datum is printed to stdout.
    /// Build a comparison row from two benchmark statistics.
    fn build_comparison_row(
        &self,
        label: String,
        stats1: &BenchStats,
        stats2: &BenchStats,
    ) -> (f64, Vec<Cell>) {
        let mut row = Vec::new();
        let ratio = stats2.mean / stats1.mean;
        let change = (ratio - 1.0) * 100.0;
        let abs_change = change.abs();

        row.push(Cell::new(label));
        row.push(Cell::new(format!(
            "{:.0}±{:.1}",
            stats1.mean, stats1.std_dev
        )));
        row.push(Cell::new(format!(
            "{:.0}±{:.1}",
            stats2.mean, stats2.std_dev
        )));
        row.push(Cell::new(format!("{ratio:.2}")));
        let change_cell = if change < 0.0 {
            Cell::new(format!("{abs_change:.2}% faster")).fg(Color::Green)
        } else {
            Cell::new(format!("{abs_change:.2}% slower")).fg(Color::Red)
        };
        row.push(change_cell);
        (change, row)
    }

    /// Render and print a comparison table.
    fn render_comparison_table(
        &self,
        mut rows: Vec<(f64, Vec<Cell>)>,
        col1_label: &str,
        col2_label: &str,
        title: String,
    ) {
        let mut table = Table::new();
        table.load_preset(comfy_table::presets::NOTHING);
        table.set_header(vec![
            "Benchmark",
            &format!("{col1_label} (ms)"),
            &format!("{col2_label} (ms)"),
            "Ratio",
            "Summary",
        ]);
        // Sort by speedup, descending. Handle NaN values by treating them as equal.
        rows.sort_by(|(c1, _), (c2, _)| c1.partial_cmp(c2).unwrap_or(std::cmp::Ordering::Equal));
        for (_, row) in rows {
            table.add_row(row);
        }

        println!("{title}");
        println!("{table}");
    }

    fn cmd_bench(&self, comment: Option<String>) {
        if Command::new("rebench").arg("--version").output().is_err() {
            eprintln!(
                "error: `rebench` binary not found or not executable. Please ensure it is installed and in your PATH."
            );
            process::exit(1);
        }

        let mut cmd = Command::new("rebench");
        cmd.args(["-c", "--no-denoise", "rebench.conf"]);

        let Ok(mut child) = cmd.spawn() else {
            eprintln!("error: failed to spawn benchmarks!");
            eprintln!("args: {cmd:?}");
            process::exit(1)
        };

        let status = child.wait().unwrap();
        if !status.success() {
            eprintln!("error: benchmark command exited non-zero!");
            process::exit(1)
        }
        let id = self.store_datum(comment.to_owned());

        let comment_s = if let Some(c) = comment {
            &format!("[{c}]")
        } else {
            ""
        };
        println!("haste: created datum {id} {comment_s}");
    }

    fn cmd_diff(&self, id1: usize, id2: usize, flat_mean: bool) {
        let data1 = ResultFile::new(&self.get_datum_results_path(id1));
        let data2 = ResultFile::new(&self.get_datum_results_path(id2));

        if let Err(e) = data1.same_dims(&data2) {
            eprintln!("{e}");
            process::exit(1);
        }

        let mut rows = Vec::new();

        if flat_mean {
            let summary1 = data1.summarise_flat();
            let summary2 = data2.summarise_flat();

            for (k, stats1) in &summary1 {
                let stats2 = &summary2[k];
                rows.push(self.build_comparison_row(k.to_string(), stats1, stats2));
            }
        } else {
            let summary1 = data1.summarise();
            let summary2 = data2.summarise();

            for (k, stats1) in &summary1 {
                let stats2 = &summary2[k];
                rows.push(self.build_comparison_row(k.to_string(), stats1, stats2));
            }
        }

        // If there's any extra metadata, print it.
        let extra1 = self.load_extra(id1);
        let extra2 = self.load_extra(id2);
        let mut title = String::new();
        if extra1.comment.is_some() || extra2.comment.is_some() {
            let no_comment = "(no comment)".to_owned();
            title.push_str(&format!(
                "Datum{id1}: {}\n",
                extra1.comment.unwrap_or(no_comment.clone())
            ));
            title.push_str(&format!(
                "Datum{id2}: {}\n\n",
                extra2.comment.unwrap_or(no_comment)
            ));
        }

        self.render_comparison_table(rows, &format!("Datum{id1}"), &format!("Datum{id2}"), title);
    }

    fn cmd_list(&self) {
        let mut ids = Vec::new();
        for ent in fs::read_dir(&self.state_dir).unwrap() {
            let ent = ent.unwrap();
            if let Ok(id) = ent
                .path()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .parse::<usize>()
            {
                ids.push(id);
            }
        }
        ids.sort();
        for id in ids {
            let extra = self.load_extra(id);
            println!("{id:3}: {}", extra.comment.unwrap_or("".into()));
        }
    }

    fn cmd_diff_exec(&self, id: usize, exec1: &str, exec2: &str, flat_mean: bool) {
        let data = ResultFile::new(&self.get_datum_results_path(id));

        let summary = if flat_mean {
            data.summarise_flat()
        } else {
            data.summarise()
        };

        // Find benchmarks that have both executors and track extra_args
        let mut bench_pairs: HashMap<String, (Option<BenchKey>, Option<BenchKey>)> = HashMap::new();
        let mut found_exec1 = false;
        let mut found_exec2 = false;

        for k in summary.keys() {
            if k.executor == exec1 {
                found_exec1 = true;
                bench_pairs
                    .entry(k.benchmark.clone())
                    .or_insert((None, None))
                    .0 = Some(k.clone());
            }
            if k.executor == exec2 {
                found_exec2 = true;
                bench_pairs
                    .entry(k.benchmark.clone())
                    .or_insert((None, None))
                    .1 = Some(k.clone());
            }
        }

        if !found_exec1 {
            eprintln!("error: executor '{}' not found in datum {}", exec1, id);
            process::exit(1);
        }
        if !found_exec2 {
            eprintln!("error: executor '{}' not found in datum {}", exec2, id);
            process::exit(1);
        }

        let mut rows = Vec::new();
        for (bench_name, (key1_opt, key2_opt)) in bench_pairs {
            if let (Some(key1), Some(key2)) = (key1_opt, key2_opt) {
                if let (Some(stats1), Some(stats2)) = (summary.get(&key1), summary.get(&key2)) {
                    // Format as benchmark/extra_args (without executor since we're comparing executors)
                    let bench_display = if key1.extra_args.is_empty() {
                        bench_name.clone()
                    } else {
                        format!("{}/{}", bench_name, key1.extra_args)
                    };

                    rows.push(self.build_comparison_row(bench_display, stats1, stats2));
                }
            }
        }

        // Show metadata
        let extra = self.load_extra(id);
        let mut title = String::new();
        if let Some(comment) = extra.comment {
            title.push_str(&format!("Datum{id}: {comment}\n\n"));
        }
        title.push_str(&format!(
            "Comparing executors within datum {id}: {exec1} vs {exec2}\n\n"
        ));

        self.render_comparison_table(rows, exec1, exec2, title);
    }
}

#[derive(Parser)]
#[command(version, about, subcommand_required = true)]
struct Cli {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand, Debug)]
enum Mode {
    /// Run rebench and store the results into a new datum.
    /// The rebench config `$PWD/rebench.conf` is used.
    #[clap(visible_alias = "b")]
    Bench {
        /// Attach a comment to the datum.
        #[clap(short, long, num_args(1))]
        comment: Option<String>,
    },
    /// Compare two datums.
    #[clap(visible_alias = "d")]
    Diff {
        id1: usize,
        id2: usize,
        /// Use flat averaging across all measurements (like multitime)
        #[clap(short, long)]
        flat_mean: bool,
    },
    /// List datums.
    #[clap(visible_alias = "l")]
    List,
    /// Compare different executors within the same datum.
    #[clap(visible_alias = "de")]
    DiffExec {
        id: usize,
        executor1: String,
        executor2: String,
        /// Use flat averaging across all measurements (like multitime)
        #[clap(short, long)]
        flat_mean: bool,
    },
}

fn main() {
    let app = App::new();
    let cli = Cli::parse();
    match cli.mode {
        Mode::Bench { comment } => app.cmd_bench(comment),
        Mode::Diff {
            id1,
            id2,
            flat_mean,
        } => app.cmd_diff(id1, id2, flat_mean),
        Mode::List => app.cmd_list(),
        Mode::DiffExec {
            id,
            executor1,
            executor2,
            flat_mean,
        } => app.cmd_diff_exec(id, &executor1, &executor2, flat_mean),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper function to create a test ResultFile from raw data
    fn create_test_result_file(data: HashMap<BenchKey, Vec<Vec<f64>>>) -> ResultFile {
        ResultFile { data }
    }

    #[test]
    fn test_summarise() {
        // Test summarise method
        let mut data = HashMap::new();
        let key = BenchKey {
            benchmark: "bench1".to_string(),
            executor: "exec1".to_string(),
            extra_args: "args1".to_string(),
        };
        // Create data with 2 invocations, 3 iterations each
        data.insert(
            key.clone(),
            vec![
                vec![100.0, 110.0, 120.0], // Invocation 1: mean = (100 + 110 + 120) / 3 = 110
                vec![200.0, 210.0, 220.0], // Invocation 2: mean = (200 + 210 + 220) / 3 = 210
            ],
        );

        let result_file = create_test_result_file(data);

        // Summarise: mean of means = (110 + 210) / 2 = 160
        let summary = result_file.summarise();
        assert!((summary[&key].mean - 160.0).abs() < 1e-10);
    }

    #[test]
    fn test_summarise_flat() {
        // Test flat-mean averaging across all measurements
        let mut data = HashMap::new();
        let key = BenchKey {
            benchmark: "bench1".to_string(),
            executor: "exec1".to_string(),
            extra_args: "args1".to_string(),
        };
        // Create data with 2 invocations, different iteration counts
        data.insert(
            key.clone(),
            vec![
                vec![1.0, 2.0, 3.0, 4.0], // Invocation 1: 4 iterations
                vec![2.0, 2.0, 2.0, 2.0], // Invocation 2: 4 iterations
            ],
        );

        let result_file = create_test_result_file(data);

        // Flat-mean summarise: flat average = (1+2+3+4+2+2+2+2) / 8 = 18 / 8 = 2.25
        let summary = result_file.summarise_flat();
        let stats = &summary[&key];

        // Check mean
        assert!(
            (stats.mean - 2.25).abs() < 1e-10,
            "Mean should be 2.25, got {}",
            stats.mean
        );

        // Check standard deviation
        // Values: 1, 2, 3, 4, 2, 2, 2, 2
        // Mean: 2.25
        // Variance: ((1-2.25)^2 + (2-2.25)^2 + (3-2.25)^2 + (4-2.25)^2 + 4*(2-2.25)^2) / 8
        //         = (1.5625 + 0.0625 + 0.5625 + 3.0625 + 4*0.0625) / 8
        //         = (1.5625 + 0.0625 + 0.5625 + 3.0625 + 0.25) / 8
        //         = 5.5 / 8 = 0.6875
        // StdDev: sqrt(0.6875) ≈ 0.8292
        assert!(
            (stats.std_dev - 0.8292).abs() < 0.001,
            "StdDev should be ~0.8292, got {}",
            stats.std_dev
        );

        // Check min and max
        assert_eq!(stats.min, 1.0, "Min should be 1.0");
        assert_eq!(stats.max, 4.0, "Max should be 4.0");

        // Check median - sorted: [1, 2, 2, 2, 2, 2, 3, 4]
        // With 8 elements (even), median is average of 4th and 5th elements: (2 + 2) / 2 = 2
        assert_eq!(stats.median, 2.0, "Median should be 2.0");

        // Check sample count
        assert_eq!(stats.n_samples, 8, "Should have 8 samples");
    }

    #[test]
    fn test_summarise_vs_flat_difference() {
        // Test that the two summarization methods produce different standard deviations
        let mut data = HashMap::new();
        let key = BenchKey {
            benchmark: "bench1".to_string(),
            executor: "exec1".to_string(),
            extra_args: "args1".to_string(),
        };

        data.insert(
            key.clone(),
            vec![
                vec![10.0, 20.0],                   // Invocation 1: 2 iterations, mean = 15
                vec![30.0, 40.0, 50.0, 60.0, 70.0], // Invocation 2: 5 iterations, mean = 50
            ],
        );

        let result_file = create_test_result_file(data);

        // Test mean-of-means (summarise)
        let summary = result_file.summarise();
        let stats = &summary[&key];

        // Test flat average (summarise_flat)
        let stats_flat = &result_file.summarise_flat()[&key];

        // Mean-of-means: (15 + 50) / 2 = 32.5
        // Flat average: (10+20+30+40+50+60+70) / 7 = 280 / 7 = 40.0
        // These are different!
        assert!(
            stats.mean != stats_flat.mean,
            "The two methods should produce different means: {} vs {}",
            stats.mean,
            stats_flat.mean
        );
    }
}
