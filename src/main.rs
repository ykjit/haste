use clap::{Parser, Subcommand};
use comfy_table::{Cell, CellAlignment, Color, Table};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    env, fmt, fs,
    path::PathBuf,
    process,
};

mod config;
mod runner;

/// The z* value used for a 95% confidence interval.
const CI_ZVAL: f64 = 1.96;
/// The confidence level of the above, as a percentage.
const CI_CONF: f64 = 95.;

/// The `extra.toml` file for a datum
#[derive(Default, Serialize, Deserialize)]
struct ExtraToml {
    comment: Option<String>,
}

/// The name of the hidden directory we store state inside.
const DOT_DIR: &str = ".haste";
/// The name of the haste config file.
const CONFIG_FILE: &str = "haste.toml";

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
    // String benchmark key -> collection of process execution times.
    data: HashMap<String, Vec<f64>>,
}

impl ResultFile {
    fn summarise(&self) -> HashMap<String, SummaryStats> {
        let mut summaries = HashMap::new();
        for (k, invocs) in &self.data {
            let n = f64::from(u32::try_from(invocs.len()).unwrap());
            let mean = invocs.iter().sum::<f64>() / n;

            // Compute a confidence interval, as per:
            // https://www.dummies.com/article/academics-the-arts/math/statistics/how-to-calculate-a-confidence-interval-for-a-population-mean-when-you-know-its-standard-deviation-169722/
            let ci = if invocs.len() > 1 {
                let variance = invocs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.);
                let std_dev = variance.sqrt();
                CI_ZVAL * std_dev / n.sqrt()
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
            return Err("results files contain different benchmarks".into());
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
    fn store_datum(&self, results: ResultFile, comment: Option<String>) -> usize {
        let id = self.next_id();
        let datum_dir = self.get_datum_dir(id);
        fs::create_dir(&datum_dir).unwrap();
        let res_path = self.get_datum_results_path(id);
        let tml = toml::to_string(&results).unwrap();
        fs::write(res_path, tml).unwrap();

        // Write out the extra metadata.
        // FIXME: consider merging this into the main toml file.
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
            ExtraToml::default()
        }
    }

    /// Run benchmarks and store the results as a new datum.
    ///
    /// If successful, the new datum is printed to stdout.
    fn cmd_bench(&self, comment: Option<String>) {
        let config_text = fs::read_to_string(CONFIG_FILE).unwrap_or_else(|e| {
            eprintln!("error: failed to read {CONFIG_FILE}: {e}");
            process::exit(1);
        });
        let config: config::Config = match toml::from_str(&config_text) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Unable to parse {CONFIG_FILE}: {e}");
                std::process::exit(1);
            }
        };
        let results = runner::run(&config);
        let id = self.store_datum(results, comment.to_owned());
        let comment_s = comment.unwrap_or("".to_owned());
        println!("haste: created datum {id} {comment_s}");
    }

    fn cmd_diff(&self, id1: usize, id2: usize) {
        let tml1 = fs::read_to_string(self.get_datum_results_path(id1)).unwrap();
        let tml2 = fs::read_to_string(self.get_datum_results_path(id2)).unwrap();
        let data1 = toml::from_str::<ResultFile>(&tml1).unwrap();
        let data2 = toml::from_str::<ResultFile>(&tml2).unwrap();

        if let Err(e) = data1.same_dims(&data2) {
            eprintln!("{e}");
            process::exit(1);
        }

        let data1 = data1.summarise();
        let data2 = data2.summarise();

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

        // If there's any extra metadata, print it.
        let extra1 = self.load_extra(id1);
        let extra2 = self.load_extra(id2);
        if extra1.comment.is_some() || extra2.comment.is_some() {
            let no_comment = "(no comment)".to_owned();
            println!(
                "Datum{id1}: {}",
                extra1.comment.unwrap_or(no_comment.clone())
            );
            println!("Datum{id2}: {}\n", extra2.comment.unwrap_or(no_comment));
        }

        println!("confidence level: {}%\n", CI_CONF);
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
    },
    /// Compare two datums.
    #[clap(visible_alias = "d")]
    Diff { id1: usize, id2: usize },
    /// List datums.
    #[clap(visible_alias = "l")]
    List,
}

fn main() {
    let app = App::new();
    let cli = Cli::parse();
    match cli.mode {
        Mode::Bench { comment } => app.cmd_bench(comment),
        Mode::Diff { id1, id2 } => app.cmd_diff(id1, id2),
        Mode::List => app.cmd_list(),
    }
}

#[cfg(test)]
mod tests {
    use super::SummaryStats;

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
}
