use engine::data_asset::DataAssetRef;
use engine_authoring::id::AssetId;

#[test]
fn data_asset_reference_roundtrips_through_authoring_value() {
    let asset = AssetId::generate();
    let reference = DataAssetRef::new(asset.clone());

    let decoded = DataAssetRef::from_authoring_value(&reference.to_authoring_value())
        .expect("data asset reference must roundtrip");

    assert_eq!(decoded, reference);
    assert_eq!(decoded.asset_id(), Some(&asset));
    assert!(decoded.is_assigned());
}

#[test]
fn data_asset_reference_default_is_unassigned() {
    let reference = DataAssetRef::default();
    let decoded = DataAssetRef::from_authoring_value(&reference.to_authoring_value())
        .expect("unassigned data asset reference must roundtrip");

    assert_eq!(decoded, reference);
    assert_eq!(decoded.asset_id(), None);
    assert!(!decoded.is_assigned());
}
