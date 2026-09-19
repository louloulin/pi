//! Runs the shared adapter conformance suite against the in-memory reference
//! adapter, exactly as an external adapter author would.

use futures::executor::block_on;
use pi_telemetry::testing::{create_telemetry_adapter_conformance, TelemetryAdapterFixture};
use pi_telemetry::MemoryTelemetry;
use std::sync::Arc;

fn factory() -> TelemetryAdapterFixture {
    let telemetry = MemoryTelemetry::new();
    let snapshots = telemetry.clone();
    TelemetryAdapterFixture::new(Arc::new(telemetry), Arc::new(move || snapshots.spans()))
}

#[test]
fn memory_telemetry_passes_the_adapter_conformance_suite() {
    let cases = create_telemetry_adapter_conformance(Arc::new(factory));
    assert_eq!(
        cases.len(),
        6,
        "the ported conformance suite should keep all six cases"
    );
    for case in cases {
        block_on(case.run()).unwrap_or_else(|error| {
            panic!(
                "conformance case {} / {} failed: {error}",
                case.group(),
                case.name()
            )
        });
    }
}
