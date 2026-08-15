use std::ops::{Deref, DerefMut};

/// Provides shared access to a world resource for one system run.
pub struct Res<'world, T: 'static + Send + Sync>(&'world T);

impl<'world, T: 'static + Send + Sync> Res<'world, T> {
    pub(crate) fn new(value: &'world T) -> Self {
        Self(value)
    }
}

impl<T: 'static + Send + Sync> Deref for Res<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

/// Provides exclusive access to a world resource for one system run.
pub struct ResMut<'world, T: 'static + Send + Sync>(&'world mut T);

impl<'world, T: 'static + Send + Sync> ResMut<'world, T> {
    pub(crate) fn new(value: &'world mut T) -> Self {
        Self(value)
    }
}

impl<T: 'static + Send + Sync> Deref for ResMut<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl<T: 'static + Send + Sync> DerefMut for ResMut<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
    }
}
