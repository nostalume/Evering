use evering::{
    Encoded, PoolId, Port,
    layout::{RegionId, Repr},
};

const REGION: RegionId = RegionId::new(7, 11);

#[test]
fn pool_and_port_have_checked_canonical_bytes() {
    let pool = PoolId::new(REGION, 13, 17, 19);
    let pool_bytes = [
        7, 0, 0, 0, 0, 0, 0, 0, 11, 0, 0, 0, 0, 0, 0, 0, 13, 0, 0, 0, 17, 0, 0, 0, 19, 0, 0, 0, 0,
        0, 0, 0,
    ];
    assert_eq!(pool.to_bytes(), pool_bytes);
    assert_eq!(PoolId::from_bytes(&pool_bytes), Some(pool));

    let id = evering::ChannelId::new(REGION, 23, 29, 31, 37);
    let port = Port::<Encoded>::from_parts(id, 1, 41).unwrap();
    let port_bytes = [
        7, 0, 0, 0, 0, 0, 0, 0, 11, 0, 0, 0, 0, 0, 0, 0, 23, 0, 0, 0, 29, 0, 0, 0, 31, 0, 0, 0, 0,
        0, 0, 0, 37, 0, 0, 0, 0, 0, 0, 0, 1, 41, 0, 0, 0, 0, 0, 0, 0,
    ];
    assert_eq!(port.to_bytes(), port_bytes);
    let opened = Port::<Encoded>::from_bytes(&port_bytes).unwrap();
    assert_eq!(opened.id(), port.id());
    assert_eq!((opened.role(), opened.generation()), (1, 41));
    assert_eq!(Encoded::SCHEMA.revision, 2);
}

#[test]
fn malformed_identity_bytes_are_rejected() {
    assert!(PoolId::from_bytes(&[]).is_none());
    let mut bytes = Port::<Encoded>::from_parts(evering::ChannelId::new(REGION, 1, 2, 3, 4), 0, 5)
        .unwrap()
        .to_bytes();
    bytes[40] = 2;
    assert!(Port::<Encoded>::from_bytes(&bytes).is_none());
    bytes[40] = 0;
    bytes[41..].fill(0);
    assert!(Port::<Encoded>::from_bytes(&bytes).is_none());
}
