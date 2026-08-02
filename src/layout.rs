pub use crate::header::{
    AbiProfile, Layout, Magic, RcHeader, RecoveryContext, RecoveryHandler, Status,
};
pub use crate::msg::Repr;
pub use crate::schema::{
    LayoutContext, LayoutId, LayoutInfo, RegionAdmission, RegionId, SchemaId, SchemaKey,
    SharedSchema,
};
pub use crate::token::Shape;

pub const fn recovery_handler<H: Repr>() -> RecoveryHandler {
    RecoveryHandler::of::<crate::channel::Duplex<H>>()
}
