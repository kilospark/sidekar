use super::*;

#[test]
fn local_owns_the_size_initially() {
    let ownership = SizeOwnership::new();
    assert_eq!(ownership.owner(), SizeOwner::Local);
}

#[test]
fn claim_transfers_ownership_and_applies() {
    let mut ownership = SizeOwnership::new();
    assert!(ownership.apply(SizeOwner::Remote, SizeIntent::Claim));
    assert_eq!(ownership.owner(), SizeOwner::Remote);
}

#[test]
fn update_from_non_owner_is_rejected() {
    let mut ownership = SizeOwnership::new();
    ownership.apply(SizeOwner::Remote, SizeIntent::Claim);
    assert!(!ownership.apply(SizeOwner::Local, SizeIntent::Update));
    assert_eq!(ownership.owner(), SizeOwner::Remote);
}

#[test]
fn update_from_owner_is_applied_without_changing_owner() {
    let mut ownership = SizeOwnership::new();
    ownership.apply(SizeOwner::Remote, SizeIntent::Claim);
    assert!(ownership.apply(SizeOwner::Remote, SizeIntent::Update));
    assert_eq!(ownership.owner(), SizeOwner::Remote);
}

#[test]
fn local_can_take_ownership_back() {
    let mut ownership = SizeOwnership::new();
    ownership.apply(SizeOwner::Remote, SizeIntent::Claim);
    assert!(ownership.apply(SizeOwner::Local, SizeIntent::Claim));
    assert_eq!(ownership.owner(), SizeOwner::Local);
    assert!(ownership.apply(SizeOwner::Local, SizeIntent::Update));
}

#[test]
fn missing_intent_defaults_to_claim() {
    assert_eq!(SizeIntent::parse(None), SizeIntent::Claim);
    assert_eq!(SizeIntent::parse(Some("claim")), SizeIntent::Claim);
    assert_eq!(SizeIntent::parse(Some("update")), SizeIntent::Update);
    assert_eq!(SizeIntent::parse(Some("nonsense")), SizeIntent::Claim);
}
