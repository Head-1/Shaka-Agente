#![no_main]

use ed25519_dalek::SigningKey;
use libfuzzer_sys::fuzz_target;
use shaka_core::OperatorId;
use shaka_skills::{
    ApprovalAttestation, ApprovalBinding, APPROVAL_PROTOCOL_V1, TrustStore, public_key_hex,
};

fuzz_target!(|data: &[u8]| {
    let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
    let operator = match OperatorId::new("fuzz-operator") {
        Ok(operator) => operator,
        Err(_) => return,
    };
    let mut trust_store = TrustStore::default();
    if trust_store
        .add_key(
            "fuzz-key",
            public_key_hex(&signing_key),
            "fuzz trust root",
            operator.clone(),
        )
        .is_err()
    {
        return;
    }
    let signature_hex = String::from_utf8_lossy(data).into_owned();
    let attestation = ApprovalAttestation {
        protocol: APPROVAL_PROTOCOL_V1.to_owned(),
        key_id: "fuzz-key".to_owned(),
        public_key_hex: public_key_hex(&signing_key),
        signature_hex,
    };
    let name = String::from_utf8_lossy(data).into_owned();
    let artifact_sha256 = "a".repeat(64);
    let authority_sha256 = "b".repeat(64);
    let binding = ApprovalBinding::new(
        &name,
        "0.1.0",
        &operator,
        &artifact_sha256,
        "fuzz reason",
        &authority_sha256,
    );
    let _ = trust_store.verify_attestation(&binding, &attestation);
});
