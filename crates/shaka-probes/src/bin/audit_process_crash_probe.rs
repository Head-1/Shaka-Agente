//! Probe multiprocesso da cadeia de auditoria.
//!
//! Coordena processos concorrentes, força a queda de um filho e verifica que
//! a cadeia de auditoria permanece válida após a recuperação.

use chrono::{DateTime, Utc};
use shaka_core::{AuditEvent, TenantId};
use shaka_memory::MemoryStore;
use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::thread;
use std::time::Duration;

const TENANT: &str = "tenant-process-crash";
const WORKERS: usize = 8;
const WAIT_ATTEMPTS: usize = 7_500;

fn wait_for_marker(path: &Path) -> bool {
    (0..WAIT_ATTEMPTS).any(|_| {
        if path.exists() {
            true
        } else {
            thread::sleep(Duration::from_millis(2));
            false
        }
    })
}

fn open_with_retry(path: &Path) -> Result<MemoryStore, Box<dyn Error>> {
    for attempt in 0..20 {
        match MemoryStore::open(path) {
            Ok(store) => return Ok(store),
            Err(error)
                if error.to_string().contains("database is locked")
                    || error.to_string().contains("database is busy") =>
            {
                let backoff_ms = 25_u64 * u64::try_from(attempt + 1)?;
                thread::sleep(Duration::from_millis(backoff_ms));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err("database remained locked after bounded retries".into())
}

fn cleanup(database: &Path, workspace: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let candidate = if suffix.is_empty() {
            database.to_path_buf()
        } else {
            PathBuf::from(format!("{}{}", database.display(), suffix))
        };
        let _ = std::fs::remove_file(candidate);
    }
    let _ = std::fs::remove_dir_all(workspace);
}

fn terminate_children(children: &mut [Child]) {
    for child in children {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn child_mode(args: &[String]) -> Result<(), Box<dyn Error>> {
    let database = args.get(2).ok_or("missing database path")?;
    let ready = args.get(3).ok_or("missing ready marker path")?;
    let start = args.get(4).ok_or("missing start marker path")?;
    let timestamp = args
        .get(5)
        .ok_or("missing event timestamp")?
        .parse::<DateTime<Utc>>()?;
    let index = args.get(6).ok_or("missing worker index")?;
    let crash = args.get(7).is_some_and(|value| value == "crash");

    let store = open_with_retry(Path::new(database))?;
    std::fs::write(ready, b"ready")?;
    if !wait_for_marker(Path::new(start)) {
        return Err("child did not observe start marker".into());
    }

    let mut event = AuditEvent::new(
        None,
        TenantId::new(TENANT)?,
        format!("actor-{index}"),
        "process.append",
        "success",
        BTreeMap::new(),
        None,
    );
    event.occurred_at = timestamp;
    store.append_audit_event(&event)?;
    if crash {
        std::process::abort();
    }
    Ok(())
}

fn run_parent() -> Result<(), Box<dyn Error>> {
    let database = std::env::temp_dir().join(format!(
        "shaka-audit-process-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let workspace =
        std::env::temp_dir().join(format!("shaka-audit-process-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace)?;
    let start = workspace.join("start");
    let timestamp = Utc::now();
    let executable = std::env::current_exe()?;
    let mut children = Vec::with_capacity(WORKERS);

    let result = (|| -> Result<(), Box<dyn Error>> {
        let store = open_with_retry(&database)?;
        drop(store);

        for index in 0..WORKERS {
            let ready = workspace.join(format!("ready-{index}"));
            let mut command = Command::new(&executable);
            command
                .arg("child")
                .arg(&database)
                .arg(&ready)
                .arg(&start)
                .arg(timestamp.to_rfc3339())
                .arg(index.to_string());
            if index == 0 {
                command.arg("crash");
            }
            children.push(command.spawn()?);
        }

        for index in 0..WORKERS {
            let ready = workspace.join(format!("ready-{index}"));
            if !wait_for_marker(&ready) {
                return Err(format!("child {index} did not become ready").into());
            }
        }
        std::fs::write(&start, b"go")?;

        let statuses = children
            .drain(..)
            .map(|mut child| child.wait())
            .collect::<Result<Vec<ExitStatus>, _>>()?;
        let crashed = statuses.iter().filter(|status| !status.success()).count();
        if crashed != 1 {
            return Err(format!("expected one crashed child, got {crashed}").into());
        }

        let store = open_with_retry(&database)?;
        let tenant = TenantId::new(TENANT)?;
        let verification = store.verify_audit_chain(&tenant)?;
        if !verification.valid || verification.checked_events != WORKERS as u64 {
            return Err(format!(
                "audit verification failed: valid={} checked_events={}",
                verification.valid, verification.checked_events
            )
            .into());
        }
        println!(
            "audit_process_probe=PASS processes={} one_process_crashed=true audit_chain_valid=true checked_events={}",
            statuses.len(),
            verification.checked_events
        );
        Ok(())
    })();

    terminate_children(&mut children);
    cleanup(&database, &workspace);
    result
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).is_some_and(|mode| mode == "child") {
        child_mode(&args)
    } else {
        run_parent()
    }
}
