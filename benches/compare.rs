use serde::{Deserialize, Serialize};
use std::{
    env,
    error::Error,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
struct BenchmarkResult {
    median_ms: f64,
    min_ms: f64,
    max_ms: f64,
    cleanup_passed: bool,
}

#[derive(Serialize)]
struct ComparisonResult {
    benchmark: &'static str,
    timestamp_ms: u128,
    platform: &'static str,
    source: PathBuf,
    samples: usize,
    candidates: Vec<CandidateResult>,
}

#[derive(Serialize)]
struct CandidateResult {
    rank: usize,
    candidate: PathBuf,
    result: PathBuf,
    median_ms: f64,
    min_ms: f64,
    max_ms: f64,
    difference_from_fastest_percent: f64,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("comparison failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os()
        .skip(1)
        .filter(|argument| argument.as_os_str() != OsStr::new("--bench"));
    let mut source = None;
    let mut output = None;
    let mut samples = 10usize;
    let mut candidates = Vec::new();

    while let Some(argument) = arguments.next() {
        if argument == OsStr::new("--output") {
            if output.is_some() {
                return Err("the comparison accepts only one --output directory".into());
            }
            output = Some(
                arguments
                    .next()
                    .map(PathBuf::from)
                    .ok_or("--output requires a directory path")?,
            );
            continue;
        }
        if argument == OsStr::new("--samples") {
            let value = arguments.next().ok_or("--samples requires a count")?;
            samples = value
                .to_str()
                .ok_or("--samples requires a UTF-8 integer")?
                .parse::<usize>()?;
            if samples == 0 {
                return Err("--samples must be greater than zero".into());
            }
            continue;
        }
        if argument == OsStr::new("--candidate") {
            candidates.push(
                arguments
                    .next()
                    .map(PathBuf::from)
                    .ok_or("--candidate requires a repository path")?,
            );
            continue;
        }
        if source.is_some() {
            return Err("the comparison accepts exactly one workload directory".into());
        }
        source = Some(PathBuf::from(argument));
    }

    let source = source.ok_or(
        "usage: cargo bench --bench compare -- /path/to/workload --candidate /path/to/pando [--candidate /path/to/candidate] --output /path/to/results [--samples N]",
    )?;
    let output = output.ok_or("--output is required for comparison results")?;
    if candidates.is_empty() {
        return Err("provide at least one candidate repository with --candidate".into());
    }

    let source = fs::canonicalize(source)?;
    fs::create_dir_all(&output)?;
    let output = fs::canonicalize(output)?;
    let mut measured = Vec::with_capacity(candidates.len());

    for (index, candidate) in candidates.into_iter().enumerate() {
        let candidate = fs::canonicalize(candidate)?;
        if !candidate.join("Cargo.toml").exists() {
            return Err(format!(
                "candidate is not a Cargo workspace: {}",
                candidate.display()
            )
            .into());
        }
        let result_path = output.join(format!("candidate-{:02}.json", index + 1));
        run_candidate(&candidate, &source, samples, &result_path)?;
        let result: BenchmarkResult = serde_json::from_slice(&fs::read(&result_path)?)?;
        if !result.cleanup_passed {
            return Err(format!(
                "candidate did not clean up benchmark workspaces: {}",
                candidate.display()
            )
            .into());
        }
        measured.push(CandidateResult {
            rank: 0,
            candidate,
            result: result_path,
            median_ms: result.median_ms,
            min_ms: result.min_ms,
            max_ms: result.max_ms,
            difference_from_fastest_percent: 0.0,
        });
    }

    measured.sort_by(|left, right| left.median_ms.total_cmp(&right.median_ms));
    let fastest = measured[0].median_ms;
    for (index, candidate) in measured.iter_mut().enumerate() {
        candidate.rank = index + 1;
        candidate.difference_from_fastest_percent = (candidate.median_ms / fastest - 1.0) * 100.0;
        println!(
            "{}\t{:.3} ms\t+{:.2}%\t{}",
            candidate.rank,
            candidate.median_ms,
            candidate.difference_from_fastest_percent,
            candidate.candidate.display()
        );
    }

    let summary = ComparisonResult {
        benchmark: "create",
        timestamp_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        platform: env::consts::OS,
        source,
        samples,
        candidates: measured,
    };
    let summary_path = output.join("summary.json");
    fs::write(
        &summary_path,
        serde_json::to_string_pretty(&summary)? + "\n",
    )?;
    println!("summary\t{}", summary_path.display());
    Ok(())
}

fn run_candidate(
    candidate: &Path,
    source: &Path,
    samples: usize,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    println!("running\t{}", candidate.display());
    let status = Command::new("cargo")
        .current_dir(candidate)
        .args(["bench", "--locked", "--bench", "create", "--"])
        .arg(source)
        .args(["--samples", &samples.to_string(), "--output"])
        .arg(output)
        .status()?;
    if !status.success() {
        return Err(format!("candidate benchmark failed: {}", candidate.display()).into());
    }
    Ok(())
}
