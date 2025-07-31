pub mod asynch;
pub mod sync;
mod tests;

pub trait UringSpec {
    type SQE;
    type CQE;
}

