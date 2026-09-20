//! Facet loader contract tests: generation ordering, static loaders, and failure isolation.
//!
//! Upstream `packages/chord/test/facet-loader.test.ts`.

use std::sync::{Arc, Mutex};

use pi_chord::context::block_on;
use pi_chord::facets::{
    combine_facet_loaders, create_static_facet_loader, Effect, LoadedFacetsImpl,
};
use pi_chord::types::{
    define_facet, Facet, FacetError, FacetFuture, FacetLoader, LoadResultFuture, LoadedFacets,
};

type Events = Arc<Mutex<Vec<String>>>;

fn events() -> Events {
    Arc::new(Mutex::new(Vec::new()))
}

fn record(events: &Events, event: impl Into<String>) {
    events.lock().expect("lock").push(event.into());
}

fn seen(events: &Events) -> Vec<String> {
    events.lock().expect("lock").clone()
}

fn named_facet(id: &'static str) -> Arc<dyn Facet> {
    define_facet(id, |_env| Ok(()))
}

/// A loader that fails, or that yields one facet plus a disposal which records itself.
struct RecordingLoader {
    id: &'static str,
    log: Events,
    fail_load: bool,
    fail_dispose: bool,
}

impl RecordingLoader {
    fn new(id: &'static str, log: &Events) -> Self {
        Self {
            id,
            log: Arc::clone(log),
            fail_load: false,
            fail_dispose: false,
        }
    }

    fn failing_load(mut self) -> Self {
        self.fail_load = true;
        self
    }

    fn failing_dispose(mut self) -> Self {
        self.fail_dispose = true;
        self
    }

    fn disposal(&self) -> Effect {
        let id = self.id;
        let log = Arc::clone(&self.log);
        let fail = self.fail_dispose;
        Box::new(move || {
            Box::pin(async move {
                record(&log, format!("dispose:{id}"));
                if fail {
                    return Err(FacetError::new(format!("dispose {id} failed")));
                }
                Ok(())
            }) as FacetFuture
        })
    }
}

impl FacetLoader for RecordingLoader {
    fn load(&self) -> LoadResultFuture {
        let id = self.id;
        let log = Arc::clone(&self.log);
        let facets = vec![named_facet(id)];
        let disposal = self.disposal();
        let fail = self.fail_load;
        Box::pin(async move {
            record(&log, format!("load:{id}"));
            if fail {
                return Err(FacetError::new("loader failed"));
            }
            Ok(
                Box::new(LoadedFacetsImpl::with_disposals(facets, vec![disposal]))
                    as Box<dyn LoadedFacets>,
            )
        })
    }
}

#[test]
fn a_static_loader_yields_its_generation_and_disposes_once() {
    let loader = create_static_facet_loader(vec![named_facet("one"), named_facet("two")]);
    let loaded = block_on(loader.load()).expect("loads");
    let ids: Vec<String> = loaded.facets().iter().map(|f| f.id().to_string()).collect();
    assert_eq!(ids, ["one", "two"]);

    assert!(block_on(loaded.dispose()).is_ok());
    // Disposal is idempotent: a second call does not run the effects again.
    assert!(block_on(loaded.dispose()).is_ok());
}

#[test]
fn combined_loaders_load_in_order_and_dispose_in_reverse() {
    let log = events();
    let loaders = vec![
        Arc::new(RecordingLoader::new("a", &log)) as Arc<dyn FacetLoader>,
        Arc::new(RecordingLoader::new("b", &log)) as Arc<dyn FacetLoader>,
    ];
    let loaded = block_on(combine_facet_loaders(loaders).load()).expect("loads");
    assert_eq!(seen(&log), ["load:a", "load:b"]);

    let ids: Vec<String> = loaded.facets().iter().map(|f| f.id().to_string()).collect();
    assert_eq!(ids, ["a", "b"]);

    assert!(block_on(loaded.dispose()).is_ok());
    assert_eq!(seen(&log), ["load:a", "load:b", "dispose:b", "dispose:a"]);
    // Idempotent.
    assert!(block_on(loaded.dispose()).is_ok());
    assert_eq!(seen(&log).len(), 4);
}

#[test]
fn a_failing_loader_disposes_what_it_already_loaded() {
    let log = events();
    let loaders = vec![
        Arc::new(RecordingLoader::new("a", &log)) as Arc<dyn FacetLoader>,
        Arc::new(RecordingLoader::new("b", &log).failing_load()) as Arc<dyn FacetLoader>,
        Arc::new(RecordingLoader::new("c", &log)) as Arc<dyn FacetLoader>,
    ];
    let error = match block_on(combine_facet_loaders(loaders).load()) {
        Ok(_) => panic!("the loader was expected to fail"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), "loader failed");
    // `c` was never loaded, and `a` was cleaned up — in reverse load order.
    assert_eq!(seen(&log), ["load:a", "load:b", "dispose:a"]);
}

#[test]
fn a_failing_cleanup_is_aggregated_with_the_load_failure() {
    let log = events();
    let loaders = vec![
        Arc::new(RecordingLoader::new("a", &log).failing_dispose()) as Arc<dyn FacetLoader>,
        Arc::new(RecordingLoader::new("b", &log).failing_load()) as Arc<dyn FacetLoader>,
    ];
    let error = match block_on(combine_facet_loaders(loaders).load()) {
        Ok(_) => panic!("the loader was expected to fail"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .starts_with("Facet loading and cleanup failed"));
    let text = error.to_string();
    assert!(text.contains("loader failed"), "{text}");
    assert!(text.contains("dispose a failed"), "{text}");
}

#[test]
fn disposal_failures_use_the_upstream_aggregate_messages() {
    let log = events();

    // A single failing disposal is reported as-is.
    let single = LoadedFacetsImpl::with_disposals(
        vec![named_facet("a")],
        vec![Box::new({
            let log = Arc::clone(&log);
            move || {
                Box::pin(async move {
                    record(&log, "single");
                    Err(FacetError::new("nope"))
                }) as FacetFuture
            }
        })],
    );
    assert_eq!(
        block_on(single.dispose()).expect_err("failed").to_string(),
        "nope"
    );

    // Several are aggregated under the loader's message.
    let make_effect = |label: &'static str| -> Effect {
        let log = Arc::clone(&log);
        Box::new(move || {
            Box::pin(async move {
                record(&log, label);
                Err(FacetError::new(format!("{label} failed")))
            }) as FacetFuture
        })
    };
    let many = LoadedFacetsImpl::with_disposals(
        vec![named_facet("a")],
        vec![make_effect("first"), make_effect("second")],
    );
    let error = block_on(many.dispose()).expect_err("failed");
    assert!(error
        .to_string()
        .starts_with("Failed to dispose loaded facets"));
    // Reverse order, and both failures are reported.
    assert_eq!(seen(&log), ["single", "second", "first"]);
    let text = error.to_string();
    assert!(text.contains("first failed"), "{text}");
    assert!(text.contains("second failed"), "{text}");
}
