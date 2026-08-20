#![no_main]

use ed25519_dalek::SigningKey;
use libfuzzer_sys::fuzz_target;
use shaka_core::OperatorId;
use shaka_skills::{ApprovalAttestation, APPROVAL_PROTOCOL_V1, TrustStore, public_key_hex};

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
    let _ = trust_store.verify_attestation(
        &String::from_utf8_lossy(data),
        "0.1.0",
        &operator,
        &"a".repeat(64),
        "fuzz reason",
        &attestation,
    );
});
