//! Dynamic detail providers.
//!
//! Search results may contain a static detail preview. Providers registered
//! here can replace that preview with asynchronously loaded data without the
//! launcher knowing which provider owns the result.

use crate::result::{DetailView, SearchResult};
use anyhow::Result;
use std::sync::Arc;

include!(concat!(env!("OUT_DIR"), "/detail_providers.rs"));

pub trait DynamicDetailProvider: Send + Sync {
    fn matches(&self, item: &SearchResult) -> bool;
    fn cached(&self, language: &str) -> Option<DetailView>;
    fn load(&self, language: &str) -> Result<DetailView>;
    fn error_view(&self, language: &str) -> DetailView;
}

#[derive(Clone, Default)]
pub struct DetailRegistry {
    providers: Vec<Arc<dyn DynamicDetailProvider>>,
}

impl DetailRegistry {
    pub fn register(&mut self, provider: Arc<dyn DynamicDetailProvider>) {
        self.providers.push(provider);
    }

    pub fn default_providers() -> Self {
        let mut registry = Self::default();
        crate::details::register_all(&mut registry);
        registry
    }

    pub fn provider_for(&self, item: &SearchResult) -> Option<Arc<dyn DynamicDetailProvider>> {
        self.providers
            .iter()
            .find(|provider| provider.matches(item))
            .cloned()
    }
}
