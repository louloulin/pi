//! Facet loaders: static generation sources and their composition.
//!
//! Port of `packages/chord/src/api.ts` (`createStaticFacetLoader`, `combineFacetLoaders`) and
//! `packages/chord/src/facets/loader.ts` (`disposeLoadedFacets`).
//!
//! The contracts that matter:
//!
//! * a loader that fails to load must dispose the generations it already loaded, in reverse order,
//!   and aggregate the cleanup failures under `Facet loading and cleanup failed`;
//! * disposing a composed generation disposes its entries in reverse order, is idempotent, and
//!   reports a single failure as-is or several under `Failed to dispose loaded facets`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::facets::lifecycle::Effect;
use crate::types::{Facet, FacetError, FacetFuture, FacetLoader, LoadResultFuture, LoadedFacets};

/// A loader that always yields the same generation and owns nothing.
pub struct StaticFacetLoader {
    facets: Vec<Arc<dyn Facet>>,
}

impl StaticFacetLoader {
    /// Creates a loader for `facets`.
    pub fn new(facets: Vec<Arc<dyn Facet>>) -> Self {
        Self { facets }
    }
}

impl FacetLoader for StaticFacetLoader {
    fn load(&self) -> LoadResultFuture {
        let facets = self.facets.clone();
        Box::pin(async move { Ok(Box::new(LoadedFacetsImpl::new(facets)) as Box<dyn LoadedFacets>) })
    }
}

/// Creates a loader for an already known generation (upstream `createStaticFacetLoader`).
pub fn create_static_facet_loader(facets: Vec<Arc<dyn Facet>>) -> Arc<dyn FacetLoader> {
    Arc::new(StaticFacetLoader::new(facets))
}

/// A resolved generation plus the disposals that own it.
///
/// Disposal runs the effects in reverse order and is idempotent.
pub struct LoadedFacetsImpl {
    facets: Vec<Arc<dyn Facet>>,
    disposals: Mutex<Vec<Effect>>,
    disposed: AtomicBool,
}

impl LoadedFacetsImpl {
    /// Creates a generation with nothing to dispose.
    pub fn new(facets: Vec<Arc<dyn Facet>>) -> Self {
        Self {
            facets,
            disposals: Mutex::new(Vec::new()),
            disposed: AtomicBool::new(false),
        }
    }

    /// Creates a generation that runs `disposals` in reverse order when disposed.
    pub fn with_disposals(facets: Vec<Arc<dyn Facet>>, disposals: Vec<Effect>) -> Self {
        Self {
            facets,
            disposals: Mutex::new(disposals),
            disposed: AtomicBool::new(false),
        }
    }

    /// Adds a disposal effect.
    pub fn push_disposal(&self, effect: Effect) {
        self.disposals.lock().push(effect);
    }
}

impl LoadedFacets for LoadedFacetsImpl {
    fn facets(&self) -> Vec<Arc<dyn Facet>> {
        self.facets.clone()
    }

    fn dispose(&self) -> FacetFuture {
        let mut disposals = std::mem::take(&mut *self.disposals.lock());
        let already = self.disposed.swap(true, Ordering::AcqRel);
        Box::pin(async move {
            if already {
                return Ok(());
            }
            disposals.reverse();
            let mut failures = Vec::new();
            for disposal in disposals {
                if let Err(error) = disposal().await {
                    failures.push(error);
                }
            }
            match failures.len() {
                0 => Ok(()),
                1 => Err(failures.remove(0)),
                _ => Err(FacetError::with_causes(
                    "Failed to dispose loaded facets",
                    failures,
                )),
            }
        })
    }
}

impl std::fmt::Debug for LoadedFacetsImpl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadedFacetsImpl")
            .field("facet_count", &self.facets.len())
            .field("disposed", &self.disposed.load(Ordering::Acquire))
            .finish()
    }
}

/// Disposes several generations, collecting failures instead of aborting on the first.
pub async fn dispose_loaded_facets(loaded: &[Arc<dyn LoadedFacets>]) -> Vec<FacetError> {
    let mut errors = Vec::new();
    for entry in loaded {
        if let Err(error) = entry.dispose().await {
            errors.push(error);
        }
    }
    errors
}

/// A loader that composes several loaders into one generation.
pub struct CombinedFacetLoader {
    loaders: Vec<Arc<dyn FacetLoader>>,
}

impl CombinedFacetLoader {
    /// Composes `loaders`; they are loaded in order and disposed in reverse.
    pub fn new(loaders: Vec<Arc<dyn FacetLoader>>) -> Self {
        Self { loaders }
    }
}

impl FacetLoader for CombinedFacetLoader {
    fn load(&self) -> LoadResultFuture {
        let loaders = self.loaders.clone();
        Box::pin(async move {
            let mut loaded: Vec<Arc<dyn LoadedFacets>> = Vec::new();
            for loader in &loaders {
                match loader.load().await {
                    Ok(entry) => loaded.push(Arc::from(entry)),
                    Err(error) => {
                        loaded.reverse();
                        let cleanup = dispose_loaded_facets(&loaded).await;
                        if cleanup.is_empty() {
                            return Err(error);
                        }
                        let mut failures = vec![error];
                        failures.extend(cleanup);
                        return Err(FacetError::aggregate(
                            "Facet loading and cleanup failed",
                            failures,
                        ));
                    }
                }
            }
            Ok(Box::new(CombinedLoadedFacets::new(loaded)) as Box<dyn LoadedFacets>)
        })
    }
}

impl std::fmt::Debug for CombinedFacetLoader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CombinedFacetLoader")
            .field("loader_count", &self.loaders.len())
            .finish()
    }
}

/// The generation produced by [`CombinedFacetLoader`]: the concatenation of its entries.
pub struct CombinedLoadedFacets {
    entries: Vec<Arc<dyn LoadedFacets>>,
    disposed: AtomicBool,
}

impl CombinedLoadedFacets {
    /// Wraps `entries`; disposal runs in reverse order.
    pub fn new(entries: Vec<Arc<dyn LoadedFacets>>) -> Self {
        Self {
            entries,
            disposed: AtomicBool::new(false),
        }
    }
}

impl LoadedFacets for CombinedLoadedFacets {
    fn facets(&self) -> Vec<Arc<dyn Facet>> {
        self.entries
            .iter()
            .flat_map(|entry| entry.facets())
            .collect()
    }

    fn dispose(&self) -> FacetFuture {
        let entries: Vec<Arc<dyn LoadedFacets>> = self.entries.iter().rev().cloned().collect();
        let already = self.disposed.swap(true, Ordering::AcqRel);
        Box::pin(async move {
            if already {
                return Ok(());
            }
            let failures = dispose_loaded_facets(&entries).await;
            match failures.len() {
                0 => Ok(()),
                1 => Err(failures.into_iter().next().unwrap_or_else(|| {
                    FacetError::new("Failed to dispose loaded facets")
                })),
                _ => Err(FacetError::with_causes(
                    "Failed to dispose loaded facets",
                    failures,
                )),
            }
        })
    }
}

/// Composes several loaders into one (upstream `combineFacetLoaders`).
pub fn combine_facet_loaders(loaders: Vec<Arc<dyn FacetLoader>>) -> Arc<dyn FacetLoader> {
    Arc::new(CombinedFacetLoader::new(loaders))
}
