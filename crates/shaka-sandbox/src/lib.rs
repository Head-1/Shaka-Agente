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
use wasmtime::{Config, Engine, Instance, Module, Store};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxPolicy {
    pub max_fuel: u64,
    pub max_elapsed_ms: u64,
    pub allow_network: bool,
    pub allow_filesystem: bool,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            max_fuel: 100_000,
            max_elapsed_ms: 1_000,
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
        let mut store = Store::new(&self.engine, ());
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
    fn rejects_host_imports() {
        let executor = WasmExecutor::new().expect("executor");
        let wasm = wat::parse_str(
            "(module (import \"host\" \"danger\" (func)) (func (export \"run\") (result i32) i32.const 0))",
        )
        .expect("wat");
        let result = executor.execute(&wasm, &[], &SandboxPolicy::default());
        assert!(matches!(result, Err(SandboxError::HostImportsDenied)));
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
