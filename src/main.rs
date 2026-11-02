use clap::{Parser, Subcommand, ValueEnum};
use comfy_table::{Cell, CellAlignment, Color, Table};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    env, fmt, fs,
    path::PathBuf,
    process,
};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

mod config;
mod runner;

/// The order in which benchmarks are executed.
#[derive(Copy, Clone, Debug, Default, PartialEq, ValueEnum)]
pub(crate) enum ExecutionOrder {
    /// Run benchmarks in the order they are declared in the config file.
    #[default]
    Declaration,
    /// Run benchmarks in a randomised order.
    Randomised,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, ValueEnum)]
enum ConfidenceLevel {
    #[value(name = "90")]
    CL90,
    #[value(name = "95")]
    CL95,
    #[default]
    #[value(name = "99")]
    CL99,
}

impl ConfidenceLevel {
    fn zval(self) -> f64 {
        match self {
            Self::CL90 => 1.645,
            Self::CL95 => 1.96,
            Self::CL99 => 2.576,
        }
    }

    fn as_percent(self) -> u8 {
        match self {
            Self::CL90 => 90,
            Self::CL95 => 95,
            Self::CL99 => 99,
        }
    }
}

/// The `extra.toml` file for a datum
#[derive(Serialize, Deserialize)]
struct ExtraToml {
    comment: Option<String>,
    executors: IndexMap<String, PathBuf>,
    suite_env: IndexMap<String, HashMap<String, String>>,
    machine: Option<IndexMap<String, String>>,
}

fn print_extra(id: usize, extra: &ExtraToml) {
    let no_comment = "(no comment)".to_owned();
    println!("Datum{id}: {}", extra.comment.clone().unwrap_or(no_comment));
    for (name, path) in &extra.executors {
        println!("  executor {name} = {}", path.display());
    }
    for (suite, env) in &extra.suite_env {
        if env.is_empty() {
            continue;
        }
        let mut entries: Vec<(&String, &String)> = env.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (k, v) in entries {
            println!("  suite {suite} env {k}={v}");
        }
    }
}

fn machine_info() -> IndexMap<String, String> {
    let refresh = RefreshKind::nothing()
        .with_cpu(CpuRefreshKind::nothing())
        .with_memory(MemoryRefreshKind::nothing().with_ram());
    let sys = System::new_with_specifics(refresh).ok();
    let mut info = IndexMap::new();
    info.insert("os", System::long_os_version().ok());
    info.insert(
        "cpu",
        sys.as_ref()
            .and_then(|s| s.cpus().first())
            .map(|c| c.brand().to_owned())
            .filter(|b| !b.is_empty()),
    );
    info.insert(
        "memory",
        sys.as_ref().map(|s| {
            let mib = u32::try_from(s.total_memory() / 1024 / 1024).unwrap();
            format!("{:.1} GiB", f64::from(mib) / 1024.)
        }),
    );
    info.insert("arch", Some(System::cpu_arch()));
    info.insert(
        "cpu_count",
        sys.as_ref().map(|s| s.cpus().len().to_string()),
    );
    info.into_iter()
        .filter_map(|(k, v)| Some((k.to_owned(), v?)))
        .collect()
}

/// The name of the hidden directory we store state inside.
const DOT_DIR: &str = ".haste";
/// The name of the haste config file.
const DEFAULT_CONFIG_FILE: &str = "haste.toml";

/// Uniquely identifies a benchmark.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
struct BenchKey {
    benchmark: String,
    executor: String,
    extra_args: Vec<String>,
}

impl fmt::Display for BenchKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}/{}/{}",
            self.benchmark,
            self.executor,
            self.extra_args.join("-")
        )
    }
}

struct SummaryStats {
    /// Sample arithmentic mean.
    mean: f64,
    /// The `CI_CONF`% confidence interval.
    ///
    /// We report the mean +/- this value.
    ci: f64,
}

impl SummaryStats {
    fn new(mean: f64, ci: f64) -> Self {
        Self { mean, ci }
    }

    /// Determine if two confidence intervals overlap.
    fn ci_overlaps(&self, other: &Self) -> bool {
        let l1 = self.mean - self.ci;
        let u1 = self.mean + self.ci;
        let l2 = other.mean - other.ci;
        let u2 = other.mean + other.ci;
        l1 <= u2 && l2 <= u1
    }
}

/// Computes a consistent width for fomatting floats in a colum so they all line up nicely.
fn compute_f64_format(fs: &[f64]) -> usize {
    let mut max_width = 1;
    for f in fs {
        let s = format!("{:.0}", f);
        if s.len() > max_width {
            max_width = s.len();
        }
    }
    max_width
}

/// The results file for a datum.
#[derive(Serialize, Deserialize, Debug, Default)]
struct ResultFile {
    // String benchmark key -> collection of process execution times (in milliseconds).
    data: HashMap<String, Vec<f64>>,
}

impl ResultFile {
    fn summarise(&self, confidence: ConfidenceLevel) -> HashMap<String, SummaryStats> {
        let mut summaries = HashMap::new();
        for (k, invocs) in &self.data {
            let n = f64::from(u32::try_from(invocs.len()).unwrap());
            let mean = invocs.iter().sum::<f64>() / n;

            // Compute a confidence interval, as per:
            // https://www.dummies.com/article/academics-the-arts/math/statistics/how-to-calculate-a-confidence-interval-for-a-population-mean-when-you-know-its-standard-deviation-169722/
            let ci = if invocs.len() > 1 {
                let variance = invocs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.);
                let std_dev = variance.sqrt();
                confidence.zval() * std_dev / n.sqrt()
            } else {
                // Avoid division by zero in case there is a single sample.
                // In this case, report a CI of +/- 0.
                0.
            };

            let summary = SummaryStats::new(mean, ci);
            summaries.insert(k.to_owned(), summary);
        }
        summaries
    }

    /// Check the results have the same data dimensionality.
    ///
    /// Returns `Ok(())` iff the same set of benchmarks were run and the same number of invocations
    /// and iterations were run (on a per-benchmark basis).
    ///
    /// Each set of results is assumed to be consistent in isolation.
    fn same_dims(&self, other: &ResultFile) -> Result<(), String> {
        let self_keys: HashSet<&String> = HashSet::from_iter(self.data.keys());
        let other_keys: HashSet<&String> = HashSet::from_iter(other.data.keys());
        if self_keys != other_keys {
            let mut lhs_diff = self_keys
                .difference(&other_keys)
                .cloned()
                .map(|x| x.to_owned())
                .collect::<Vec<_>>();
            lhs_diff.sort();
            let mut rhs_diff = other_keys
                .difference(&self_keys)
                .cloned()
                .map(|x| x.to_owned())
                .collect::<Vec<_>>();
            rhs_diff.sort();
            return Err(format!(
                "results files contain different benchmarks:\n  lhs lacks: {}\n  rhs lacks: {}",
                lhs_diff.join(", "),
                rhs_diff.join(", ")
            ));
        }
        for (k, v1) in &self.data {
            let v2 = &other.data[k];
            if v1.len() != v2.len() {
                return Err(format!("different number of process executions for {k}"));
            }
        }
        Ok(())
    }
}

/// Return a copy of `config` restricted to the benchmarks named in the comma-separated `filter`.
fn filter_benchmarks(mut config: config::Config, filter: &str) -> config::Config {
    let names: HashSet<&str> = filter.split(',').map(str::trim).collect();
    for suite in config.suites.values_mut() {
        suite
            .benchmarks
            .retain(|name, _| names.contains(name.as_str()));
    }
    config
}

struct App {
    /// The directory where persistent state is stored.
    state_dir: PathBuf,
    /// The path to the config file.
    config_file: PathBuf,
}

impl App {
    fn new(config_file: Option<PathBuf>) -> Self {
        let state_dir = [env::current_dir().unwrap().to_str().unwrap(), DOT_DIR]
            .iter()
            .collect();
        if !fs::exists(&state_dir).unwrap() {
            fs::create_dir(&state_dir).unwrap();
        }
        let config_file = config_file.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_FILE));
        Self {
            state_dir,
            config_file,
        }
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
    fn store_datum(
        &self,
        results: ResultFile,
        comment: Option<String>,
        executors: IndexMap<String, PathBuf>,
        suite_env: IndexMap<String, HashMap<String, String>>,
        machine: IndexMap<String, String>,
    ) -> usize {
        let id = self.next_id();
        let datum_dir = self.get_datum_dir(id);
        fs::create_dir(&datum_dir).unwrap();
        let res_path = self.get_datum_results_path(id);
        let tml = toml::to_string(&results).unwrap();
        fs::write(res_path, tml).unwrap();

        // Write out the extra metadata.
        // FIXME: consider merging this into the main toml file.
        let extra_data = toml::to_string(&ExtraToml {
            comment,
            executors,
            suite_env,
            machine: Some(machine),
        })
        .unwrap();
        let extra_path = self.get_datum_extra_path(id);
        std::fs::write(extra_path, extra_data).unwrap();

        id
    }

    fn get_datum_dir(&self, id: usize) -> PathBuf {
        [DOT_DIR, &id.to_string()].iter().collect::<PathBuf>()
    }

    fn get_datum_results_path(&self, id: usize) -> PathBuf {
        let mut p = self.get_datum_dir(id);
        p.push("data.toml");
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
            ExtraToml {
                comment: None,
                executors: IndexMap::new(),
                suite_env: IndexMap::new(),
                machine: None,
            }
        }
    }

    /// Run benchmarks and store the results as a new datum.
    ///
    /// If successful, the new datum is printed to stdout.
    fn cmd_bench(&self, comment: Option<String>, order: ExecutionOrder, filter: Option<String>) {
        let config_path = self.config_file.display();
        let config_text = fs::read_to_string(&self.config_file).unwrap_or_else(|e| {
            eprintln!("error: failed to read {config_path}: {e}");
            process::exit(1);
        });
        let config: config::Config = match toml::from_str(&config_text) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Unable to parse {config_path}: {e}");
                std::process::exit(1);
            }
        };
        let config = match &filter {
            Some(filter) => filter_benchmarks(config, filter),
            None => config,
        };
        let results = runner::run(&config, order);
        let suite_env = config
            .suites
            .iter()
            .map(|(name, suite)| (name.clone(), suite.env.clone()))
            .collect();
        let id = self.store_datum(
            results,
            comment.to_owned(),
            config.executors.clone(),
            suite_env,
            machine_info(),
        );
        let comment_s = comment.unwrap_or("".to_owned());
        println!("haste: created datum {id} {comment_s}");
    }

    fn cmd_diff(&self, id1: usize, id2: usize, confidence: ConfidenceLevel) {
        let tml1 = fs::read_to_string(self.get_datum_results_path(id1)).unwrap();
        let tml2 = fs::read_to_string(self.get_datum_results_path(id2)).unwrap();
        let data1 = toml::from_str::<ResultFile>(&tml1).unwrap();
        let data2 = toml::from_str::<ResultFile>(&tml2).unwrap();

        if let Err(e) = data1.same_dims(&data2) {
            eprintln!("{e}");
            process::exit(1);
        }

        let data1 = data1.summarise(confidence);
        let data2 = data2.summarise(confidence);

        // Compute the formatting of our data.
        let means = data1
            .iter()
            .chain(&data2)
            .map(|(_, s)| s.mean)
            .collect::<Vec<f64>>();
        let mean_width = compute_f64_format(&means);
        let cis = data1
            .iter()
            .chain(&data2)
            .map(|(_, s)| s.ci)
            .collect::<Vec<f64>>();
        let ci_width = compute_f64_format(&cis);
        let mut ratios = Vec::new();
        for (key, s1) in data1.iter() {
            let s2 = &data2[key];
            ratios.push(s2.mean / s1.mean);
        }
        let ratio_width = compute_f64_format(&ratios) + 3;

        let mut sig_rows = Vec::new();
        let mut insig_rows = Vec::new();
        for (k, v1) in &data1 {
            let mut row = Vec::new();
            let v2 = &data2[k];
            let ratio = v2.mean / v1.mean;
            let change = (ratio - 1.0) * 100.0;
            let abs_change = change.abs();

            row.push(Cell::new(k));
            let v1_cell = Cell::new(format!("{:mean_width$.0} ±{:ci_width$.0}", v1.mean, v1.ci));
            row.push(v1_cell.set_alignment(CellAlignment::Right));
            let v2_cell = Cell::new(format!("{:mean_width$.0} ±{:ci_width$.0}", v2.mean, v2.ci));
            row.push(v2_cell.set_alignment(CellAlignment::Right));
            let ratio_cell = Cell::new(format!("{ratio:>ratio_width$.2}"));
            row.push(ratio_cell.set_alignment(CellAlignment::Right));

            if !v1.ci_overlaps(v2) {
                let change_cell = if change < 0.0 {
                    Cell::new(format!("{abs_change:.2}% faster")).fg(Color::Green)
                } else {
                    Cell::new(format!("{abs_change:.2}% slower")).fg(Color::Red)
                };
                row.push(change_cell);
                sig_rows.push((change, row));
            } else {
                row.push(Cell::new("indistinguishable".to_owned()).fg(Color::Magenta));
                insig_rows.push((change, row));
            }
        }

        let mut table = Table::new();
        table.load_preset(comfy_table::presets::NOTHING);
        table.set_header(vec![
            Cell::new("Benchmark").set_alignment(CellAlignment::Left),
            Cell::new(format!("Datum{id1} (ms)")).set_alignment(CellAlignment::Right),
            Cell::new(format!("Datum{id2} (ms)")).set_alignment(CellAlignment::Right),
            Cell::new("Ratio").set_alignment(CellAlignment::Right),
            Cell::new("Summary").set_alignment(CellAlignment::Left),
        ]);
        // Sort the rows first by significance, then by speedup, descending.
        sig_rows.sort_by(|(c1, _), (c2, _)| c1.partial_cmp(c2).unwrap());
        for (_, row) in sig_rows {
            table.add_row(row);
        }
        // Insignifcant results: sort by speedup, descending.
        insig_rows.sort_by(|(c1, _), (c2, _)| c1.partial_cmp(c2).unwrap());
        for (_, row) in insig_rows {
            table.add_row(row);
        }

        print_extra(id1, &self.load_extra(id1));
        print_extra(id2, &self.load_extra(id2));
        println!();

        println!("confidence level: {}%\n", confidence.as_percent());
        println!("{table}");
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
}

#[derive(Parser)]
#[command(version, about, subcommand_required = true)]
struct Cli {
    /// Path to the haste configuration file (defaults to haste.toml).
    #[arg(short = 'f', long, global = true, value_name = "FILE")]
    file: Option<PathBuf>,

    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand, Debug)]
enum Mode {
    /// Run benchmarks and store the results into a new datum.
    #[clap(visible_alias = "b")]
    Bench {
        /// Attach a comment to the datum.
        #[clap(short, long, num_args(1))]
        comment: Option<String>,
        /// Order in which benchmarks are run.
        #[arg(short, long, value_enum, default_value_t = ExecutionOrder::default())]
        order: ExecutionOrder,
        /// Only run benchmarks with these names (comma-separated, e.g. --filter=richards,cd).
        #[arg(long)]
        filter: Option<String>,
    },
    /// Compare two datums.
    #[clap(visible_alias = "d")]
    Diff {
        id1: usize,
        id2: usize,
        /// Confidence level for the interval.
        #[arg(short, long, value_enum, default_value_t = ConfidenceLevel::default())]
        confidence: ConfidenceLevel,
    },
    /// List datums.
    #[clap(visible_alias = "l")]
    List,
}

fn main() {
    let cli = Cli::parse();
    let app = App::new(cli.file);
    match cli.mode {
        Mode::Bench {
            comment,
            order,
            filter,
        } => app.cmd_bench(comment, order, filter),
        Mode::Diff {
            id1,
            id2,
            confidence,
        } => app.cmd_diff(id1, id2, confidence),
        Mode::List => app.cmd_list(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        App, ConfidenceLevel, DEFAULT_CONFIG_FILE, ExecutionOrder, ExtraToml, SummaryStats,
        filter_benchmarks,
    };
    use clap::ValueEnum;
    use std::path::PathBuf;

    #[test]
    fn filter_benchmarks_restricts_named_benchmarks() {
        let config: crate::config::Config = toml::from_str(
            r#"
                proc_execs = 1
                inproc_iters = 1

                [executors]
                e1 = "/bin/sh"

                [suites.s]
                dir = "."
                harness = "h"
                [suites.s.benchmarks.b1]
                [suites.s.benchmarks.b2]
                [suites.s.benchmarks.b3]
            "#,
        )
        .unwrap();
        // b4 is unknown and ignored; b2 is omitted from the filter and dropped from the config.
        let config = filter_benchmarks(config, "b1, b3, b4");
        let names: Vec<&str> = config.suites["s"]
            .benchmarks
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(names, ["b1", "b3"]);
    }

    #[test]
    fn cis_overlap() {
        let s1 = SummaryStats::new(10., 5.);
        let s2 = SummaryStats::new(5., 8.);
        let s3 = SummaryStats::new(50.6, 20.6667);
        let s4 = SummaryStats::new(-0.5, 0.1);
        let s5 = SummaryStats::new(-0.5, 0.2);
        assert!(s1.ci_overlaps(&s2));
        assert!(s2.ci_overlaps(&s1));
        assert!(s1.ci_overlaps(&s1));
        assert!(s2.ci_overlaps(&s2));
        assert!(!s1.ci_overlaps(&s3));
        assert!(!s3.ci_overlaps(&s1));
        assert!(s1.ci_overlaps(&s1));
        assert!(s2.ci_overlaps(&s2));
        assert!(s3.ci_overlaps(&s3));
        assert!(s4.ci_overlaps(&s5));
        assert!(s5.ci_overlaps(&s4));
        assert!(!s4.ci_overlaps(&s1));
        assert!(s4.ci_overlaps(&s2));
        assert!(!s4.ci_overlaps(&s3));
        assert!(!s5.ci_overlaps(&s1));
        assert!(s5.ci_overlaps(&s2));
        assert!(!s5.ci_overlaps(&s3));
    }

    #[test]
    fn test_default_config_path() {
        use std::fs;

        // Create a temporary directory for testing
        let temp_dir = std::env::temp_dir().join("haste_test_default");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_dir).unwrap();

        // Create the app with no config specified
        let app = App::new(None);

        // Should use the default CONFIG_FILE
        assert_eq!(app.config_file, PathBuf::from(DEFAULT_CONFIG_FILE));

        // Cleanup
        std::env::set_current_dir(original_dir).unwrap();
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_config_file_loading() {
        use std::fs;

        let temp_dir = std::env::temp_dir().join("haste_test_loading");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        // Create a test config file
        let config_content = r#"
        proc_execs = 2
        inproc_iters = 3

        [executors]
        test_exec = "/bin/sh"

        [suites.test_suite]
        dir = "."
        harness = "test.sh"

        [suites.test_suite.benchmarks.test_bench]
        extra_args = ["arg1"]
        "#;
        let config_path = temp_dir.join("test_config.toml");
        fs::write(&config_path, config_content).unwrap();

        // Parse the config
        let config_text = fs::read_to_string(&config_path).unwrap();
        let config: crate::config::Config = toml::from_str(&config_text).unwrap();

        // Verify parsed values
        assert_eq!(config.proc_execs, 2);
        assert_eq!(config.inproc_iters, 3);
        assert_eq!(config.executors.len(), 1);
        assert!(config.executors.contains_key("test_exec"));
        assert_eq!(config.suites.len(), 1);
        assert!(config.suites.contains_key("test_suite"));

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_benchmark_order_preserved() {
        // Declare benchmarks in a deliberately non-alphabetical order to ensure the order is
        // preserved from the config file rather than being sorted or hashed into some other order.
        let config_content = r#"
        proc_execs = 1
        inproc_iters = 1

        [executors]
        test_exec = "/bin/sh"

        [suites.test_suite]
        dir = "."
        harness = "test.sh"

        [suites.test_suite.benchmarks.charlie]
        [suites.test_suite.benchmarks.alpha]
        [suites.test_suite.benchmarks.bravo]
        [suites.test_suite.benchmarks.delta]
        "#;

        let config: crate::config::Config = toml::from_str(config_content).unwrap();
        let suite = &config.suites["test_suite"];
        let order: Vec<&str> = suite.benchmarks.keys().map(String::as_str).collect();
        assert_eq!(order, ["charlie", "alpha", "bravo", "delta"]);
    }

    #[test]
    fn confidence_level_default() {
        assert_eq!(ConfidenceLevel::default(), ConfidenceLevel::CL99);
    }

    #[test]
    fn execution_order_from_str() {
        assert_eq!(
            ExecutionOrder::from_str("declaration", false).unwrap(),
            ExecutionOrder::Declaration
        );
        assert_eq!(
            ExecutionOrder::from_str("randomised", false).unwrap(),
            ExecutionOrder::Randomised
        );
    }

    #[test]
    fn confidence_level_from_str() {
        assert_eq!(
            ConfidenceLevel::from_str("90", false).unwrap(),
            ConfidenceLevel::CL90
        );
        assert_eq!(
            ConfidenceLevel::from_str("95", false).unwrap(),
            ConfidenceLevel::CL95
        );
        assert_eq!(
            ConfidenceLevel::from_str("99", false).unwrap(),
            ConfidenceLevel::CL99
        );
    }

    #[test]
    fn extra_toml_loads_all_fields() {
        let extra: ExtraToml = toml::from_str(
            r#"
comment = "baseline"
[executors]
csom = "/opt/SOM++"
yksom = "/opt/yksom"
[suite_env.awfy]
SOM_LIB = "/opt/Smalltalk"
"#,
        )
        .unwrap();
        assert_eq!(extra.comment.as_deref(), Some("baseline"));
        let exec_keys: Vec<&str> = extra.executors.keys().map(String::as_str).collect();
        assert_eq!(exec_keys, ["csom", "yksom"]);
        assert_eq!(extra.executors["csom"], PathBuf::from("/opt/SOM++"));
        assert_eq!(extra.suite_env["awfy"]["SOM_LIB"], "/opt/Smalltalk");
        // Datums created before machine info was recorded have no `[machine]` table.
        assert!(extra.machine.is_none());

        let extra: ExtraToml = toml::from_str(
            r#"
[executors]
[suite_env]
[machine]
os = "Linux (Debian GNU/Linux 13)"
cpu = "Intel(R) Xeon(R) Platinum 8452Y"
memory = "32 GiB"
arch = "x86_64"
cpu_count = "8"
"#,
        )
        .unwrap();
        let machine = extra.machine.unwrap();
        let keys: Vec<&str> = machine.keys().map(String::as_str).collect();
        assert_eq!(keys, ["os", "cpu", "memory", "arch", "cpu_count"]);
        assert_eq!(machine["os"], "Linux (Debian GNU/Linux 13)");
        assert_eq!(machine["cpu"], "Intel(R) Xeon(R) Platinum 8452Y");
        assert_eq!(machine["memory"], "32 GiB");
        assert_eq!(machine["arch"], "x86_64");
        assert_eq!(machine["cpu_count"], "8");
    }
}
