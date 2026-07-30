use pando::{
    backend::PlatformCowBackend,
    home::state_dir,
    lifecycle::{create_workspace, destroy_workspace},
};
use serde::Serialize;
use std::{
    env,
    error::Error,
    ffi::OsStr,
    fs,
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Serialize)]
struct BenchmarkResult {
    benchmark: &'static str,
    timestamp_ms: u128,
    platform: &'static str,
    source: PathBuf,
    home: PathBuf,
    samples_ms: Vec<f64>,
    median_ms: f64,
    min_ms: f64,
    max_ms: f64,
    cleanup_passed: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("benchmark failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os()
        .skip(1)
        .filter(|argument| argument.as_os_str() != OsStr::new("--bench"));
    let mut source = None;
    let mut output = None;
    let mut samples = 1usize;
    let mut home = None;

    while let Some(argument) = arguments.next() {
        if argument == OsStr::new("--output") {
            if output.is_some() {
                return Err("the create benchmark accepts only one --output path".into());
            }
            output = Some(
                arguments
                    .next()
                    .map(PathBuf::from)
                    .ok_or("--output requires a file path")?,
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
        if argument == OsStr::new("--home") {
            if home.is_some() {
                return Err("the create benchmark accepts only one --home path".into());
            }
            home = Some(
                arguments
                    .next()
                    .map(PathBuf::from)
                    .ok_or("--home requires a directory path")?,
            );
            continue;
        }
        if source.is_some() {
            return Err("the create benchmark accepts exactly one workspace directory".into());
        }
        source = Some(PathBuf::from(argument));
    }

    let source = source.ok_or(
        "usage: cargo bench --bench create -- /path/to/workspace [--samples N] [--home /path/to/home] [--output /path/to/result.json]",
    )?;
    let source = fs::canonicalize(source)?;
    if !source.is_dir() {
        return Err(format!("source must be a directory: {}", source.display()).into());
    }

    let run_id = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let process_id = std::process::id();
    let home = match home {
        Some(path) => path,
        None => source
            .parent()
            .ok_or("source has no parent directory")?
            .join(format!(".pando-bench-{process_id}-{run_id}")),
    };
    if home.exists() {
        return Err(format!("benchmark home already exists: {}", home.display()).into());
    }
    fs::create_dir_all(&home)?;

    let backend = PlatformCowBackend::default();
    let mut samples_ms = Vec::with_capacity(samples);

    for sample in 1..=samples {
        let name = format!("benchmark-{process_id}-{run_id}-{sample}");
        let started = Instant::now();
        let destination = create_workspace(&home, &backend, &name, &source, None)?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;

        println!(
            "create\t{sample}/{samples}\t{elapsed_ms:.3} ms\t{}",
            destination.display()
        );

        if let Err(error) = destroy_workspace(&home, &backend, &name, false) {
            return Err(format!("failed to destroy benchmark workspace {name}: {error:#}").into());
        }
        if state_dir(&home, &name).exists() {
            return Err(format!("destroy left state behind for benchmark workspace {name}").into());
        }
        samples_ms.push(elapsed_ms);
    }

    let mut sorted = samples_ms.clone();
    sorted.sort_by(f64::total_cmp);
    let median_ms = if samples.is_multiple_of(2) {
        (sorted[samples / 2 - 1] + sorted[samples / 2]) / 2.0
    } else {
        sorted[samples / 2]
    };
    let min_ms = sorted[0];
    let max_ms = sorted[samples - 1];

    if samples > 1 {
        println!("median\t{median_ms:.3} ms\tmin {min_ms:.3} ms\tmax {max_ms:.3} ms");
    }

    if let Some(output) = output {
        let result = BenchmarkResult {
            benchmark: "create",
            timestamp_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
            platform: env::consts::OS,
            source,
            home: home.clone(),
            samples_ms,
            median_ms,
            min_ms,
            max_ms,
            cleanup_passed: true,
        };
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, serde_json::to_string_pretty(&result)? + "\n")?;
    }

    let _ = fs::remove_dir_all(&home);
    Ok(())
}
