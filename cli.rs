use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use prop_amm_engine::runner::StrategyRunner;
use prop_amm_engine::sim::run_parallel;
use prop_amm_engine::types::{SimConfig, STORAGE_SIZE};
use serde_json::json;

#[derive(Parser)]
#[command(name = "prop-amm-multi", about = "CLI for Prop AMM Multi strategies")]
struct Cli {
	#[command(subcommand)]
	command: Commands,
}

#[derive(Subcommand)]
enum Commands {
	Validate {
		files: Vec<PathBuf>,
	},
	Run {
		files: Vec<PathBuf>,
		#[arg(long, default_value_t = 100)]
		simulations: usize,
		#[arg(long, default_value_t = 10_000)]
		steps: usize,
		#[arg(long, default_value_t = 1_000)]
		epoch_len: usize,
		#[arg(long, default_value_t = 0)]
		seed_start: u64,
	},
	Submit {
		files: Vec<PathBuf>,
		#[arg(long, default_value_t = 250)]
		simulations: usize,
		#[arg(long, default_value_t = 10_000)]
		steps: usize,
		#[arg(long, default_value_t = 1_000)]
		epoch_len: usize,
		#[arg(long, default_value_t = 0)]
		seed_start: u64,
	},
}

fn main() -> Result<()> {
	let cli = Cli::parse();
	match cli.command {
		Commands::Validate { files } => validate_cmd(&files),
		Commands::Run {
			files,
			simulations,
			steps,
			epoch_len,
			seed_start,
		} => run_cmd(&files, simulations, steps, epoch_len, seed_start, false),
		Commands::Submit {
			files,
			simulations,
			steps,
			epoch_len,
			seed_start,
		} => run_cmd(&files, simulations, steps, epoch_len, seed_start, true),
	}
}

fn validate_cmd(files: &[PathBuf]) -> Result<()> {
	if files.is_empty() {
		bail!("Provide at least one strategy source file.");
	}

	for file in files {
		let artifact = compile_strategy(file)?;
		let runner = StrategyRunner::load(&artifact).map_err(|e| {
			anyhow::anyhow!("failed to load compiled strategy for {}: {e}", file.display())
		})?;

		let storage = [0u8; STORAGE_SIZE];
		let rx = 100 * 1_000_000_000u64;
		let ry = 10_000 * 1_000_000_000u64;

		let out_small = runner.compute_swap(true, 1_000_000_000u64, rx, ry, &storage);
		let out_large = runner.compute_swap(true, 5_000_000_000u64, rx, ry, &storage);
		if out_small == 0 || out_large == 0 {
			bail!("{} produced zero output on validation quotes", file.display());
		}
		if out_large <= out_small {
			bail!("{} failed monotonicity check", file.display());
		}

		println!("[PASS] {}", file.display());
	}

	Ok(())
}

fn run_cmd(
	files: &[PathBuf],
	simulations: usize,
	steps: usize,
	epoch_len: usize,
	seed_start: u64,
	submit_mode: bool,
) -> Result<()> {
	if files.is_empty() {
		bail!("Provide at least one strategy source file.");
	}

	validate_cmd(files)?;

	let artifacts: Vec<PathBuf> = files
		.iter()
		.map(|p| compile_strategy(p.as_path()))
		.collect::<Result<Vec<_>>>()?;

	let mut config = SimConfig::default();
	config.total_steps = steps;
	config.epoch_len = epoch_len;

	let results = run_parallel(&artifacts, &config, simulations, seed_start);

	println!("\nStrategy                           Mean Edge    Std Edge   vs Norm    Sharpe   Final Cap%");
	println!("---------------------------------------------------------------------------------------------");
	for r in &results {
		println!(
			"{:<34} {:>10.2} {:>10.2} {:>9.2} {:>9.3} {:>10.2}",
			r.name,
			r.mean_edge,
			r.std_edge,
			r.edge_vs_normalizer,
			r.sharpe,
			r.mean_final_capital_weight * 100.0
		);
	}

	if submit_mode {
		let receipt = write_submission_receipt(files, &results, simulations, steps, epoch_len, seed_start)?;
		println!("\nSubmission receipt: {}", receipt.display());
	}

	Ok(())
}

fn compile_strategy(file: &Path) -> Result<PathBuf> {
	if !file.exists() {
		bail!("strategy file not found: {}", file.display());
	}

	let target_dir = PathBuf::from("target/strategies");
	fs::create_dir_all(&target_dir)?;

	let stem = file
		.file_stem()
		.and_then(|s| s.to_str())
		.context("invalid strategy filename")?;

	let output = target_dir.join(format!("lib{}_{}", stem, dylib_ext()));

	let status = Command::new("rustc")
		.arg(file)
		.arg("--edition")
		.arg("2021")
		.arg("--crate-type")
		.arg("cdylib")
		.arg("-O")
		.arg("-o")
		.arg(&output)
		.status()
		.with_context(|| format!("failed to invoke rustc for {}", file.display()))?;

	if !status.success() {
		bail!("rustc failed compiling {}", file.display());
	}

	Ok(output)
}

fn write_submission_receipt(
	files: &[PathBuf],
	results: &[prop_amm_engine::sim::AggregatedResult],
	simulations: usize,
	steps: usize,
	epoch_len: usize,
	seed_start: u64,
) -> Result<PathBuf> {
	let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
	let out_dir = PathBuf::from("submissions").join(format!("submission_{}", ts));
	fs::create_dir_all(&out_dir)?;

	for file in files {
		let dest = out_dir.join(
			file.file_name()
				.context("invalid source filename")?,
		);
		fs::copy(file, dest)?;
	}

	let payload = json!({
		"timestamp": ts,
		"simulations": simulations,
		"steps": steps,
		"epoch_len": epoch_len,
		"seed_start": seed_start,
		"strategies": results.iter().map(|r| json!({
			"name": r.name,
			"mean_edge": r.mean_edge,
			"std_edge": r.std_edge,
			"edge_vs_normalizer": r.edge_vs_normalizer,
			"sharpe": r.sharpe,
			"mean_final_capital_weight": r.mean_final_capital_weight
		})).collect::<Vec<_>>()
	});

	let receipt = out_dir.join("receipt.json");
	fs::write(&receipt, serde_json::to_vec_pretty(&payload)?)?;
	Ok(receipt)
}

fn dylib_ext() -> &'static str {
	#[cfg(target_os = "macos")]
	{
		"dylib"
	}
	#[cfg(target_os = "linux")]
	{
		"so"
	}
	#[cfg(target_os = "windows")]
	{
		"dll"
	}
}
