//! Execução de artefatos WASM com política deny-by-default.
//!
//! O MVP não habilita WASI nem expõe rede, filesystem, ambiente ou host
//! functions. O módulo precisa ser autocontido e exportar `run() -> i32`.

use serde::{Deserialize, Serialize};
use shaka_core::Capability;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::sync_channel,
    },
    thread,
    time::Duration,
};
use thiserror::Error;
use wasmtime::{Config, Engine, Instance, Module, Store, StoreLimits, StoreLimitsBuilder};

/// Limite máximo host-side para memória linear de uma execução WASM.
pub const MAX_SANDBOX_MEMORY_BYTES: u64 = 64 * 1024 * 1024;

const fn default_max_memory_bytes() -> u64 {
    16 * 1024 * 1024
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxPolicy {
    pub max_fuel: u64,
    pub max_elapsed_ms: u64,
    /// Limite de memória linear do guest em bytes.
    #[serde(default = "default_max_memory_bytes")]
    pub max_memory_bytes: u64,
    pub allow_network: bool,
    pub allow_filesystem: bool,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            max_fuel: 100_000,
            max_elapsed_ms: 1_000,
            max_memory_bytes: default_max_memory_bytes(),
            allow_network: false,
            allow_filesystem: false,
        }
    }
}

impl SandboxPolicy {
    pub fn validate(&self, required: &[Capability]) -> Result<(), SandboxError> {
        if required.contains(&Capability::Network) && !self.allow_network {
            return Err(SandboxError::CapabilityDenied(Capability::Network));
        }
        if required.contains(&Capability::FilesystemRead) && !self.allow_filesystem {
            return Err(SandboxError::CapabilityDenied(Capability::FilesystemRead));
        }
        if required.contains(&Capability::FilesystemWrite) && !self.allow_filesystem {
            return Err(SandboxError::CapabilityDenied(Capability::FilesystemWrite));
        }
        if self.max_memory_bytes == 0 || self.max_memory_bytes > MAX_SANDBOX_MEMORY_BYTES {
            return Err(SandboxError::InvalidPolicy(format!(
                "max_memory_bytes deve estar entre 1 e {MAX_SANDBOX_MEMORY_BYTES}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxResult {
    pub exit_code: i32,
    pub fuel_consumed: u64,
}

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("configuração inválida do sandbox: {0}")]
    InvalidPolicy(String),
    #[error("capacidade negada pelo sandbox: {0:?}")]
    CapabilityDenied(Capability),
    #[error("módulo WASM inválido: {0}")]
    InvalidModule(String),
    #[error("o módulo solicita imports do host, proibidos no MVP")]
    HostImportsDenied,
    #[error("o módulo não exporta run() -> i32: {0}")]
    MissingEntryPoint(String),
    #[error("execução WASM falhou: {0}")]
    Execution(String),
}

struct SandboxStoreState {
    limits: StoreLimits,
}

#[derive(Clone)]
pub struct WasmExecutor {
    engine: Engine,
}

impl std::fmt::Debug for WasmExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WasmExecutor")
            .finish_non_exhaustive()
    }
}

impl WasmExecutor {
    pub fn new() -> Result<Self, SandboxError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine =
            Engine::new(&config).map_err(|error| SandboxError::InvalidPolicy(error.to_string()))?;
        Ok(Self { engine })
    }

    /// Executa WASM fora das threads assíncronas, preservando o contrato síncrono.
    pub async fn execute_async(
        &self,
        wasm: Vec<u8>,
        required_capabilities: Vec<Capability>,
        policy: SandboxPolicy,
    ) -> Result<SandboxResult, SandboxError> {
        let executor = self.clone();
        tokio::task::spawn_blocking(move || {
            executor.execute(&wasm, &required_capabilities, &policy)
        })
        .await
        .map_err(|error| SandboxError::Execution(format!("thread do sandbox terminou: {error}")))?
    }

    pub fn execute(
        &self,
        wasm: &[u8],
        required_capabilities: &[Capability],
        policy: &SandboxPolicy,
    ) -> Result<SandboxResult, SandboxError> {
        policy.validate(required_capabilities)?;
        if policy.max_fuel == 0 || policy.max_elapsed_ms == 0 {
            return Err(SandboxError::InvalidPolicy(
                "max_fuel e max_elapsed_ms precisam ser maiores que zero".to_owned(),
            ));
        }
        let module = Module::new(&self.engine, wasm)
            .map_err(|error| SandboxError::InvalidModule(error.to_string()))?;
        if module.imports().next().is_some() {
            return Err(SandboxError::HostImportsDenied);
        }
        let memory_limit = usize::try_from(policy.max_memory_bytes).map_err(|_| {
            SandboxError::InvalidPolicy("max_memory_bytes não cabe em usize".to_owned())
        })?;
        let limits = StoreLimitsBuilder::new().memory_size(memory_limit).build();
        let mut store = Store::new(&self.engine, SandboxStoreState { limits });
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(policy.max_fuel)
            .map_err(|error| SandboxError::InvalidPolicy(error.to_string()))?;
        store.set_epoch_deadline(1);
        let instance = Instance::new(&mut store, &module, &[])
            .map_err(|error| SandboxError::Execution(error.to_string()))?;
        let entry = instance
            .get_typed_func::<(), i32>(&mut store, "run")
            .map_err(|error| SandboxError::MissingEntryPoint(error.to_string()))?;
        let (cancel_tx, cancel_rx) = sync_channel(0);
        let timed_out = Arc::new(AtomicBool::new(false));
        let timer_timed_out = Arc::clone(&timed_out);
        let timer_engine = self.engine.clone();
        let max_elapsed_ms = policy.max_elapsed_ms;
        let timer = thread::spawn(move || {
            if cancel_rx
                .recv_timeout(Duration::from_millis(max_elapsed_ms))
                .is_err()
            {
                timer_timed_out.store(true, Ordering::Release);
                timer_engine.increment_epoch();
            }
        });
        let execution = entry.call(&mut store, ());
        let _ = cancel_tx.send(());
        let _ = timer.join();
        if timed_out.load(Ordering::Acquire) {
            return Err(SandboxError::Execution(
                "limite de tempo excedido".to_owned(),
            ));
        }
        let exit_code = execution.map_err(|error| SandboxError::Execution(error.to_string()))?;
        let fuel_remaining = store
            .get_fuel()
            .map_err(|error| SandboxError::Execution(error.to_string()))?;
        Ok(SandboxResult {
            exit_code,
            fuel_consumed: policy.max_fuel.saturating_sub(fuel_remaining),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use shaka_core::Capability;

    #[test]
    fn executes_pure_module() {
        let executor = WasmExecutor::new().expect("executor");
        let wasm = wat::parse_str("(module (func (export \"run\") (result i32) i32.const 7))")
            .expect("wat");
        let result = executor
            .execute(&wasm, &[], &SandboxPolicy::default())
            .expect("execution");
        assert_eq!(result.exit_code, 7);
        assert!(result.fuel_consumed > 0);
    }

    #[test]
    fn memory_policy_rejects_zero_and_above_host_maximum() {
        let zero = SandboxPolicy {
            max_memory_bytes: 0,
            ..SandboxPolicy::default()
        };
        assert!(matches!(
            zero.validate(&[]),
            Err(SandboxError::InvalidPolicy(message)) if message.contains("max_memory_bytes")
        ));

        let above_maximum = SandboxPolicy {
            max_memory_bytes: MAX_SANDBOX_MEMORY_BYTES + 1,
            ..SandboxPolicy::default()
        };
        assert!(matches!(
            above_maximum.validate(&[]),
            Err(SandboxError::InvalidPolicy(message)) if message.contains("max_memory_bytes")
        ));
    }

    #[test]
    fn legacy_memory_policy_deserializes_to_safe_default() {
        let policy: SandboxPolicy = serde_json::from_str(
            r#"{
                "max_fuel": 100000,
                "max_elapsed_ms": 1000,
                "allow_network": false,
                "allow_filesystem": false
            }"#,
        )
        .expect("legacy sandbox policy");
        assert_eq!(policy.max_memory_bytes, 16 * 1024 * 1024);
    }

    #[test]
    fn guest_memory_growth_is_bounded_by_policy() {
        let executor = WasmExecutor::new().expect("executor");
        let wasm = wat::parse_str(
            r#"(module
                (memory 1)
                (func (export "run") (result i32)
                    i32.const 1
                    memory.grow))"#,
        )
        .expect("wat");
        let policy = SandboxPolicy {
            max_memory_bytes: 64 * 1024,
            ..SandboxPolicy::default()
        };
        let result = executor
            .execute(&wasm, &[], &policy)
            .expect("memory.grow should fail inside guest, not trap the host");
        assert_eq!(result.exit_code, -1);
    }

    #[test]
    fn rejects_host_imports() {
        let executor = WasmExecutor::new().expect("executor");
        let wasm = wat::parse_str(
            "(module (import \"host\" \"danger\" (func)) (func (export \"run\") (result i32) i32.const 0))",
        )
        .expect("wat");
        let result = executor.execute(&wasm, &[], &SandboxPolicy::default());
        assert!(matches!(result, Err(SandboxError::HostImportsDenied)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_execution_does_not_block_executor() {
        let executor = WasmExecutor::new().expect("executor");
        let wasm = wat::parse_str(
            r#"(module (func (export "run") (result i32) (loop br 0) i32.const 0))"#,
        )
        .expect("wat");
        let policy = SandboxPolicy {
            max_fuel: u64::MAX,
            max_elapsed_ms: 150,
            ..SandboxPolicy::default()
        };
        let started = std::time::Instant::now();
        let execution = tokio::spawn({
            let executor = executor.clone();
            async move { executor.execute_async(wasm, Vec::new(), policy).await }
        });
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let tick_elapsed_ms = started.elapsed().as_millis();
        let result = execution.await.expect("join");
        assert!(
            matches!(result, Err(SandboxError::Execution(message)) if message.contains("tempo"))
        );
        assert!(tick_elapsed_ms < 100, "ticker atrasou {tick_elapsed_ms} ms");
    }

    #[test]
    fn interrupts_long_running_module() {
        let executor = WasmExecutor::new().expect("executor");
        let wasm = wat::parse_str(
            r#"(module (func (export "run") (result i32) (loop br 0) i32.const 0))"#,
        )
        .expect("wat");
        let policy = SandboxPolicy {
            max_fuel: u64::MAX,
            max_elapsed_ms: 10,
            ..SandboxPolicy::default()
        };
        let result = executor.execute(&wasm, &[], &policy);
        assert!(
            matches!(result, Err(SandboxError::Execution(message)) if message.contains("tempo"))
        );
    }

    #[test]
    fn denies_network_by_default() {
        let executor = WasmExecutor::new().expect("executor");
        let wasm = wat::parse_str("(module (func (export \"run\") (result i32) i32.const 0))")
            .expect("wat");
        let result = executor.execute(&wasm, &[Capability::Network], &SandboxPolicy::default());
        assert!(matches!(
            result,
            Err(SandboxError::CapabilityDenied(Capability::Network))
        ));
    }
}
