pub mod asynch;
pub mod sync;
pub mod bare;
mod tests;

pub trait UringSpec {
    type SQE;
    type CQE;
}

pub const DEFAULT_CAP: usize = 1 << 5;

macro_rules! with_send {
    ($self:ident, $field:ident, $sender:ident,$data:ident) => {
        impl<S: UringSpec> $self<S> {
            pub fn sender(&self) -> &$sender<S::$data> {
                &self.$field
            }
        }
    };
}

macro_rules! with_recv {
    ($self:ident, $field:ident, $receiver:ident,$data:ident) => {
        impl<S: UringSpec> $self<S> {
            pub fn receiver(&self) -> &$receiver<S::$data> {
                &self.$field
            }
        }
    };
}

pub(crate) use with_recv;
pub(crate) use with_send;
