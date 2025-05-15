use comfy_table::{Cell, Color, Table};
use std::{
    collections::{HashMap, HashSet},
    env, fmt, fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process,
};

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

/// Rebench data file parser.
#[derive(Debug)]
struct ResultFile {
    data: HashMap<BenchKey, Vec<Vec<f64>>>,
}

impl ResultFile {
    fn new(p: &Path) -> Self {
        let f = fs::File::open(p).unwrap();
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
    fn summarise(&self) -> HashMap<BenchKey, f64> {
        let mut summary = HashMap::new();
        for (k, invocs) in &self.data {
            let mut invoc_means: Vec<f64> = Vec::new();
            for invoc in invocs {
                invoc_means.push(invoc.iter().sum::<f64>() / invoc.len() as f64);
            }
            summary.insert(
                k.to_owned(),
                invoc_means.iter().sum::<f64>() / invoc_means.len() as f64,
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
    fn store_datum(&self) -> usize {
        let id = self.next_id();
        fs::create_dir(self.get_datum_dir(id)).unwrap();
        let copy_to = self.get_datum_results_path(id);
        fs::rename("benchmark.data", copy_to).unwrap();
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

    /// Run benchmarks and store the results as a new datum.
    ///
    /// If successful, the new datum is printed to stdout.
    fn cmd_bench(&self) {
        let mut cmd = process::Command::new("rebench");
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
        let id = self.store_datum();
        println!("haste: created datum {id}");
    }

    fn cmd_diff(&self, id1: usize, id2: usize) {
        let data1 = ResultFile::new(&self.get_datum_results_path(id1));
        let data2 = ResultFile::new(&self.get_datum_results_path(id2));

        if let Err(e) = data1.same_dims(&data2) {
            eprintln!("{e}");
            process::exit(1);
        }

        let data1 = data1.summarise();
        let data2 = data2.summarise();

        let mut rows = Vec::new();
        for (k, v1) in &data1 {
            let mut row = Vec::new();
            let v2 = data2[k];
            let ratio = v2 / v1;
            let change = (ratio - 1.0) * 100.0;
            let abs_change = change.abs();

            row.push(Cell::new(k));
            row.push(Cell::new(format!("{v1:.0}")));
            row.push(Cell::new(format!("{v2:.0}")));
            row.push(Cell::new(format!("{ratio:.2}")));
            let change_cell = if change < 0.0 {
                Cell::new(format!("{abs_change:.2}% faster")).fg(Color::Green)
            } else {
                Cell::new(format!("{abs_change:.2}% slower")).fg(Color::Red)
            };
            row.push(change_cell);
            rows.push((change, row));
        }

        let mut table = Table::new();
        table.load_preset(comfy_table::presets::NOTHING);
        table.set_header(vec![
            "Benchmark",
            &format!("Datum{id1} (ms)"),
            &format!("Datum{id2} (ms)"),
            "Ratio",
            "Summary",
        ]);
        // Sort by speedup, descending.
        rows.sort_by(|(c1, _), (c2, _)| c1.partial_cmp(c2).unwrap());
        for (_, row) in rows {
            table.add_row(row);
        }
        println!("{table}");
    }
}

fn usage() -> ! {
    println!("usage: haste <command> <args>\n");
    println!("Available commands:");
    println!("  haste bench|b");
    println!("    run rebench and store the results into a new datum.");
    println!("    the rebench config file must be `$PWD/rebench.conf`\n");
    println!("  haste diff|d <base-id> <diff-id>");
    println!("    compare two datums");
    process::exit(1);
}

fn parse_id(args: &mut std::iter::Skip<env::Args>) -> Result<usize, ()> {
    let Some(arg) = args.next() else {
        return Err(());
    };
    let Ok(ret) = arg.parse::<usize>() else {
        return Err(());
    };
    Ok(ret)
}

fn main() {
    let app = App::new();

    let mut args = env::args().skip(1);
    let Some(mode) = args.next() else { usage() };

    match mode.as_str() {
        "b" | "bench" => {
            if args.next().is_some() {
                usage();
            }
            app.cmd_bench()
        }
        "d" | "diff" => {
            let id1 = parse_id(&mut args).unwrap_or_else(|_| usage());
            let id2 = parse_id(&mut args).unwrap_or_else(|_| usage());
            app.cmd_diff(id1, id2);
        }
        _ => usage(),
    }
}
