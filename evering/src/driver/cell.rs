use alloc::fmt::Debug;
use core::{
    fmt::Display,
    ops::{Deref, DerefMut},
};

pub struct IdCell<Id, T> {
    id: Id,
    data: T,
}

impl<Id, T> IdCell<Id, T> {
    pub fn new(id: Id, data: T) -> Self {
        IdCell { id, data }
    }

    pub fn into_inner(self) -> (Id, T) {
        (self.id, self.data)
    }

    pub fn store<U>(self, data: U) -> (IdCell<Id, U>, T) {
        let IdCell { id, data: old } = self;

        (IdCell::new(id, data), old)
    }

    pub fn replace<U>(self, data: U) -> IdCell<Id, U> {
        let (res, _) = self.store(data);
        res
    }

    pub fn update(&mut self, f: impl FnOnce(&mut T)) {
        f(&mut self.data);
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> IdCell<Id, U> {
        IdCell {
            id: self.id,
            data: f(self.data),
        }
    }
}

impl<Id, T> Deref for IdCell<Id, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<Id, T> DerefMut for IdCell<Id, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

impl<Id, T> AsRef<T> for IdCell<Id, T> {
    fn as_ref(&self) -> &T {
        &self.data
    }
}

impl<Id, T> AsMut<T> for IdCell<Id, T> {
    fn as_mut(&mut self) -> &mut T {
        &mut self.data
    }
}

impl<Id, T> Clone for IdCell<Id, T>
where
    T: Clone,
    Id: Clone,
{
    fn clone(&self) -> Self {
        IdCell {
            id: self.id.clone(),
            data: self.data.clone(),
        }
    }
}

impl<Id, T> Copy for IdCell<Id, T>
where
    T: Copy,
    Id: Copy,
{
}

impl<Id, T> Display for IdCell<Id, T>
where
    T: Display,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.data)
    }
}

impl<Id, T> Debug for IdCell<Id, T>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("IdCell").finish()
    }
}

impl<Id, T> PartialEq for IdCell<Id, T>
where
    T: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl<Id, T> Eq for IdCell<Id, T> where T: Eq {}

#[cfg(feature = "std")]
use std::hash::{Hash, Hasher};
#[cfg(feature = "std")]
impl<Id, T> Hash for IdCell<Id, T>
where
    T: Hash,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.data.hash(state);
    }
}

impl<Id, T> Unpin for IdCell<Id, T> where T: Unpin {}

unsafe impl<Id, T> Send for IdCell<Id, T>
where
    Id: Send,
    T: Send,
{
}
unsafe impl<Id, T> Sync for IdCell<Id, T>
where
    Id: Sync,
    T: Sync,
{
}
