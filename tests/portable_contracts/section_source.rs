//! The Windows section leg, which only a Windows host can compile: a freshly made anonymous section is a
//! session source in its own right, and the three requests that must not be honored — zero size, no read
//! access, and a mapping wider than the section was declared with — are refused by the call they are
//! made to, not by the first fault that reaches through the mapping.

use evering::{
    Session,
    layout::RegionId,
    mapping::{Access, Request, Source},
    os::windows::Section,
};

#[test]
fn anonymous_section_is_a_safe_session_source() {
    const SIZE: usize = 4 * 1024 * 1024;
    let section = Section::anonymous(SIZE, Access::READ | Access::WRITE).unwrap();
    let session = Session::create(
        section,
        Request::new(SIZE, Access::READ | Access::WRITE),
        RegionId::new(0x5749_4e44_4f57_534d, 1),
    )
    .unwrap();
    assert_ne!(session.base_addr(), 0);
}

#[test]
fn section_rejects_empty_unreadable_and_escalated_mappings() {
    assert!(Section::anonymous(0, Access::READ).is_err());
    assert!(Section::anonymous(4096, Access::WRITE).is_err());

    let section = Section::anonymous(4096, Access::READ).unwrap();
    assert!(
        <Section<_> as Source>::map(section, Request::new(4096, Access::READ | Access::WRITE),)
            .is_err()
    );
}
