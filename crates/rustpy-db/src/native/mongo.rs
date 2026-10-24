use super::*;
use mongodb::{
    bson::{self, doc, Bson, Document},
    sync::Client,
};
pub(super) struct Connection {
    client: Option<Client>,
    database: String,
    timeout: Duration,
    bridge: Option<mongo_bridge::Bridge>,
    snapshots: std::collections::BTreeMap<(String, String), Document>,
}
pub(super) fn connect(
    source: &Source,
    secret: &str,
    policy: transport::Policy,
) -> Result<Connection> {
    let timeout = policy.timeout;
    ensure!(!source.database.is_empty(), "Choose a MongoDB database");
    ensure!(
        !source.options.contains_key("tlsClientCert")
            && !source.options.contains_key("tlsClientKey"),
        "Redis-specific TLS options cannot configure MongoDB"
    );
    let mut url = url::Url::parse("mongodb://localhost/")?;
    url.set_host(Some(&source.host))
        .map_err(|_| anyhow::anyhow!("Invalid MongoDB host"))?;
    url.set_port(Some(source.port))
        .map_err(|_| anyhow::anyhow!("Invalid MongoDB port"))?;
    url.set_username(&source.user)
        .map_err(|_| anyhow::anyhow!("Invalid MongoDB user"))?;
    if !secret.is_empty() {
        url.set_password(Some(secret))
            .map_err(|_| anyhow::anyhow!("Invalid MongoDB credential"))?;
    }
    url.set_path(&source.database);
    let millis = timeout.as_millis().to_string();
    let bridge = mongo_bridge::Bridge::start(policy)?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("directConnection", "true")
            .append_pair("retryWrites", "false")
            .append_pair("retryReads", "false")
            .append_pair("connectTimeoutMS", &millis)
            .append_pair("serverSelectionTimeoutMS", &millis)
            .append_pair("maxPoolSize", "1")
            .append_pair("tls", "false")
            .append_pair("proxyHost", "127.0.0.1")
            .append_pair("proxyPort", &bridge.address.port().to_string())
            .append_pair("proxyUsername", "rustpy")
            .append_pair("proxyPassword", &bridge.password);
        for (key, value) in &source.options {
            if key == "authSource" {
                query.append_pair(key, value);
            }
        }
    }
    let client = Client::with_uri_str(url.as_str())?;
    // A failed initial ping must shut down this actual driver's monitor/pool too.
    if let Err(error) = client
        .database(&source.database)
        .run_command(doc! {"ping":1})
        .run()
    {
        client.shutdown().run();
        return Err(error.into());
    }
    Ok(Connection {
        client: Some(client),
        database: source.database.clone(),
        timeout,
        bridge: Some(bridge),
        snapshots: Default::default(),
    })
}
impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(client) = self.client.take() {
            client.shutdown().run();
        }
        drop(self.bridge.take());
    }
}
pub(super) fn document(value: Value) -> Result<Document> {
    ensure!(
        serde_json::to_vec(&value)?.len() <= MAX_BYTES,
        "Mongo document exceeds 1 MiB"
    );
    match Bson::try_from(value)? {
        Bson::Document(document) => Ok(document),
        _ => bail!("Enter a BSON Extended JSON document"),
    }
}
fn collection_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.contains('\0') && !name.starts_with("system."),
        "Choose a non-system Mongo collection"
    );
    Ok(())
}
pub(super) fn compare_filter(expected: Document) -> Result<Document> {
    let id = expected
        .get("_id")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Captured Mongo document requires _id"))?;
    let mut guards = vec![Bson::Document(
        doc! {"$eq":["$$ROOT",{"$literal":expected.clone()}]},
    )];
    // Charge the outer filter, repeated _id and BSON array element headers too.
    // Sixteen bytes per guard exceeds the index/type/terminator overhead for
    // the bounded 10000-element array. Refuse before accumulating the filter.
    let mut budget = bson::to_vec(&doc! {"_id":{"$eq":id.clone()}})?.len()
        + 128
        + 16
        + bson::to_vec(guards[0].as_document().expect("literal guard"))?.len();
    ensure!(budget <= MAX_BYTES, "Mongo typed CAS filter exceeds 1 MiB");
    type_guards(
        Bson::String("$$ROOT".into()),
        &Bson::Document(expected),
        &mut guards,
        0,
        &mut budget,
    )?;
    Ok(doc! {"_id":{"$eq":id},"$expr":{"$and":guards}})
}
// Mongo equality treats some differently sized numbers as equal. Add exact
// BSON type guards at every node so Int64->Int32, nested arrays included,
// cannot pass a CAS made from the old typed snapshot. No server JavaScript.
fn type_guards(
    expression: Bson,
    expected: &Bson,
    guards: &mut Vec<Bson>,
    depth: usize,
    budget: &mut usize,
) -> Result<()> {
    ensure!(
        depth <= 32,
        "Mongo typed CAS supports nested values through depth32"
    );
    ensure!(
        guards.len() < 10000,
        "Mongo snapshot has too many typed nodes for CAS"
    );
    let kind = match expected {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Array(_) => "array",
        Bson::Document(_) => "object",
        Bson::Boolean(_) => "bool",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Timestamp(_) => "timestamp",
        Bson::Binary(_) => "binData",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Symbol(_) => "symbol",
        Bson::Decimal128(_) => "decimal",
        Bson::Undefined => "undefined",
        Bson::MaxKey => "maxKey",
        Bson::MinKey => "minKey",
        Bson::DbPointer(_) => "dbPointer",
    };
    let guard = doc! {"$eq":[{"$type":expression.clone()},kind]};
    *budget = budget.saturating_add(bson::to_vec(&guard)?.len() + 16);
    ensure!(*budget <= MAX_BYTES, "Mongo typed CAS filter exceeds 1 MiB");
    guards.push(Bson::Document(guard));
    match expected {
        Bson::Document(document) => {
            for (name, value) in document {
                let child = Bson::Document(
                    doc! {"$let":{"vars":{"p":expression.clone()},"in":{"$cond":[{"$eq":[{"$type":"$$p"},"object"]},{"$getField":{"field":{"$literal":name},"input":"$$p"}},{"$literal":Bson::Null}]}}},
                );
                type_guards(child, value, guards, depth + 1, budget)?;
            }
        }
        Bson::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                let child = Bson::Document(
                    doc! {"$let":{"vars":{"p":expression.clone()},"in":{"$cond":[{"$eq":[{"$type":"$$p"},"array"]},{"$arrayElemAt":["$$p",i64::try_from(index)?]},{"$literal":Bson::Null}]}}},
                );
                type_guards(child, value, guards, depth + 1, budget)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub(super) fn request(c: &mut Connection, request: NativeRequest) -> Result<NativeEvent> {
    let db = c
        .client
        .as_ref()
        .expect("live Mongo client")
        .database(&c.database);
    let event = match request {
        NativeRequest::MongoCollections => {
            let names = db.list_collection_names().run()?;
            ensure!(
                names.len() <= MAX_ROWS,
                "More than1000 collections; narrow this database"
            );
            NativeEvent::MongoCollections(names)
        }
        NativeRequest::MongoFind {
            collection,
            filter,
            sort,
            skip,
            limit,
        } => {
            collection_name(&collection)?;
            let coll = db.collection::<Document>(&collection);
            let limit = limit.clamp(1, MAX_ROWS);
            let mut cursor = coll
                .find(document(filter)?)
                .sort(document(sort)?)
                .skip(skip)
                .limit((limit + 1) as i64)
                .batch_size(limit as u32)
                .max_time(c.timeout)
                .run()?;
            let mut documents = Vec::new();
            let mut snapshots = std::collections::BTreeMap::new();
            let mut bytes = 0;
            let mut has_more = false;
            for result in &mut cursor {
                let document = result?;
                if documents.len() == limit {
                    has_more = true;
                    break;
                }
                let json = Bson::Document(document.clone()).into_canonical_extjson();
                snapshots.insert(
                    (collection.clone(), serde_json::to_string(&json)?),
                    document,
                );
                bytes += serde_json::to_vec(&json)?.len();
                ensure!(
                    bytes <= MAX_BYTES,
                    "Mongo page exceeds 1 MiB; lower page size"
                );
                documents.push(json);
            }
            drop(cursor);
            c.snapshots = snapshots;
            NativeEvent::MongoDocuments {
                documents,
                has_more,
            }
        }
        NativeRequest::MongoInsert {
            collection,
            document: json,
        } => {
            collection_name(&collection)?;
            let document = document(json)?;
            ensure!(
                document.contains_key("_id"),
                "Prepare and retain the Mongo insert identity before dispatch"
            );
            db.collection::<Document>(&collection)
                .insert_one(document)
                .run()?;
            NativeEvent::Mutation { affected: 1 }
        }
        NativeRequest::MongoReplace {
            collection,
            expected,
            replacement,
        } => {
            collection_name(&collection)?;
            // JSON objects need not retain BSON field order. Use the actual
            // captured BSON for whole-document equality, never a reconstructed
            // document whose sorted keys could make every CAS spuriously fail.
            let Some(expected) = c
                .snapshots
                .get(&(collection.clone(), serde_json::to_string(&expected)?))
                .cloned()
            else {
                return Ok(NativeEvent::Conflict);
            };
            let replacement = document(replacement)?;
            ensure!(
                expected.get("_id").is_some() && expected.get("_id") == replacement.get("_id"),
                "Replacement must retain the exact BSON _id"
            );
            let result = db
                .collection::<Document>(&collection)
                .replace_one(compare_filter(expected)?, replacement)
                .run()?;
            if result.matched_count == 0 {
                NativeEvent::Conflict
            } else {
                NativeEvent::Mutation {
                    affected: result.matched_count,
                }
            }
        }
        NativeRequest::MongoDelete {
            collection,
            expected,
        } => {
            collection_name(&collection)?;
            let Some(expected) = c
                .snapshots
                .get(&(collection.clone(), serde_json::to_string(&expected)?))
                .cloned()
            else {
                return Ok(NativeEvent::Conflict);
            };
            let result = db
                .collection::<Document>(&collection)
                .delete_one(compare_filter(expected)?)
                .run()?;
            if result.deleted_count == 0 {
                NativeEvent::Conflict
            } else {
                NativeEvent::Mutation {
                    affected: result.deleted_count,
                }
            }
        }
        _ => bail!("This request requires a Redis session"),
    };
    bounded(event)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_extended_json_preserves_non_string_ids_and_numeric_width() {
        let json = serde_json::json!({"_id":{"$oid":"507f1f77bcf86cd799439011"},"n":{"$numberLong":"9007199254740993"},"binary":{"$binary":{"base64":"AP8=","subType":"00"}}});
        let parsed = document(json.clone()).unwrap();
        assert!(matches!(parsed.get("_id"), Some(Bson::ObjectId(_))));
        assert_eq!(Bson::Document(parsed).into_canonical_extjson(), json);
    }
    #[test]
    fn compare_filter_quotes_expected_document_operators() {
        let original = doc! {"_id":42_i64,"nested":{"$gt":5}};
        let filter = compare_filter(original.clone()).unwrap();
        assert_eq!(
            filter.get_document("_id").unwrap().get_i64("$eq").unwrap(),
            42
        );
        assert_eq!(
            filter
                .get_document("$expr")
                .unwrap()
                .get_array("$and")
                .unwrap()[0]
                .as_document()
                .unwrap()
                .get_array("$eq")
                .unwrap()[1],
            Bson::Document(doc! {"$literal":original})
        );
        assert!(compare_filter(doc! {"no_id":1}).is_err());
        let expression = filter
            .get_document("$expr")
            .unwrap()
            .get_array("$and")
            .unwrap();
        assert!(expression.iter().any(|guard| guard
            .as_document()
            .and_then(|guard| guard.get_array("$eq").ok())
            .is_some_and(|args| args.last() == Some(&Bson::String("long".into())))));
        assert!(expression.iter().any(|guard| guard
            .as_document()
            .and_then(|guard| guard.get_array("$eq").ok())
            .is_some_and(|args| args.last() == Some(&Bson::String("int".into())))));
    }
}
