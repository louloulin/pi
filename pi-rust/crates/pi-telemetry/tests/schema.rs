//! Schema data serializes to the same JSON shape the TypeScript package uses.

use indexmap::IndexMap;
use pi_telemetry::{
    define_telemetry_schema, TelemetryAttributeCardinality, TelemetryAttributeDefinition,
    TelemetryAttributeType, TelemetryAttributeValues, TelemetryEventDefinition,
    TelemetryParentDefinition, TelemetrySchemaDefinition, TelemetrySpanDefinition,
    TelemetryStatusDefault, TelemetryStatusRule,
};

fn sample_schema() -> TelemetrySchemaDefinition {
    let start_attributes = IndexMap::from([(
        "pi.ai.model".to_owned(),
        TelemetryAttributeDefinition::start("Model id.", TelemetryAttributeType::String)
            .required(true)
            .sensitive(false)
            .cardinality(TelemetryAttributeCardinality::Low)
            .values(TelemetryAttributeValues::Strings(vec!["gpt-4o".to_owned()])),
    )]);
    let end_attributes = IndexMap::from([(
        "pi.ai.duration_ms".to_owned(),
        TelemetryAttributeDefinition::end("Duration.", TelemetryAttributeType::Number)
            .examples(TelemetryAttributeValues::Numbers(vec![12.5])),
    )]);
    let events = IndexMap::from([(
        "pi.ai.retry".to_owned(),
        TelemetryEventDefinition {
            description: "A retry was scheduled.".to_owned(),
            attributes: IndexMap::from([(
                "attempt".to_owned(),
                TelemetryAttributeDefinition::event(
                    "Attempt number.",
                    TelemetryAttributeType::Number,
                )
                .required(true),
            )]),
        },
    )]);
    let spans = IndexMap::from([(
        "pi.ai.request".to_owned(),
        TelemetrySpanDefinition {
            description: "A provider request.".to_owned(),
            parents: TelemetryParentDefinition::RootOrExternal,
            start_attributes,
            end_attributes,
            events: Some(events),
            status: TelemetryStatusRule {
                default: TelemetryStatusDefault::Ok,
                error_when: "The provider request fails.".to_owned(),
            },
        },
    )]);
    define_telemetry_schema(TelemetrySchemaDefinition { version: 1, spans })
}

#[test]
fn schema_matches_the_upstream_json_shape() {
    let schema = sample_schema();
    let json = serde_json::to_value(&schema).expect("schema must be serializable");

    assert_eq!(json["version"], 1);
    let span = &json["spans"]["pi.ai.request"];
    assert_eq!(span["description"], "A provider request.");
    assert_eq!(span["parents"]["kind"], "root_or_external");
    assert_eq!(span["status"]["default"], "ok");
    assert_eq!(span["status"]["errorWhen"], "The provider request fails.");

    let start = &span["startAttributes"]["pi.ai.model"];
    assert_eq!(start["type"], "string");
    assert_eq!(start["description"], "Model id.");
    assert_eq!(start["required"], true);
    assert_eq!(start["sensitive"], false);
    assert_eq!(start["cardinality"], "low");
    assert_eq!(start["values"], serde_json::json!(["gpt-4o"]));

    let end = &span["endAttributes"]["pi.ai.duration_ms"];
    assert_eq!(end["type"], "number");
    assert_eq!(end["examples"], serde_json::json!([12.5]));
    // `required` only belongs to start and event attributes.
    assert!(
        end.get("required").is_none(),
        "end attributes omit required"
    );

    let event = &span["events"]["pi.ai.retry"];
    assert_eq!(event["description"], "A retry was scheduled.");
    assert_eq!(event["attributes"]["attempt"]["type"], "number");
    assert_eq!(event["attributes"]["attempt"]["required"], true);
}

#[test]
fn schema_round_trips_through_json() {
    let schema = sample_schema();
    let encoded = serde_json::to_string(&schema).expect("schema must be serializable");
    let decoded: TelemetrySchemaDefinition =
        serde_json::from_str(&encoded).expect("schema must be deserializable");
    assert_eq!(decoded, schema);
}

#[test]
fn every_parent_kind_and_value_type_round_trips() {
    for parents in [
        TelemetryParentDefinition::Any,
        TelemetryParentDefinition::RootOrExternal,
        TelemetryParentDefinition::Spans {
            spans: vec!["pi.ai.request".to_owned()],
        },
    ] {
        let encoded = serde_json::to_value(&parents).expect("parent must be serializable");
        let decoded: TelemetryParentDefinition =
            serde_json::from_value(encoded).expect("parent must be deserializable");
        assert_eq!(decoded, parents);
    }
    assert_eq!(
        serde_json::to_value(TelemetryParentDefinition::Spans {
            spans: vec!["a".to_owned()],
        })
        .expect("parent must be serializable"),
        serde_json::json!({ "kind": "spans", "spans": ["a"] })
    );

    for (value_type, encoded) in [
        (TelemetryAttributeType::String, "string"),
        (TelemetryAttributeType::Number, "number"),
        (TelemetryAttributeType::Boolean, "boolean"),
        (TelemetryAttributeType::StringArray, "string[]"),
        (TelemetryAttributeType::NumberArray, "number[]"),
        (TelemetryAttributeType::BooleanArray, "boolean[]"),
    ] {
        assert_eq!(
            serde_json::to_value(value_type).expect("type must be serializable"),
            serde_json::json!(encoded)
        );
    }

    // Untagged value lists keep their variant across a round trip.
    for values in [
        TelemetryAttributeValues::Strings(vec!["a".to_owned()]),
        TelemetryAttributeValues::Numbers(vec![1.5]),
        TelemetryAttributeValues::Booleans(vec![true]),
        TelemetryAttributeValues::StringArrays(vec![vec!["a".to_owned()]]),
        TelemetryAttributeValues::NumberArrays(vec![vec![1.5]]),
        TelemetryAttributeValues::BooleanArrays(vec![vec![true]]),
    ] {
        let encoded = serde_json::to_value(&values).expect("values must be serializable");
        let decoded: TelemetryAttributeValues =
            serde_json::from_value(encoded).expect("values must be deserializable");
        assert_eq!(decoded, values);
    }
}
