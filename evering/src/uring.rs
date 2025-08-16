pub mod asynch;
pub mod sync;
mod tests;

pub trait UringSpec {
    type SQE;
    type CQE;
}

macro_rules! with_send {
    ($self:ident, $field:ident, $sender:ident,$data:ident) => {
        impl<S: UringSpec> $self<S> {
            pub fn sender(&self) -> &$sender<S::$data> {
                &self.$field
            }
        }
    };
}

macro_rules! with_send_alloc {
    ($self:ident, $field:ident, $sender:ident,$data:ident) => {
        impl<S: UringSpec, A: Allocator> $self<S, A> {
            pub fn sender(&self) -> &$sender<S::$data, A> {
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

macro_rules! with_recv_alloc {
    ($self:ident, $field:ident, $receiver:ident,$data:ident) => {
        impl<S: UringSpec, A: Allocator> $self<S, A> {
            pub fn receiver(&self) -> &$receiver<S::$data, A> {
                &self.$field
            }
        }
    };
}

pub(crate) use with_recv;
pub(crate) use with_recv_alloc;
pub(crate) use with_send;
pub(crate) use with_send_alloc;
