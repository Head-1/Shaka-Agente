//! Probe multiprocesso de recuperação da fila.
//!
//! Reproduz a queda de um worker após o claim e verifica que a lease expirada
//! retorna à fila e pode ser reclamada após a reabertura do banco.

use chrono::{Duration, Utc};
use serde_json::Value;
use shaka_core::{OperatorId, Principal, Role, TaskEnvelope, TenantId};
use shaka_queue::{QueueStore, TaskStatus};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration as StdDuration;

const TENANT: &str = "tenant-process-crash";
const OPERATOR: &str = "operator-process-crash";

fn wait_for_marker(path: &Path, attempts: usize) -> bool {
    (0..attempts).any(|_| {
        if path.exists() {
            true
        } else {
            thread::sleep(StdDuration::from_millis(2));
            false
        }
    })
}

fn cleanup(path: &Path, marker: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let candidate = if suffix.is_empty() {
            path.to_path_buf()
        } else {
            PathBuf::from(format!("{}{}", path.display(), suffix))
        };
        let _ = std::fs::remove_file(candidate);
    }
    let _ = std::fs::remove_file(marker);
}

fn child_mode(args: &[String]) -> Result<(), Box<dyn Error>> {
    let path = args.get(2).ok_or("missing database path")?;
    let marker = args.get(3).ok_or("missing claim marker path")?;
    let store = QueueStore::open(path)?;
    let claimed = store.claim_next(Utc::now(), Duration::seconds(1))?;
    if claimed.is_none() {
        return Err("child did not claim task".into());
    }
    std::fs::write(marker, b"claimed")?;
    std::process::abort();
}

fn run_parent() -> Result<(), Box<dyn Error>> {
    let path = std::env::temp_dir().join(format!(
        "shaka-queue-process-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    let marker = std::env::temp_dir().join(format!("shaka-queue-claimed-{}", uuid::Uuid::new_v4()));
    let principal = Principal {
        operator_id: OperatorId::new(OPERATOR)?,
        tenant_id: TenantId::new(TENANT)?,
        role: Role::Administrator,
    };
    let store = QueueStore::open(&path)?;
    store.bootstrap_principal(&principal)?;
    let session = store.create_session(principal.clone(), Value::Null)?;
    let envelope = TaskEnvelope::new(
        principal.tenant_id.clone(),
        principal.operator_id.clone(),
        "restart recovery probe",
    )?;
    let task_id = envelope.task_id.clone();
    store.submit_task_governed(
        session.session_id,
        &principal,
        "process-crash-idempotency",
        "process-crash-fingerprint",
        &envelope,
        0,
        2,
    )?;
    drop(store);

    let result = (|| -> Result<(), Box<dyn Error>> {
        let executable = std::env::current_exe()?;
        let mut child = Command::new(executable)
            .args([
                "child",
                path.to_str().ok_or("invalid database path")?,
                marker.to_str().ok_or("invalid claim marker path")?,
            ])
            .spawn()?;
        if !wait_for_marker(&marker, 500) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("child did not publish claim marker".into());
        }
        let status = child.wait()?;
        if status.success() {
            return Err("child crash probe exited successfully".into());
        }

        thread::sleep(StdDuration::from_millis(1_200));
        let store = QueueStore::open(&path)?;
        let recovered = store.recover_expired_leases(Utc::now())?;
        if recovered != 1 {
            return Err(format!("expected one recovered lease, got {recovered}").into());
        }
        let recovered_task = store.get_task(&task_id, &principal.tenant_id)?;
        if recovered_task.status != TaskStatus::Queued {
            return Err(format!(
                "expected queued task after recovery, got {:?}",
                recovered_task.status
            )
            .into());
        }
        let reclaimed = store
            .claim_next(Utc::now(), Duration::seconds(1))?
            .ok_or("recovered task could not be reclaimed")?;
        if reclaimed.task_id != task_id {
            return Err("reclaimed task id differs from original".into());
        }
        println!(
            "queue_process_probe=PASS child_crashed=true recovered={} status_after_recovery={} attempts_after_recovery={} reclaim_after_restart=true",
            recovered,
            recovered_task.status.as_str(),
            recovered_task.attempts
        );
        Ok(())
    })();

    cleanup(&path, &marker);
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
