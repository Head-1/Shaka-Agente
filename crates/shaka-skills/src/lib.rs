//! Governança de skills: nenhuma skill vira ativa ou executável sem aprovação humana verificável.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
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

/// Versão do protocolo de atestação de aprovação humana.
pub const APPROVAL_PROTOCOL_V1: &str = "shaka-skill-approval-v1";

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
    #[error("aprovação assinada é obrigatória para execução da skill")]
    UnsignedApproval,
    #[error("assinatura da aprovação é inválida")]
    InvalidSignature,
    #[error("chave de assinatura inválida: {0}")]
    InvalidKey(String),
    #[error("chave não confiável: {0}")]
    UntrustedKey(String),
    #[error("chave revogada: {0}")]
    RevokedKey(String),
    #[error("hash do artefato não corresponde à aprovação")]
    ArtifactHashMismatch,
    #[error("artefato aprovado não possui caminho verificável")]
    MissingArtifactPath,
    #[error("permissões inseguras no arquivo de chave: {0}")]
    InsecureKeyFile(PathBuf),
    #[error("permissão excessiva para skill: {0:?}")]
    ExcessivePermission(Capability),
    #[error("arquivo de chave já existe: {0}")]
    KeyFileAlreadyExists(PathBuf),
    #[error("chave confiável já existe: {0}")]
    KeyAlreadyExists(String),
    #[error("chave confiável não encontrada: {0}")]
    KeyNotFound(String),
    #[error("erro de arquivo: {0}")]
    Io(#[from] std::io::Error),
    #[error("erro de serialização: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalAttestation {
    pub protocol: String,
    pub key_id: String,
    pub public_key_hex: String,
    pub signature_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub operator_id: OperatorId,
    pub approved_at: DateTime<Utc>,
    pub artifact_sha256: String,
    #[serde(default)]
    pub artifact_path: Option<PathBuf>,
    pub reason: String,
    /// Atestação Ed25519; ausente somente em registros legados não executáveis.
    #[serde(default)]
    pub attestation: Option<ApprovalAttestation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillRecord {
    pub manifest: SkillManifest,
    pub approval: Option<ApprovalRecord>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustedKey {
    pub key_id: String,
    pub public_key_hex: String,
    pub description: String,
    pub added_by: OperatorId,
    pub added_at: DateTime<Utc>,
    #[serde(default)]
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustStore {
    #[serde(default)]
    keys: HashMap<String, TrustedKey>,
}

impl TrustStore {
    /// Carrega um trust store; arquivo ausente significa zero chaves confiáveis.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SkillError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    /// Persiste o trust store com escrita atômica e permissões restritas.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), SkillError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        let temporary = temporary_path(path);
        let mut file = File::create(&temporary)?;
        file.write_all(format!("{content}\n").as_bytes())?;
        file.sync_all()?;
        set_restricted_permissions(&temporary)?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }

    /// Adiciona uma chave pública confiável após uma decisão explícita do operador.
    pub fn add_key(
        &mut self,
        key_id: impl Into<String>,
        public_key_hex: impl Into<String>,
        description: impl Into<String>,
        added_by: OperatorId,
    ) -> Result<TrustedKey, SkillError> {
        let key_id = validate_key_id(key_id.into())?;
        let public_key_hex = normalize_public_key(public_key_hex.into())?;
        let description = description.into();
        if description.trim().is_empty() {
            return Err(SkillError::InvalidKey(
                "description não pode ser vazia".to_owned(),
            ));
        }
        if self.keys.contains_key(&key_id) {
            return Err(SkillError::KeyAlreadyExists(key_id));
        }
        let record = TrustedKey {
            key_id: key_id.clone(),
            public_key_hex,
            description,
            added_by,
            added_at: Utc::now(),
            revoked_at: None,
        };
        self.keys.insert(key_id, record.clone());
        Ok(record)
    }

    /// Revoga uma chave, impedindo novas execuções assinadas por ela.
    pub fn revoke_key(&mut self, key_id: &str) -> Result<TrustedKey, SkillError> {
        let record = self
            .keys
            .get_mut(key_id)
            .ok_or_else(|| SkillError::KeyNotFound(key_id.to_owned()))?;
        record.revoked_at = Some(Utc::now());
        Ok(record.clone())
    }

    #[must_use]
    pub fn get(&self, key_id: &str) -> Option<&TrustedKey> {
        self.keys.get(key_id)
    }

    #[must_use]
    pub fn list(&self) -> Vec<&TrustedKey> {
        let mut keys: Vec<_> = self.keys.values().collect();
        keys.sort_by(|left, right| left.key_id.cmp(&right.key_id));
        keys
    }

    /// Verifica uma atestação contra a chave confiável e o payload canônico.
    pub fn verify_attestation(
        &self,
        name: &str,
        version: &str,
        operator_id: &OperatorId,
        artifact_sha256: &str,
        reason: &str,
        attestation: &ApprovalAttestation,
    ) -> Result<(), SkillError> {
        if attestation.protocol != APPROVAL_PROTOCOL_V1 {
            return Err(SkillError::InvalidSignature);
        }
        let trusted = self
            .keys
            .get(&attestation.key_id)
            .ok_or_else(|| SkillError::UntrustedKey(attestation.key_id.clone()))?;
        if trusted.revoked_at.is_some() {
            return Err(SkillError::RevokedKey(attestation.key_id.clone()));
        }
        if trusted.public_key_hex != attestation.public_key_hex {
            return Err(SkillError::UntrustedKey(attestation.key_id.clone()));
        }
        let verifying_key = verifying_key_from_hex(&attestation.public_key_hex)?;
        let signature_bytes =
            hex::decode(&attestation.signature_hex).map_err(|_| SkillError::InvalidSignature)?;
        let signature =
            Signature::from_slice(&signature_bytes).map_err(|_| SkillError::InvalidSignature)?;
        let payload = approval_payload(name, version, operator_id, artifact_sha256, reason);
        verifying_key
            .verify(&payload, &signature)
            .map_err(|_| SkillError::InvalidSignature)
    }
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
    pub attestation: ApprovalAttestation,
    pub approval_operator_id: OperatorId,
    pub approval_reason: String,
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

    /// Aprova um artefato e cria uma atestação Ed25519 vinculada ao conteúdo exato.
    pub fn approve_signed_artifact(
        &mut self,
        name: &str,
        operator_id: OperatorId,
        artifact_path: impl AsRef<Path>,
        key_id: impl Into<String>,
        signing_key: &SigningKey,
        reason: impl Into<String>,
    ) -> Result<SkillRecord, SkillError> {
        let canonical_path = artifact_path.as_ref().canonicalize()?;
        let artifact_sha256 = sha256_file(&canonical_path)?;
        let record = self
            .skills
            .get(name)
            .ok_or_else(|| SkillError::NotFound(name.to_owned()))?;
        let key_id = validate_key_id(key_id.into())?;
        let reason = validate_reason(reason.into())?;
        let attestation = sign_approval(
            name,
            &record.manifest.version,
            &operator_id,
            &artifact_sha256,
            &reason,
            key_id,
            signing_key,
        );
        self.approve_with_attestation(
            name,
            operator_id,
            artifact_sha256,
            canonical_path,
            reason,
            attestation,
        )
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

    /// Compatibilidade de migração: cria uma aprovação legada, não executável no caminho verificado.
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
        ensure_candidate(record)?;
        let artifact_sha256 = validate_hash(artifact_sha256.into())?;
        let reason = validate_reason(reason.into())?;
        record.manifest.status = SkillStatus::Active;
        record.manifest.artifact_sha256 = Some(artifact_sha256.clone());
        record.approval = Some(ApprovalRecord {
            operator_id,
            approved_at: Utc::now(),
            artifact_sha256,
            artifact_path: None,
            reason,
            attestation: None,
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
        ensure_candidate(record)?;
        let artifact_sha256 = validate_hash(artifact_sha256)?;
        let reason = validate_reason(reason.into())?;
        record.manifest.status = SkillStatus::Active;
        record.manifest.artifact_sha256 = Some(artifact_sha256.clone());
        record.approval = Some(ApprovalRecord {
            operator_id,
            approved_at: Utc::now(),
            artifact_sha256,
            artifact_path: Some(artifact_path),
            reason,
            attestation: None,
        });
        record.updated_at = Utc::now();
        Ok(record.clone())
    }

    fn approve_with_attestation(
        &mut self,
        name: &str,
        operator_id: OperatorId,
        artifact_sha256: String,
        artifact_path: PathBuf,
        reason: String,
        attestation: ApprovalAttestation,
    ) -> Result<SkillRecord, SkillError> {
        let record = self
            .skills
            .get_mut(name)
            .ok_or_else(|| SkillError::NotFound(name.to_owned()))?;
        ensure_candidate(record)?;
        let artifact_sha256 = validate_hash(artifact_sha256)?;
        record.manifest.status = SkillStatus::Active;
        record.manifest.artifact_sha256 = Some(artifact_sha256.clone());
        record.approval = Some(ApprovalRecord {
            operator_id,
            approved_at: Utc::now(),
            artifact_sha256,
            artifact_path: Some(artifact_path),
            reason,
            attestation: Some(attestation),
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

    /// Retorna apenas skills ativas cujo artefato, assinatura e chave passam por nova verificação.
    pub fn active_verified_artifacts(
        &self,
        trust_store: &TrustStore,
    ) -> Result<Vec<ActiveSkillArtifact>, SkillError> {
        let mut artifacts = Vec::new();
        for record in self
            .skills
            .values()
            .filter(|record| record.manifest.status == SkillStatus::Active)
        {
            let approval = record
                .approval
                .as_ref()
                .ok_or(SkillError::UnsignedApproval)?;
            let attestation = approval
                .attestation
                .as_ref()
                .ok_or(SkillError::UnsignedApproval)?;
            let artifact_path = approval
                .artifact_path
                .clone()
                .ok_or(SkillError::MissingArtifactPath)?;
            let actual_sha256 = sha256_file(&artifact_path)?;
            if actual_sha256 != approval.artifact_sha256
                || Some(actual_sha256.clone()) != record.manifest.artifact_sha256
            {
                return Err(SkillError::ArtifactHashMismatch);
            }
            trust_store.verify_attestation(
                &record.manifest.name,
                &record.manifest.version,
                &approval.operator_id,
                &approval.artifact_sha256,
                &approval.reason,
                attestation,
            )?;
            artifacts.push(ActiveSkillArtifact {
                name: record.manifest.name.clone(),
                version: record.manifest.version.clone(),
                description: record.manifest.description.clone(),
                permissions: record.manifest.permissions.clone(),
                input_schema: record.manifest.input_schema.clone(),
                output_schema: record.manifest.output_schema.clone(),
                artifact_path,
                artifact_sha256: approval.artifact_sha256.clone(),
                attestation: attestation.clone(),
                approval_operator_id: approval.operator_id.clone(),
                approval_reason: approval.reason.clone(),
            });
        }
        artifacts.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(artifacts)
    }

    /// Compatibilidade de inspeção; não deve ser usado para montar ferramentas executáveis.
    #[must_use]
    pub fn active_artifacts(&self) -> Vec<ActiveSkillArtifact> {
        self.skills
            .values()
            .filter(|record| record.manifest.status == SkillStatus::Active)
            .filter_map(|record| {
                let approval = record.approval.as_ref()?;
                let artifact_path = approval.artifact_path.clone()?;
                let artifact_sha256 = record.manifest.artifact_sha256.clone()?;
                let attestation = approval.attestation.clone()?;
                Some(ActiveSkillArtifact {
                    name: record.manifest.name.clone(),
                    version: record.manifest.version.clone(),
                    description: record.manifest.description.clone(),
                    permissions: record.manifest.permissions.clone(),
                    input_schema: record.manifest.input_schema.clone(),
                    output_schema: record.manifest.output_schema.clone(),
                    artifact_path,
                    artifact_sha256,
                    attestation,
                    approval_operator_id: approval.operator_id.clone(),
                    approval_reason: approval.reason.clone(),
                })
            })
            .collect()
    }
}

/// Assina a decisão de aprovação sobre um payload canônico e versionado.
#[must_use]
pub fn sign_approval(
    name: &str,
    version: &str,
    operator_id: &OperatorId,
    artifact_sha256: &str,
    reason: &str,
    key_id: String,
    signing_key: &SigningKey,
) -> ApprovalAttestation {
    let payload = approval_payload(name, version, operator_id, artifact_sha256, reason);
    let signature = signing_key.sign(&payload);
    ApprovalAttestation {
        protocol: APPROVAL_PROTOCOL_V1.to_owned(),
        key_id,
        public_key_hex: hex::encode(signing_key.verifying_key().to_bytes()),
        signature_hex: hex::encode(signature.to_bytes()),
    }
}

/// Persiste uma chave Ed25519 como seed hexadecimal e aplica permissões restritas.
pub fn save_signing_key(
    path: impl AsRef<Path>,
    signing_key: &SigningKey,
) -> Result<(), SkillError> {
    let path = path.as_ref();
    if path.exists() {
        return Err(SkillError::KeyFileAlreadyExists(path.to_owned()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = temporary_path(path);
    let mut file = File::create(&temporary)?;
    file.write_all(hex::encode(signing_key.to_bytes()).as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    set_restricted_permissions(&temporary)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// Retorna a chave pública Ed25519 em hexadecimal canônico.
#[must_use]
pub fn public_key_hex(signing_key: &SigningKey) -> String {
    hex::encode(signing_key.verifying_key().to_bytes())
}

/// Lê uma chave Ed25519 a partir de um arquivo hexadecimal com 32 bytes de seed.
pub fn load_signing_key(path: impl AsRef<Path>) -> Result<SigningKey, SkillError> {
    let path = path.as_ref();
    ensure_restricted_file_permissions(path)?;
    let content = std::fs::read_to_string(path)?;
    let bytes = hex::decode(content.trim()).map_err(|_| {
        SkillError::InvalidKey("o arquivo deve conter 64 caracteres hexadecimais".to_owned())
    })?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
        SkillError::InvalidKey("a seed Ed25519 deve conter exatamente 32 bytes".to_owned())
    })?;
    Ok(SigningKey::from_bytes(&bytes))
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

fn approval_payload(
    name: &str,
    version: &str,
    operator_id: &OperatorId,
    artifact_sha256: &str,
    reason: &str,
) -> Vec<u8> {
    let mut payload = Vec::from(APPROVAL_PROTOCOL_V1.as_bytes());
    payload.push(0);
    for (label, value) in [
        ("name", name),
        ("version", version),
        ("operator_id", operator_id.0.as_str()),
        ("artifact_sha256", artifact_sha256),
        ("reason", reason),
    ] {
        payload.extend_from_slice(label.as_bytes());
        payload.push(b'=');
        payload.extend_from_slice(value.len().to_string().as_bytes());
        payload.push(b':');
        payload.extend_from_slice(value.as_bytes());
        payload.push(0);
    }
    payload
}

fn ensure_candidate(record: &SkillRecord) -> Result<(), SkillError> {
    if record.manifest.status != SkillStatus::Candidate {
        return Err(SkillError::InvalidTransition {
            from: record.manifest.status.clone(),
            to: SkillStatus::Active,
        });
    }
    Ok(())
}

fn validate_hash(value: impl AsRef<str>) -> Result<String, SkillError> {
    let value = value.as_ref();
    if value.len() != 64 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(SkillError::InvalidApproval(
            "artifact_sha256 deve conter 64 caracteres hexadecimais".to_owned(),
        ));
    }
    Ok(value.to_ascii_lowercase())
}

fn validate_reason(value: String) -> Result<String, SkillError> {
    if value.trim().is_empty() {
        return Err(SkillError::InvalidApproval(
            "a aprovação precisa registrar uma justificativa".to_owned(),
        ));
    }
    if value.len() > 4096 {
        return Err(SkillError::InvalidApproval(
            "a justificativa não pode exceder 4096 bytes".to_owned(),
        ));
    }
    Ok(value)
}

fn validate_key_id(value: String) -> Result<String, SkillError> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_whitespace) {
        return Err(SkillError::InvalidKey(
            "key_id deve ser não vazio, ter até 128 caracteres e não conter espaços".to_owned(),
        ));
    }
    Ok(value)
}

fn normalize_public_key(value: impl AsRef<str>) -> Result<String, SkillError> {
    let key = verifying_key_from_hex(value.as_ref())?;
    Ok(hex::encode(key.to_bytes()))
}

fn verifying_key_from_hex(value: &str) -> Result<VerifyingKey, SkillError> {
    let bytes = hex::decode(value)
        .map_err(|_| SkillError::InvalidKey("chave pública hex inválida".to_owned()))?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
        SkillError::InvalidKey("chave pública Ed25519 deve conter 32 bytes".to_owned())
    })?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| SkillError::InvalidKey("chave pública Ed25519 inválida".to_owned()))
}

fn temporary_path(path: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let suffix = format!("tmp-{}-{nanos}", std::process::id());
    path.with_extension(suffix)
}

fn ensure_restricted_file_permissions(path: &Path) -> Result<(), SkillError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(SkillError::InsecureKeyFile(path.to_owned()));
        }
    }
    Ok(())
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

    fn fixture_path(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "shaka-skill-{label}-{}-{}.wasm",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(&path, b"wasm-fixture").unwrap();
        path
    }

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7_u8; 32])
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
    fn legacy_hash_approval_is_not_executable() {
        let mut registry = SkillRegistry::default();
        registry.register_candidate(candidate()).unwrap();
        let path = fixture_path("legacy");
        let hash = sha256_file(&path).unwrap();
        registry
            .approve_artifact(
                "demo",
                OperatorId::new("reviewer").unwrap(),
                &path,
                "legado",
            )
            .unwrap();
        assert!(matches!(
            registry.active_verified_artifacts(&TrustStore::default()),
            Err(SkillError::UnsignedApproval)
        ));
        assert_eq!(
            registry
                .get("demo")
                .unwrap()
                .approval
                .as_ref()
                .unwrap()
                .artifact_sha256,
            hash
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn signed_approval_requires_trusted_key_and_verifies_artifact() {
        let mut registry = SkillRegistry::default();
        registry.register_candidate(candidate()).unwrap();
        let path = fixture_path("signed");
        let key = signing_key();
        let operator = OperatorId::new("reviewer").unwrap();
        registry
            .approve_signed_artifact(
                "demo",
                operator.clone(),
                &path,
                "review-key",
                &key,
                "artefato testado em sandbox",
            )
            .unwrap();
        assert!(matches!(
            registry.active_verified_artifacts(&TrustStore::default()),
            Err(SkillError::UntrustedKey(_))
        ));
        let mut trust = TrustStore::default();
        trust
            .add_key(
                "review-key",
                hex::encode(key.verifying_key().to_bytes()),
                "chave de revisão",
                operator,
            )
            .unwrap();
        let artifacts = registry.active_verified_artifacts(&trust).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_path, path.canonicalize().unwrap());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn changed_reason_invalidates_signature() {
        let key = signing_key();
        let operator = OperatorId::new("reviewer").unwrap();
        let attestation = sign_approval(
            "demo",
            "0.1.0",
            &operator,
            &"a".repeat(64),
            "motivo original",
            "review-key".to_owned(),
            &key,
        );
        let mut trust = TrustStore::default();
        trust
            .add_key(
                "review-key",
                hex::encode(key.verifying_key().to_bytes()),
                "chave",
                operator.clone(),
            )
            .unwrap();
        assert!(matches!(
            trust.verify_attestation(
                "demo",
                "0.1.0",
                &operator,
                &"a".repeat(64),
                "motivo adulterado",
                &attestation,
            ),
            Err(SkillError::InvalidSignature)
        ));
    }

    #[test]
    fn signing_key_round_trips_with_restricted_file() {
        let path = std::env::temp_dir().join(format!(
            "shaka-signing-key-{}-{}.key",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let key = signing_key();
        save_signing_key(&path, &key).unwrap();
        let loaded = load_signing_key(&path).unwrap();
        assert_eq!(public_key_hex(&loaded), public_key_hex(&key));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn changed_artifact_blocks_execution() {
        let mut registry = SkillRegistry::default();
        registry.register_candidate(candidate()).unwrap();
        let path = fixture_path("tampered");
        let key = signing_key();
        let operator = OperatorId::new("reviewer").unwrap();
        registry
            .approve_signed_artifact(
                "demo",
                operator.clone(),
                &path,
                "review-key",
                &key,
                "artefato testado",
            )
            .unwrap();
        std::fs::write(&path, b"tampered-artifact").unwrap();
        let mut trust = TrustStore::default();
        trust
            .add_key("review-key", public_key_hex(&key), "chave", operator)
            .unwrap();
        assert!(matches!(
            registry.active_verified_artifacts(&trust),
            Err(SkillError::ArtifactHashMismatch)
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn adversarial_inputs_are_rejected_without_panic() {
        let operator = OperatorId::new("reviewer").unwrap();
        let mut trust = TrustStore::default();
        let public_key = public_key_hex(&signing_key());
        for key_id in ["", "has whitespace", "x".repeat(129).as_str()] {
            assert!(
                trust
                    .add_key(key_id, public_key.clone(), "fixture", operator.clone(),)
                    .is_err()
            );
        }
        let key = signing_key();
        trust
            .add_key(
                "review-key",
                public_key_hex(&key),
                "fixture",
                operator.clone(),
            )
            .unwrap();
        for signature_hex in ["", "00", "not-hex", &"a".repeat(2048)] {
            let attestation = ApprovalAttestation {
                protocol: APPROVAL_PROTOCOL_V1.to_owned(),
                key_id: "review-key".to_owned(),
                public_key_hex: public_key_hex(&key),
                signature_hex: signature_hex.to_owned(),
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                trust.verify_attestation(
                    "demo",
                    "0.1.0",
                    &operator,
                    &"a".repeat(64),
                    "fixture",
                    &attestation,
                )
            }));
            assert!(result.is_ok());
            assert!(result.unwrap().is_err());
        }
        assert!(validate_reason("x".repeat(4097)).is_err());
    }

    #[test]
    fn revoked_key_blocks_execution() {
        let key = signing_key();
        let operator = OperatorId::new("reviewer").unwrap();
        let mut trust = TrustStore::default();
        trust
            .add_key(
                "review-key",
                hex::encode(key.verifying_key().to_bytes()),
                "chave",
                operator.clone(),
            )
            .unwrap();
        trust.revoke_key("review-key").unwrap();
        let attestation = sign_approval(
            "demo",
            "0.1.0",
            &operator,
            &"a".repeat(64),
            "motivo",
            "review-key".to_owned(),
            &key,
        );
        assert!(matches!(
            trust.verify_attestation(
                "demo",
                "0.1.0",
                &operator,
                &"a".repeat(64),
                "motivo",
                &attestation,
            ),
            Err(SkillError::RevokedKey(_))
        ));
    }
}
