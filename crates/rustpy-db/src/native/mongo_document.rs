use super::*;
use mongodb::bson::{oid::ObjectId, Bson};

/// Prepare an insert draft before dispatch. Retain this returned document until
/// the operation is reconciled, including after cancellation or reconnection.
/// Reusing its exact `_id` makes an uncertain insert identifiable without
/// silently issuing a second insert with another generated identity.
pub fn prepare_mongo_document(value: Value) -> Result<Value> {
    let mut document = mongo::document(value)?;
    if !document.contains_key("_id") {
        document.insert("_id", ObjectId::new());
    }
    let prepared = Bson::Document(document).into_canonical_extjson();
    ensure!(
        serde_json::to_vec(&prepared)?.len() <= MAX_BYTES,
        "Prepared Mongo document exceeds 1 MiB"
    );
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_insert_identity_survives_preparation_and_typed_values() {
        let prepared = prepare_mongo_document(serde_json::json!({
            "n":{"$numberLong":"9007199254740993"}
        }))
        .unwrap();
        assert!(prepared["_id"]["$oid"]
            .as_str()
            .is_some_and(|id| id.len() == 24));
        assert_eq!(prepared["n"]["$numberLong"], "9007199254740993");
        assert_eq!(prepare_mongo_document(prepared.clone()).unwrap(), prepared);
        let supplied = serde_json::json!({"_id":{"$numberLong":"7"},"v":"x"});
        assert_eq!(prepare_mongo_document(supplied.clone()).unwrap(), supplied);
        assert!(prepare_mongo_document(serde_json::json!([])).is_err());
    }
}
