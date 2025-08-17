pub mod asynch;
pub mod bare;
pub mod sync;
mod tests;

pub trait UringSpec {
    type SQE;
    type CQE;
}

pub const DEFAULT_CAP: usize = 1 << 5;

pub trait ISender : Sealed {
    type Item;
    type Error;
    type TryError;
    fn send(&self, item: Self::Item) -> impl Future<Output = Result<(), Self::Error>> {
        async { unimplemented!() }
    }
    fn try_send(&self, item: Self::Item) -> Result<(), Self::TryError>;
}

pub trait IReceiver : Sealed {
    type Item;
    type Error;
    type TryError;
    fn recv(&self) -> impl Future<Output = Result<Self::Item, Self::Error>> {
        async { unimplemented!() }
    }
    fn try_recv(&self) -> Result<Self::Item, Self::TryError>;    
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

macro_rules! with_recv {
    ($self:ident, $field:ident, $receiver:ident,$data:ident) => {
        impl<S: UringSpec> $self<S> {
            pub fn receiver(&self) -> &$receiver<S::$data> {
                &self.$field
            }
        }
    };
}


use crate::seal::Sealed;
