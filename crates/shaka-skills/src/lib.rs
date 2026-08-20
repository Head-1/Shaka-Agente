//! Governança de skills: nenhuma skill vira ativa sem aprovação humana.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use shaka_core::{Capability, OperatorId, SkillManifest, SkillStatus};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SkillError {
    #[error("skill já existe: {0}")]
    AlreadyExists(String),
    #[error("skill não encontrada: {0}")]
    NotFound(String),
    #[error("transição não permitida: {from:?} -> {to:?}")]
    InvalidTransition { from: SkillStatus, to: SkillStatus },
    #[error("aprovação inválida: {0}")]
    InvalidApproval(String),
    #[error("permissão excessiva para skill: {0:?}")]
    ExcessivePermission(Capability),
    #[error("erro de arquivo: {0}")]
    Io(#[from] std::io::Error),
    #[error("erro de serialização: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub operator_id: OperatorId,
    pub approved_at: DateTime<Utc>,
    pub artifact_sha256: String,
    #[serde(default)]
    pub artifact_path: Option<PathBuf>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillRecord {
    pub manifest: SkillManifest,
    pub approval: Option<ApprovalRecord>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: HashMap<String, SkillRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveSkillArtifact {
    pub name: String,
    pub version: String,
    pub description: String,
    pub permissions: Vec<Capability>,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub artifact_path: PathBuf,
    pub artifact_sha256: String,
}

impl SkillRegistry {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SkillError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        let skills = serde_json::from_str::<HashMap<String, SkillRecord>>(&content)?;
        Ok(Self { skills })
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), SkillError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(&self.skills)?;
        let temporary = temporary_path(path);
        let mut file = File::create(&temporary)?;
        file.write_all(format!("{content}\n").as_bytes())?;
        file.sync_all()?;
        set_restricted_permissions(&temporary)?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }

    pub fn approve_artifact(
        &mut self,
        name: &str,
        operator_id: OperatorId,
        artifact_path: impl AsRef<Path>,
        reason: impl Into<String>,
    ) -> Result<SkillRecord, SkillError> {
        let canonical_path = artifact_path.as_ref().canonicalize()?;
        let actual_sha256 = sha256_file(&canonical_path)?;
        self.approve_with_artifact(name, operator_id, actual_sha256, canonical_path, reason)
    }

    pub fn register_candidate(&mut self, manifest: SkillManifest) -> Result<(), SkillError> {
        if self.skills.contains_key(&manifest.name) {
            return Err(SkillError::AlreadyExists(manifest.name));
        }
        if manifest.status != SkillStatus::Candidate {
            return Err(SkillError::InvalidTransition {
                from: manifest.status.clone(),
                to: SkillStatus::Candidate,
            });
        }
        if manifest.permissions.contains(&Capability::Network)
            && manifest
                .permissions
                .contains(&Capability::ExternalMessaging)
        {
            return Err(SkillError::ExcessivePermission(
                Capability::ExternalMessaging,
            ));
        }
        let now = Utc::now();
        self.skills.insert(
            manifest.name.clone(),
            SkillRecord {
                manifest,
                approval: None,
                created_at: now,
                updated_at: now,
            },
        );
        Ok(())
    }

    pub fn approve(
        &mut self,
        name: &str,
        operator_id: OperatorId,
        artifact_sha256: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<SkillRecord, SkillError> {
        let record = self
            .skills
            .get_mut(name)
            .ok_or_else(|| SkillError::NotFound(name.to_owned()))?;
        if record.manifest.status != SkillStatus::Candidate {
            return Err(SkillError::InvalidTransition {
                from: record.manifest.status.clone(),
                to: SkillStatus::Active,
            });
        }
        let artifact_sha256 = artifact_sha256.into();
        if artifact_sha256.len() != 64 || !artifact_sha256.chars().all(|ch| ch.is_ascii_hexdigit())
        {
            return Err(SkillError::InvalidApproval(
                "artifact_sha256 deve conter 64 caracteres hexadecimais".to_owned(),
            ));
        }
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(SkillError::InvalidApproval(
                "a aprovação precisa registrar uma justificativa".to_owned(),
            ));
        }
        record.manifest.status = SkillStatus::Active;
        record.manifest.artifact_sha256 = Some(artifact_sha256.clone());
        record.approval = Some(ApprovalRecord {
            operator_id,
            approved_at: Utc::now(),
            artifact_sha256,
            artifact_path: None,
            reason,
        });
        record.updated_at = Utc::now();
        Ok(record.clone())
    }

    fn approve_with_artifact(
        &mut self,
        name: &str,
        operator_id: OperatorId,
        artifact_sha256: String,
        artifact_path: PathBuf,
        reason: impl Into<String>,
    ) -> Result<SkillRecord, SkillError> {
        let record = self
            .skills
            .get_mut(name)
            .ok_or_else(|| SkillError::NotFound(name.to_owned()))?;
        if record.manifest.status != SkillStatus::Candidate {
            return Err(SkillError::InvalidTransition {
                from: record.manifest.status.clone(),
                to: SkillStatus::Active,
            });
        }
        if artifact_sha256.len() != 64 || !artifact_sha256.chars().all(|ch| ch.is_ascii_hexdigit())
        {
            return Err(SkillError::InvalidApproval(
                "artifact_sha256 deve conter 64 caracteres hexadecimais".to_owned(),
            ));
        }
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(SkillError::InvalidApproval(
                "a aprovação precisa registrar uma justificativa".to_owned(),
            ));
        }
        record.manifest.status = SkillStatus::Active;
        record.manifest.artifact_sha256 = Some(artifact_sha256.clone());
        record.approval = Some(ApprovalRecord {
            operator_id,
            approved_at: Utc::now(),
            artifact_sha256,
            artifact_path: Some(artifact_path),
            reason,
        });
        record.updated_at = Utc::now();
        Ok(record.clone())
    }

    pub fn revoke(&mut self, name: &str) -> Result<SkillRecord, SkillError> {
        let record = self
            .skills
            .get_mut(name)
            .ok_or_else(|| SkillError::NotFound(name.to_owned()))?;
        if record.manifest.status != SkillStatus::Active {
            return Err(SkillError::InvalidTransition {
                from: record.manifest.status.clone(),
                to: SkillStatus::Revoked,
            });
        }
        record.manifest.status = SkillStatus::Revoked;
        record.updated_at = Utc::now();
        Ok(record.clone())
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SkillRecord> {
        self.skills.get(name)
    }

    #[must_use]
    pub fn active_skills(&self) -> Vec<&SkillRecord> {
        self.skills
            .values()
            .filter(|record| record.manifest.status == SkillStatus::Active)
            .collect()
    }

    #[must_use]
    pub fn active_artifacts(&self) -> Vec<ActiveSkillArtifact> {
        self.skills
            .values()
            .filter(|record| record.manifest.status == SkillStatus::Active)
            .filter_map(|record| {
                let approval = record.approval.as_ref()?;
                let artifact_path = approval.artifact_path.clone()?;
                let artifact_sha256 = record.manifest.artifact_sha256.clone()?;
                Some(ActiveSkillArtifact {
                    name: record.manifest.name.clone(),
                    version: record.manifest.version.clone(),
                    description: record.manifest.description.clone(),
                    permissions: record.manifest.permissions.clone(),
                    input_schema: record.manifest.input_schema.clone(),
                    output_schema: record.manifest.output_schema.clone(),
                    artifact_path,
                    artifact_sha256,
                })
            })
            .collect()
    }
}

pub fn sha256_file(path: impl AsRef<Path>) -> Result<String, SkillError> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn temporary_path(path: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let suffix = format!("tmp-{}-{nanos}", std::process::id());
    path.with_extension(suffix)
}

fn set_restricted_permissions(path: &Path) -> Result<(), SkillError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn candidate() -> SkillManifest {
        SkillManifest {
            name: "demo".to_owned(),
            version: "0.1.0".to_owned(),
            description: "skill de teste".to_owned(),
            permissions: vec![Capability::MemoryWrite],
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
            status: SkillStatus::Candidate,
            artifact_sha256: None,
        }
    }

    #[test]
    fn candidate_cannot_become_active_without_approval() {
        let mut registry = SkillRegistry::default();
        registry.register_candidate(candidate()).unwrap();
        assert_eq!(
            registry.get("demo").map(|item| &item.manifest.status),
            Some(&SkillStatus::Candidate)
        );
    }

    #[test]
    fn approval_requires_sha256_and_reason() {
        let mut registry = SkillRegistry::default();
        registry.register_candidate(candidate()).unwrap();
        let operator = OperatorId::new("operator").unwrap();
        assert!(registry.approve("demo", operator, "bad", "ok").is_err());
    }

    #[test]
    fn artifact_approval_exposes_verified_artifact() {
        let mut registry = SkillRegistry::default();
        registry.register_candidate(candidate()).unwrap();
        let path = std::env::temp_dir().join(format!(
            "shaka-skill-{}-{}.wasm",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(&path, b"wasm-fixture").unwrap();
        let operator = OperatorId::new("reviewer").unwrap();
        registry
            .approve_artifact("demo", operator, &path, "artefato testado")
            .unwrap();
        let artifacts = registry.active_artifacts();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_path, path.canonicalize().unwrap());
        assert_eq!(artifacts[0].artifact_sha256, sha256_file(&path).unwrap());
        let _ = std::fs::remove_file(path);
    }
}
