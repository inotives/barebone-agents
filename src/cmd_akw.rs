//! `barebone-agent akw push | status` CLI verbs (EP-00015 Decision A2).

use std::path::Path;

use crate::akw_pusher::{
    self, default_manifest_path, default_mappings, status, Manifest, WatchedMapping,
};
use crate::cli::AkwCommand;
use crate::tools::akw_client::AkwClient;

pub async fn run(root_dir: &Path, cmd: AkwCommand) -> Result<(), String> {
    match cmd {
        AkwCommand::Push { agent } => run_push(root_dir, agent.as_deref()).await,
        AkwCommand::Status => run_status(root_dir),
    }
}

async fn run_push(root_dir: &Path, agent_override: Option<&str>) -> Result<(), String> {
    let mappings = default_mappings();
    let manifest_path = root_dir.join(default_manifest_path());

    let client = AkwClient::connect(root_dir, agent_override)
        .await
        .map_err(|e| e.to_string())?;
    println!("Using akw config from {}", client.source_path());

    let report = akw_pusher::push_cycle(&client, &mappings, &manifest_path, root_dir).await;
    client.shutdown().await;

    println!(
        "Push complete: {} created, {} updated, {} failed",
        report.created, report.updated, report.failed
    );
    if !report.failure_messages.is_empty() {
        println!("Failures:");
        for msg in &report.failure_messages {
            println!("  - {}", msg);
        }
    }
    if report.failed > 0 {
        return Err(format!("{} push(es) failed", report.failed));
    }
    Ok(())
}

fn run_status(root_dir: &Path) -> Result<(), String> {
    let mappings = default_mappings();
    let manifest_path = root_dir.join(default_manifest_path());
    let manifest = Manifest::load(&manifest_path);

    println!("Manifest: {}", manifest_path.display());
    if !manifest_path.exists() {
        println!("  (does not exist yet — first push will create it)");
    }
    println!();

    let report = status(&mappings, &manifest, root_dir);
    print_status_table(&report);
    Ok(())
}

fn print_status_table(items: &[crate::akw_pusher::MappingStatus]) {
    let label_w = items
        .iter()
        .map(|i| i.label.len())
        .max()
        .unwrap_or(15)
        .max(15);
    let dir_w = items
        .iter()
        .map(|i| i.local_dir.display().to_string().len())
        .max()
        .unwrap_or(20)
        .max(20);

    println!(
        "{:<label_w$}  {:<dir_w$}  {:>5}  {:>5}  {:>11}",
        "label",
        "local_dir",
        "files",
        "dirty",
        "never_pushed",
        label_w = label_w,
        dir_w = dir_w,
    );
    println!("{}", "-".repeat(label_w + dir_w + 5 + 5 + 11 + 8));
    for s in items {
        println!(
            "{:<label_w$}  {:<dir_w$}  {:>5}  {:>5}  {:>11}",
            s.label,
            s.local_dir.display(),
            s.file_count,
            s.dirty_count,
            s.never_pushed,
            label_w = label_w,
            dir_w = dir_w,
        );
    }
}

// Suppress unused warnings if `WatchedMapping` is only consumed transitively.
#[allow(dead_code)]
fn _ensure_used(_: &WatchedMapping) {}
