//! `cargo run --release --example actor_bench -- <mode>`: benchmark and
//! crash-test harness for the workspace actor (`src/actor/mod.rs`) against
//! today's direct `actions::dispatch` path. Modes: `latency`, `batch`,
//! `wake`, `crash`, `litestream`, `partitions`, `all`. `crash-child` is an internal
//! subcommand the `crash` mode spawns itself as; it is not a mode to run
//! directly.

mod batch;
mod common;
mod crash;
mod crash_child;
mod latency;
mod litestream;
mod partitions;
mod wake;

use std::fmt::Write as _;

use serde_json::Value;
use tracing_subscriber::EnvFilter;

fn render_markdown(report: &Value) -> String {
    let mut out = String::new();
    if let Value::Object(sections) = report {
        for (section, value) in sections {
            let _ = write!(out, "\n## {section}\n\n| key | value |\n|---|---|\n");
            flatten(value, "", &mut out);
        }
    }
    out
}

fn flatten(value: &Value, prefix: &str, out: &mut String) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(v, &key, out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                flatten(v, &format!("{prefix}[{i}]"), out);
            }
        }
        other => {
            let _ = writeln!(out, "| {prefix} | {other} |");
        }
    }
}

fn print_report(report: &Value) -> anyhow::Result<()> {
    println!("===JSON_START===");
    println!("{}", serde_json::to_string_pretty(report)?);
    println!("===JSON_END===");
    println!("===MARKDOWN_START===");
    println!("{}", render_markdown(report));
    println!("===MARKDOWN_END===");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map_or("all", String::as_str);

    if mode == "crash-child" {
        let rest: Vec<String> = args.get(2..).unwrap_or(&[]).to_vec();
        return crash_child::run_child(&rest).await;
    }

    let report = match mode {
        "latency" => serde_json::json!({ "latency": latency::run().await? }),
        "batch" => serde_json::json!({ "batch": batch::run().await? }),
        "wake" => serde_json::json!({ "wake": wake::run().await? }),
        "crash" => serde_json::json!({ "crash": crash::run().await? }),
        "litestream" => serde_json::json!({ "litestream": litestream::run().await? }),
        "partitions" => serde_json::json!({ "partitions": partitions::run().await? }),
        "all" => serde_json::json!({
            "latency": latency::run().await?,
            "batch": batch::run().await?,
            "wake": wake::run().await?,
            "crash": crash::run().await?,
            "litestream": litestream::run().await?,
            "partitions": partitions::run().await?,
        }),
        other => {
            eprintln!(
                "unknown mode {other:?}; expected one of: latency, batch, wake, crash, litestream, partitions, all"
            );
            std::process::exit(2);
        }
    };
    print_report(&report)?;
    Ok(())
}
