use super::*;
#[test]
fn readonly_and_provider_identity_are_checked_before_native_admission() {
    let mut source = Source {
        dialect: Dialect::Redis,
        read_only: true,
        ..Default::default()
    };
    let read = NativeRequest::RedisRead {
        key: vec![0, 255],
        cursor: 0,
        limit: 10,
    };
    assert!(read.validate_for(&source).is_ok());
    let edit = NativeRequest::RedisCompare {
        key: vec![0, 255],
        expected: None,
        mutation: RedisMutation::SetString(vec![255, 0]),
    };
    assert!(edit.validate_for(&source).is_err());
    source.read_only = false;
    assert!(edit.validate_for(&source).is_ok());
    source.dialect = Dialect::Mongo;
    assert!(edit.validate_for(&source).is_err());
    assert!(NativeRequest::MongoCollections
        .validate_for(&source)
        .is_ok());
    source.read_only = true;
    assert!(NativeRequest::MongoDelete {
        collection: "items".into(),
        expected: serde_json::json!({"_id":1})
    }
    .validate_for(&source)
    .is_err());
}
#[test]
fn binary_record_roundtrip_and_reply_bounds_do_not_use_lossy_text() {
    let record = RedisRecord {
        key: vec![0, 255],
        kind: "hash".into(),
        ttl_ms: -1,
        value: RedisValue::Hash(vec![(vec![255], vec![0, 128, 255])]),
        next_cursor: None,
        version: Some(vec![255, 0, 1]),
    };
    let bytes = serde_json::to_vec(&record).unwrap();
    assert_eq!(
        serde_json::from_slice::<RedisRecord>(&bytes).unwrap(),
        record
    );
    assert!(bounded(NativeEvent::RedisValue(record)).is_ok());
    assert!(bounded(NativeEvent::RedisKeys {
        cursor: 0,
        keys: vec![vec![0; MAX_BYTES + 1]]
    })
    .is_err());
}
#[test]
fn session_credentials_are_redacted_in_raw_and_uri_forms() {
    let secret = "private @/pass";
    let mut uri = url::Url::parse("redis://localhost/").unwrap();
    uri.set_password(Some(secret)).unwrap();
    let message = format!("{secret} {}", uri.password().unwrap());
    let clean = redact(&message, secret);
    assert!(!clean.contains(secret));
    assert!(!clean.contains(uri.password().unwrap()));
    assert_eq!(clean, "<credential> <credential>");
}
#[test]
fn unsupported_captured_transports_fail_before_connection() {
    let mut source = Source {
        dialect: Dialect::Redis,
        host: "127.0.0.1".into(),
        port: 6379,
        ..Default::default()
    };
    let config = rustpy_net::NetConfig::default();
    assert!(validate_transport(&source, &config, &[]).is_ok());
    source.ssh = Some(Default::default());
    // Invalid empty tunnel authority is refused during validation, before
    // constructing a native worker or spawning any SSH process.
    assert!(validate_transport(&source, &config, &[]).is_err());
    source.ssh = None;
    source
        .options
        .insert("tlsAllowInvalidCertificates".into(), "true".into());
    assert!(validate_transport(&source, &config, &[]).is_err());
}
type Cleanup = Box<dyn FnOnce() -> Result<()>>;
struct Owned(NativeSession, Option<Cleanup>);
impl Owned {
    fn cleanup(&mut self) -> Result<()> {
        self.1.take().map_or(Ok(()), |cleanup| cleanup())
    }
}
fn unique_name(prefix: &str) -> String {
    format!(
        "{prefix}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
fn collection_uuid(
    client: &mongodb::sync::Client,
    database: &str,
    name: &str,
) -> Result<Option<mongodb::bson::Bson>> {
    use mongodb::bson::doc;
    let reply = client
        .database(database)
        .run_command(doc! {
            "listCollections": 1, "filter": {"name": name}, "nameOnly": false
        })
        .run()?;
    let batch = reply.get_document("cursor")?.get_array("firstBatch")?;
    ensure!(batch.len() <= 1, "Collection identity is ambiguous");
    batch
        .first()
        .map(|entry| {
            let entry = entry
                .as_document()
                .ok_or_else(|| anyhow::anyhow!("Invalid collection metadata"))?;
            ensure!(
                entry.get_str("name")? == name,
                "Collection identity changed"
            );
            entry
                .get_document("info")?
                .get("uuid")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Collection UUID missing"))
        })
        .transpose()
}
impl Drop for Owned {
    fn drop(&mut self) {
        // Clean data while the captured session still exists, including unwind.
        // Always join its worker even if identity-checked cleanup was refused.
        // A timed-out request may still be executing. Drain its captured
        // completion before cleanup so no later mutation can recreate data.
        while self.0.busy() {
            self.0.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        let cleanup = self.cleanup();
        self.0.stop();
        while self.0.alive() {
            self.0.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        if cleanup.is_err() {
            if std::thread::panicking() {
                eprintln!("Native fixture owned-data cleanup failed or identity changed; foreign data was retained");
            } else {
                panic!("Native fixture owned-data cleanup failed or identity changed; foreign data was retained");
            }
        }
    }
}
fn event(owner: &mut Owned, id: u64, request: NativeRequest) -> NativeEvent {
    owner.0.send(id, request).unwrap();
    let end = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        for reply in owner.0.poll() {
            assert_eq!(reply.session_id, owner.0.id);
            assert_eq!(reply.source_id, owner.0.source.id);
            if reply.request_id == id {
                return reply.event;
            }
            assert!(
                !matches!(reply.event, NativeEvent::Error(_) | NativeEvent::Retired),
                "unexpected startup/retirement: {:?}",
                reply.event
            );
        }
        assert!(std::time::Instant::now() < end, "native request deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn configured(dialect: Dialect, port: u16, database: &str) -> Owned {
    assert_eq!(
        std::env::var("RUSTPY_NATIVE_DISPOSABLE").as_deref(),
        Ok("yes")
    );
    Owned(
        NativeSession::start(
            7,
            Source {
                id: 11,
                dialect,
                host: "127.0.0.1".into(),
                port,
                database: database.into(),
                user: std::env::var("RUSTPY_NATIVE_USER").unwrap_or_default(),
                ..Default::default()
            },
            std::env::var("RUSTPY_NATIVE_PASSWORD").unwrap_or_default(),
            Duration::from_secs(5),
        )
        .unwrap(),
        None,
    )
}
#[test]
#[ignore = "explicit disposable loopback Redis; official driver, no provider installed by test"]
fn redis_binary_cas_conflict_and_ttl_use_real_owned_driver() {
    let mut owner = configured(Dialect::Redis, 6379, "15");
    let key = unique_name("rustpy_native").into_bytes();
    // Keep cleanup independent of the session under test, through the same
    // admitted loopback server and official Redis client.
    let mut cleanup_url = url::Url::parse("redis://127.0.0.1:6379/15").unwrap();
    cleanup_url.set_username(&owner.0.source.user).unwrap();
    let secret = std::env::var("RUSTPY_NATIVE_PASSWORD").unwrap_or_default();
    if !secret.is_empty() {
        cleanup_url.set_password(Some(&secret)).unwrap();
    }
    let cleanup_client = ::redis::Client::open(cleanup_url.as_str()).unwrap();
    let mut cleanup_connection = cleanup_client
        .get_connection_with_timeout(Duration::from_secs(5))
        .unwrap();
    cleanup_connection
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    cleanup_connection
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let existing: bool = ::redis::cmd("EXISTS")
        .arg(&key)
        .query(&mut cleanup_connection)
        .unwrap();
    assert!(!existing, "fresh owned key must not replace existing data");
    let cleanup_key = key.clone();
    owner.1 = Some(Box::new(move || {
        // The only value this fixture creates is this exact binary string.
        // Never erase a concurrently replaced key of another type/value.
        let result: i64 = ::redis::cmd("EVAL").arg(
            "local t=redis.call('TYPE',KEYS[1]).ok; if t=='none' then return 1 end; if t~='string' or redis.call('GET',KEYS[1])~=ARGV[1] then return 0 end; redis.call('DEL',KEYS[1]); return 1"
        ).arg(1).arg(&cleanup_key).arg(vec![0_u8,255,128]).query(&mut cleanup_connection)?;
        ensure!(result == 1, "Owned Redis key changed; cleanup refused");
        let exists: bool = ::redis::cmd("EXISTS")
            .arg(cleanup_key)
            .query(&mut cleanup_connection)?;
        ensure!(!exists, "Owned Redis key survived cleanup");
        Ok(())
    }));
    let request = |expected, mutation| NativeRequest::RedisCompare {
        key: key.clone(),
        expected,
        mutation,
    };
    assert!(matches!(
        event(
            &mut owner,
            1,
            request(None, RedisMutation::SetString(vec![0, 255, 128]))
        ),
        NativeEvent::Mutation { affected: 1 }
    ));
    let snapshot = match event(
        &mut owner,
        2,
        NativeRequest::RedisRead {
            key: key.clone(),
            cursor: 0,
            limit: 20,
        },
    ) {
        NativeEvent::RedisValue(record) => {
            assert_eq!(record.value, RedisValue::String(vec![0, 255, 128]));
            record.version.unwrap()
        }
        other => panic!("{other:?}"),
    };
    assert!(matches!(
        event(&mut owner, 3, request(Some(vec![0]), RedisMutation::Delete)),
        NativeEvent::Conflict
    ));
    assert!(matches!(
        event(
            &mut owner,
            4,
            request(
                Some(snapshot.clone()),
                RedisMutation::Expire { millis: 60000 }
            )
        ),
        NativeEvent::Mutation { .. }
    ));
    match event(
        &mut owner,
        5,
        NativeRequest::RedisRead {
            key: key.clone(),
            cursor: 0,
            limit: 20,
        },
    ) {
        NativeEvent::RedisValue(record) => {
            assert_eq!(record.value, RedisValue::String(vec![0, 255, 128]));
            assert!(record.ttl_ms > 0 && record.ttl_ms <= 60000);
            assert_eq!(record.version.as_ref(), Some(&snapshot));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        event(
            &mut owner,
            6,
            request(Some(snapshot), RedisMutation::Delete)
        ),
        NativeEvent::Mutation { .. }
    ));
    owner.cleanup().expect("identity-checked owned key cleanup");
}
#[test]
#[ignore = "explicit disposable loopback MongoDB; official driver, no fake protocol"]
fn mongo_extended_id_cas_and_find_use_real_owned_driver() {
    let mut owner = configured(Dialect::Mongo, 27017, "rustpy_native_disposable");
    let collection = unique_name("owned");
    let mut url = url::Url::parse("mongodb://127.0.0.1:27017/rustpy_native_disposable").unwrap();
    url.set_username(&owner.0.source.user).unwrap();
    let secret = std::env::var("RUSTPY_NATIVE_PASSWORD").unwrap_or_default();
    if !secret.is_empty() {
        url.set_password(Some(&secret)).unwrap();
    }
    url.query_pairs_mut()
        .append_pair("directConnection", "true")
        .append_pair("serverSelectionTimeoutMS", "5000")
        .append_pair("connectTimeoutMS", "5000")
        .append_pair("maxPoolSize", "1");
    let client = mongodb::sync::Client::with_uri_str(url.as_str()).unwrap();
    let database = owner.0.source.database.clone();
    assert!(
        collection_uuid(&client, &database, &collection)
            .unwrap()
            .is_none(),
        "fresh owned collection must not replace an existing collection"
    );
    client
        .database(&database)
        .create_collection(&collection)
        .run()
        .unwrap();
    let captured_uuid = collection_uuid(&client, &database, &collection);
    let cleanup_collection = collection.clone();
    let cleanup_database = database.clone();
    // Install cleanup before any further assertion or second-session setup.
    let uuid = captured_uuid.as_ref().ok().and_then(|uuid| uuid.clone());
    owner.1 = Some(Box::new(move || {
        let result = (|| {
            let current = collection_uuid(&client, &cleanup_database, &cleanup_collection)?;
            ensure!(
                uuid.is_some() && current == uuid,
                "Collection identity changed or unavailable; cleanup refused"
            );
            // UUID travels in the mutation itself: a replacement created after
            // our metadata read must be refused by the server, not dropped.
            client
                .database(&cleanup_database)
                .run_command(mongodb::bson::doc! {
                    "drop": &cleanup_collection,
                    "collectionUUID": uuid.clone().expect("UUID checked above"),
                })
                .run()?;
            ensure!(
                collection_uuid(&client, &cleanup_database, &cleanup_collection)?.is_none(),
                "Owned collection survived cleanup"
            );
            Ok(())
        })();
        client.shutdown().run();
        result
    }));
    assert!(captured_uuid
        .expect("actual created collection identity query")
        .is_some());
    // A separate official connection also supplies the concurrent-write oracle.
    let mut writer = configured(Dialect::Mongo, 27017, "rustpy_native_disposable");
    let original = serde_json::json!({"_id":{"$oid":"507f1f77bcf86cd799439011"},"n":{"$numberLong":"9007199254740993"},"width":{"$numberLong":"1"}});
    assert!(matches!(
        event(
            &mut owner,
            1,
            NativeRequest::MongoInsert {
                collection: collection.clone(),
                document: original.clone()
            }
        ),
        NativeEvent::Mutation { affected: 1 }
    ));
    let found = event(
        &mut owner,
        2,
        NativeRequest::MongoFind {
            collection: collection.clone(),
            filter: serde_json::json!({}),
            sort: serde_json::json!({}),
            skip: 0,
            limit: 10,
        },
    );
    assert!(
        matches!(found,NativeEvent::MongoDocuments{ref documents,has_more:false} if documents==&vec![original.clone()])
    );
    // The second genuine session captures and replaces the live document.
    // Only BSON width changes: semantic numeric equality alone misses this race.
    assert!(matches!(event(&mut writer, 1, NativeRequest::MongoFind {
        collection: collection.clone(), filter: serde_json::json!({}),
        sort: serde_json::json!({}), skip: 0, limit: 10,
    }), NativeEvent::MongoDocuments { ref documents, has_more: false } if documents == &vec![original.clone()]));
    let mut changed = original.clone();
    changed["width"] = serde_json::json!({"$numberInt":"1"});
    assert!(matches!(
        event(
            &mut writer,
            2,
            NativeRequest::MongoReplace {
                collection: collection.clone(),
                expected: original.clone(),
                replacement: changed.clone(),
            }
        ),
        NativeEvent::Mutation { affected: 1 }
    ));
    assert!(matches!(
        event(
            &mut owner,
            3,
            NativeRequest::MongoDelete {
                collection: collection.clone(),
                expected: original,
            }
        ),
        NativeEvent::Conflict
    ));
    assert!(matches!(event(&mut owner, 4, NativeRequest::MongoFind {
        collection: collection.clone(), filter: serde_json::json!({}),
        sort: serde_json::json!({}), skip: 0, limit: 10,
    }), NativeEvent::MongoDocuments { ref documents, has_more: false } if documents == &vec![changed.clone()]));
    assert!(matches!(
        event(
            &mut owner,
            5,
            NativeRequest::MongoDelete {
                collection: collection.clone(),
                expected: changed,
            }
        ),
        NativeEvent::Mutation { affected: 1 }
    ));
    assert!(matches!(event(&mut owner, 6, NativeRequest::MongoFind {
        collection, filter: serde_json::json!({}), sort: serde_json::json!({}), skip: 0, limit: 10,
    }), NativeEvent::MongoDocuments { ref documents, has_more: false } if documents.is_empty()));
    // Explicit success cleanup asserts collection absence; Drop does the same on panic.
    owner
        .cleanup()
        .expect("identity-checked owned collection cleanup");
}

#[test]
fn mongo_typed_cas_refuses_small_deep_documents_but_retains_nested_width_guards() {
    use mongodb::bson::{doc, Bson, Document};
    let ordinary = doc! {"_id": 7_i64, "nested": [{"wide": 1_i64, "narrow": 1_i32}]};
    let filter = mongo::compare_filter(ordinary).unwrap();
    let encoded = serde_json::to_value(&filter).unwrap();
    let text = encoded.to_string();
    assert!(text.contains("$getField") && text.contains("$arrayElemAt"));
    assert!(text.contains("long") && text.contains("int"));
    assert!(mongodb::bson::to_vec(&filter).unwrap().len() <= MAX_BYTES);
    let mut value = Bson::Int64(1);
    for _ in 0..40 {
        value = Bson::Document(doc! {"child": value});
    }
    let mut deep = Document::new();
    deep.insert("_id", 7_i64);
    deep.insert("nested", value);
    assert!(mongodb::bson::to_vec(&deep).unwrap().len() < 1024);
    assert!(
        mongo::compare_filter(deep).is_err(),
        "small untrusted nested documents must not expand into unbounded CAS filters"
    );
}
