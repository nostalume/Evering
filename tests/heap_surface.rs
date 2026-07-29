use evering::PBox;

fn accepts_heap_box(_: PBox<'_, u64>) {}

#[test]
fn shared_box_has_only_mapping_lifetime_and_value_in_its_type() {
    let _ = accepts_heap_box;
}
