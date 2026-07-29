#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegionId {
    pub high: u64,
    pub low: u64,
}

impl RegionId {
    pub const fn new(high: u64, low: u64) -> Self {
        Self { high, low }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutId {
    pub region: RegionId,
    pub offset: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionAdmission {
    Create(RegionId),
    Expect(RegionId),
    Discover,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaId(pub u64);

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemaKey {
    pub id: SchemaId,
    pub revision: u32,
}

impl SchemaKey {
    pub const fn new(id: SchemaId, revision: u32) -> Self {
        Self { id, revision }
    }
}

pub trait SharedSchema {
    const SCHEMA: SchemaKey;
}

/// A stable, fully initialized value stored in shared memory for layout admission.
///
/// # Safety
///
/// Implementations must have a stable representation for their schema revision
/// and must not contain process-local pointers, references, handles, or padding
/// whose value is left uninitialized.
pub unsafe trait LayoutInfo: SharedSchema + Copy + Eq + core::fmt::Debug {}

#[derive(Clone, Copy, Debug)]
pub struct LayoutContext {
    pub region: RegionId,
    pub offset: u64,
    pub allow_init: bool,
}

pub const fn schema_id(name: &str) -> SchemaId {
    let mut hash = 0xcbf29ce484222325_u64;
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        i += 1;
    }
    SchemaId(hash)
}

pub const fn compose_schema(domain: SchemaKey, part: SchemaKey) -> SchemaKey {
    let id = SchemaId(
        (domain.id.0 ^ part.id.0.rotate_left(17) ^ (part.revision as u64).rotate_left(41))
            .wrapping_mul(0x9E3779B97F4A7C15),
    );
    let revision = domain.revision.rotate_left(7).wrapping_mul(0x9E37_79B9) ^ part.revision;
    SchemaKey::new(id, revision)
}

#[cfg(test)]
pub const fn compose_const(domain: SchemaKey, value: u64) -> SchemaKey {
    compose_schema(domain, SchemaKey::new(SchemaId(value), 0))
}

impl SharedSchema for () {
    const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.unit"), 1);
}

unsafe impl LayoutInfo for () {}

#[cfg(test)]
mod tests {
    use super::{SchemaId, SchemaKey, compose_schema};

    #[test]
    fn child_revision_changes_composite_key() {
        let domain = SchemaKey::new(SchemaId(1), 1);
        assert_ne!(
            compose_schema(domain, SchemaKey::new(SchemaId(2), 1)),
            compose_schema(domain, SchemaKey::new(SchemaId(2), 2))
        );
    }
}
