use crate::{
    SchemaKey,
    numeric::Id,
    schema::{compose_schema, schema_id},
};

pub type TypeId = u64;

/// A value whose initialized representation may cross a process boundary.
///
/// ```compile_fail
/// use evering::{Repr, SchemaId, SchemaKey};
/// use std::rc::Rc;
///
/// struct Local(Rc<()>);
/// unsafe impl Repr for Local {
///     const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(1), 1);
/// }
/// ```
///
/// ```compile_fail
/// use evering::{Repr, SchemaId, SchemaKey};
///
/// struct Dropping;
/// impl Drop for Dropping {
///     fn drop(&mut self) {}
/// }
/// unsafe impl Repr for Dropping {
///     const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(1), 1);
/// }
/// const _: () = <Dropping as Repr>::VALID;
/// ```
///
/// # Safety
///
/// `SCHEMA` must identify one stable representation across every admitted
/// build. Every observable byte must be initialized and values must contain no
/// process-local pointer, reference, handle, or other local authority.
pub unsafe trait Repr: Send + 'static {
    const SCHEMA: SchemaKey;

    #[doc(hidden)]
    const VALID: () = assert!(
        !core::mem::needs_drop::<Self>(),
        "shared-memory representations must not require Drop"
    );
}

macro_rules! repr {
    ($($ty:ty => $name:literal),* $(,)?) => {
        $(
            unsafe impl Repr for $ty {
                const SCHEMA: SchemaKey = SchemaKey::new(schema_id($name), 1);
            }
        )*
    };
}

repr! {
    u8 => "core.u8",
    u16 => "core.u16",
    u32 => "core.u32",
    u64 => "core.u64",
    f32 => "core.f32",
    f64 => "core.f64",
    i8 => "core.i8",
    i16 => "core.i16",
    i32 => "core.i32",
    i64 => "core.i64",
    bool => "core.bool",
    () => "core.unit",
}

unsafe impl<T: Repr> Repr for [T] {
    const SCHEMA: SchemaKey =
        compose_schema(SchemaKey::new(schema_id("evering.slice"), 1), T::SCHEMA);
    const VALID: () = T::VALID;
}

/// A library-owned protocol header for safely encoded byte payloads.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encoded {
    schema: SchemaKey,
}

impl Encoded {
    #[inline(always)]
    pub const fn new(schema: SchemaKey) -> Self {
        Self { schema }
    }

    #[inline(always)]
    pub const fn schema(&self) -> SchemaKey {
        self.schema
    }
}

unsafe impl Repr for Encoded {
    const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.encoded"), 1);
}

/// Derives a runtime discriminator from a protocol schema and a body schema.
///
/// Namespace and body remain API positions; neither needs a separate marker or
/// a user-selected numeric tag.
#[inline(always)]
pub const fn type_id<P: Repr + ?Sized, T: Repr + ?Sized>() -> TypeId {
    let protocol = P::SCHEMA;
    let body = T::SCHEMA;
    let domain = schema_id("evering.type");
    (domain.0
        ^ protocol.id.0.rotate_left(7)
        ^ (protocol.revision as u64).rotate_left(19)
        ^ body.id.0.rotate_left(31)
        ^ (body.revision as u64).rotate_left(47))
    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

pub trait Tag<T>: Repr {
    fn with_tag(self, value: T) -> Self
    where
        Self: Sized;
    fn tag(&self) -> T;
}

pub trait TagRef<T>: Repr {
    fn set_tag(&mut self, value: T);
    fn tag_ref(&self) -> &T;
}

pub trait TagId: Repr {
    fn with_id(self, value: Id) -> Self
    where
        Self: Sized;
    fn id(&self) -> Id;
}

#[cfg(test)]
mod tests {
    use super::{Repr, type_id};
    use crate::{SchemaId, SchemaKey};

    struct First;
    struct Second;
    struct Member;

    unsafe impl Repr for First {
        const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x100), 1);
    }
    unsafe impl Repr for Second {
        const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x200), 1);
    }
    unsafe impl Repr for Member {
        const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x300), 1);
    }

    #[test]
    fn protocol_scopes_body_identity() {
        assert_ne!(type_id::<First, Member>(), type_id::<Second, Member>());
    }

    #[test]
    fn body_schema_changes_identity() {
        assert_ne!(type_id::<(), u8>(), type_id::<(), u32>());
    }

    #[test]
    fn slice_schema_is_not_element_schema() {
        assert_ne!(type_id::<(), [u32]>(), type_id::<(), u32>());
    }
}
