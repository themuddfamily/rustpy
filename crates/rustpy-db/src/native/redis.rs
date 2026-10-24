use super::*;
use ::redis::ConnectionLike;
use std::io::{Read, Write};
pub(super) struct Connection<W = transport::Wire> {
    wire: Option<W>,
    parser: ::redis::Parser,
    db: i64,
}
impl<W: Read + Write> Connection<W> {
    fn closed_error() -> ::redis::RedisError {
        std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "Native Redis connection is closed; open a new captured session",
        )
        .into()
    }
    fn poison(&mut self) {
        // A failed write or partial RESP read destroys reply ownership. Closing
        // the real wire is mandatory: a later command must never consume the
        // previous operation's delayed reply, even if the parser could resume.
        drop(self.wire.take());
    }
    fn send_bytes(&mut self, command: &[u8]) -> ::redis::RedisResult<()> {
        let wire = self.wire.as_mut().ok_or_else(Self::closed_error)?;
        if let Err(error) = wire.write_all(command).and_then(|_| wire.flush()) {
            self.poison();
            return Err(error.into());
        }
        Ok(())
    }
    fn response(&mut self) -> ::redis::RedisResult<::redis::Value> {
        let wire = self.wire.as_mut().ok_or_else(Self::closed_error)?;
        let value = self.parser.parse_value(wire.take((4 * MAX_BYTES) as u64));
        if value.is_err() {
            self.poison();
        }
        value
    }
}
impl<W: Read + Write> ConnectionLike for Connection<W> {
    fn req_packed_command(&mut self, command: &[u8]) -> ::redis::RedisResult<::redis::Value> {
        self.send_bytes(command)?;
        match self.response()? {
            ::redis::Value::ServerError(error) => Err(error.into()),
            value => Ok(value),
        }
    }
    fn req_packed_commands(
        &mut self,
        command: &[u8],
        offset: usize,
        count: usize,
    ) -> ::redis::RedisResult<Vec<::redis::Value>> {
        let total = offset
            .checked_add(count)
            .filter(|n| *n <= 256)
            .ok_or_else(|| {
                ::redis::RedisError::from(std::io::Error::other(
                    "Native pipeline exceeds256 replies",
                ))
            })?;
        self.send_bytes(command)?;
        let mut out = Vec::new();
        let mut error = None;
        for index in 0..total {
            let value = self.response()?;
            match value {
                ::redis::Value::ServerError(value) => {
                    error = Some(value);
                }
                value if index >= offset => out.push(value),
                _ => {}
            }
        }
        if let Some(error) = error {
            return Err(error.into());
        }
        Ok(out)
    }
    fn get_db(&self) -> i64 {
        self.db
    }
    fn check_connection(&mut self) -> bool {
        ::redis::cmd("PING").query::<String>(self).is_ok()
    }
    fn is_open(&self) -> bool {
        self.wire.is_some()
    }
}
// One server turn owns the type, page, TTL and exact value token. Separate
// DUMP/read/DUMP calls admit an A->B->A race even when their tokens agree.
const READ_SNAPSHOT: &str = r#"
local k=KEYS[1]; local kind=redis.call('TYPE',k).ok
local v=redis.call('DUMP',k); if v and #v>1048576 then return redis.error_reply('Snapshot exceeds 1 MiB') end
local ttl=redis.call('PTTL',k); local start=tonumber(ARGV[1]); local limit=tonumber(ARGV[2]); local data={}; local next=0
if (kind=='string' or kind=='list') and start>9007199254739992 then return redis.error_reply('Range exceeds exact integer precision') end
if kind=='string' then
 data=redis.call('GETRANGE',k,start,start+limit-1); if start+#data<redis.call('STRLEN',k) then next=start+#data end
elseif kind=='list' then
 data=redis.call('LRANGE',k,start,start+limit-1); if start+#data<redis.call('LLEN',k) then next=start+#data end
elseif kind=='hash' or kind=='set' or kind=='zset' then
 local cmd=({hash='HSCAN',set='SSCAN',zset='ZSCAN'})[kind]; local page=redis.call(cmd,k,ARGV[1],'COUNT',limit); next=page[1]; data=page[2]
 if #data>(kind=='set' and 1000 or 2000) then return redis.error_reply('Page exceeds 1000 rows') end
elseif kind~='none' then return redis.error_reply('Unsupported typed Redis value') end
return {kind,ttl,v or false,data,tostring(next)}
"#;
pub(super) fn connect(
    source: &Source,
    secret: &str,
    policy: transport::Policy,
) -> Result<Connection> {
    let database = if source.database.is_empty() {
        0
    } else {
        source.database.parse::<u32>()?
    };
    ensure!(
        !source.options.contains_key("authSource")
            && !source.options.contains_key("tlsCertificateKeyFile"),
        "Mongo-specific options cannot configure Redis"
    );
    let mut connection = Connection {
        wire: Some(policy.open(false)?),
        parser: ::redis::Parser::new(),
        db: database as i64,
    };
    if !source.user.is_empty() || !secret.is_empty() {
        let mut auth = ::redis::cmd("AUTH");
        if !source.user.is_empty() {
            auth.arg(&source.user);
        }
        let _: () = auth.arg(secret).query(&mut connection)?;
    }
    if database != 0 {
        let _: () = ::redis::cmd("SELECT")
            .arg(database)
            .query(&mut connection)?;
    }
    let _: String = ::redis::cmd("PING").query(&mut connection)?;
    Ok(connection)
}
pub(super) fn request(c: &mut Connection, request: NativeRequest) -> Result<NativeEvent> {
    let event = match request {
        NativeRequest::RedisScan {
            cursor,
            pattern,
            count,
        } => {
            let (cursor, keys): (u64, Vec<Vec<u8>>) = ::redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(count.clamp(1, MAX_ROWS))
                .query(c)?;
            ensure!(
                keys.len() <= MAX_ROWS,
                "Redis SCAN returned more than1000 keys; narrow pattern"
            );
            NativeEvent::RedisKeys { cursor, keys }
        }
        NativeRequest::RedisRead { key, cursor, limit } => {
            ensure!(key.len() <= MAX_BYTES, "Redis key exceeds limit");
            let limit = limit.clamp(1, MAX_ROWS);
            let (kind, ttl_ms, version, raw, next): (
                String,
                i64,
                Option<Vec<u8>>,
                ::redis::Value,
                u64,
            ) = ::redis::cmd("EVAL")
                .arg(READ_SNAPSHOT)
                .arg(1)
                .arg(&key)
                .arg(cursor)
                .arg(limit)
                .query(c)?;
            let (value, next_cursor) = match kind.as_str() {
                "none" => (RedisValue::Missing, None),
                "string" => {
                    let bytes = ::redis::from_redis_value(raw)?;
                    (RedisValue::String(bytes), (next != 0).then_some(next))
                }
                "list" => {
                    let items = ::redis::from_redis_value(raw)?;
                    (RedisValue::List(items), (next != 0).then_some(next))
                }
                "hash" => {
                    let items: Vec<(Vec<u8>, Vec<u8>)> = ::redis::from_redis_value(raw)?;
                    ensure!(
                        items.len() <= MAX_ROWS,
                        "Redis hash page exceeds row budget"
                    );
                    (RedisValue::Hash(items), (next != 0).then_some(next))
                }
                "set" => {
                    let items: Vec<Vec<u8>> = ::redis::from_redis_value(raw)?;
                    ensure!(items.len() <= MAX_ROWS, "Redis set page exceeds row budget");
                    (RedisValue::Set(items), (next != 0).then_some(next))
                }
                "zset" => {
                    let items: Vec<(Vec<u8>, f64)> = ::redis::from_redis_value(raw)?;
                    ensure!(
                        items.len() <= MAX_ROWS,
                        "Redis sorted-set page exceeds row budget"
                    );
                    (RedisValue::SortedSet(items), (next != 0).then_some(next))
                }
                _ => bail!("Redis type {kind} is not supported for typed editing"),
            };
            NativeEvent::RedisValue(RedisRecord {
                key,
                kind,
                ttl_ms,
                value,
                next_cursor,
                version,
            })
        }
        NativeRequest::RedisCompare {
            key,
            expected,
            mutation,
        } => {
            ensure!(
                key.len() <= MAX_BYTES && expected.as_ref().is_none_or(|v| v.len() <= MAX_BYTES),
                "Redis edit exceeds budget"
            );
            let (mut args, command) = match mutation {
                // Editing the value must not silently discard an existing TTL.
                RedisMutation::SetString(value) => (vec![value, b"KEEPTTL".to_vec()], "SET"),
                RedisMutation::Delete => (vec![], "DEL"),
                RedisMutation::Expire { millis } => {
                    ensure!(millis >= -1, "Choose expiration >=0 or -1 for PERSIST");
                    if millis == -1 {
                        (vec![], "PERSIST")
                    } else {
                        (vec![millis.to_string().into_bytes()], "PEXPIRE")
                    }
                }
                RedisMutation::HashSet { field, value } => (vec![field, value], "HSET"),
                RedisMutation::HashDelete { field } => (vec![field], "HDEL"),
                RedisMutation::ListSet { index, value } => {
                    (vec![index.to_string().into_bytes(), value], "LSET")
                }
                RedisMutation::SetAdd(value) => (vec![value], "SADD"),
                RedisMutation::ListPush { value, left } => {
                    (vec![value], if left { "LPUSH" } else { "RPUSH" })
                }
                RedisMutation::SetRemove(value) => (vec![value], "SREM"),
                RedisMutation::SortedSetRemove(member) => (vec![member], "ZREM"),
                RedisMutation::SortedSetAdd { member, score } => {
                    ensure!(score.is_finite(), "Sorted-set score must be finite");
                    (vec![score.to_string().into_bytes(), member], "ZADD")
                }
            };
            ensure!(
                args.iter().map(Vec::len).sum::<usize>() <= MAX_BYTES,
                "Redis mutation exceeds budget"
            );
            // Atomic compare and mutation; opaque DUMP comparison never becomes executable Lua.
            let script="local v=redis.call('DUMP',KEYS[1]); if ARGV[1]=='missing' then if v then return 0 end else if not v or v~=ARGV[2] then return 0 end end; local a={KEYS[1]}; for i=4,#ARGV do a[#a+1]=ARGV[i] end; redis.call(ARGV[3],unpack(a)); return 1";
            let mut query = ::redis::cmd("EVAL");
            query
                .arg(script)
                .arg(1)
                .arg(key)
                .arg(if expected.is_some() {
                    "present"
                } else {
                    "missing"
                })
                .arg(expected.unwrap_or_default())
                .arg(command);
            for arg in args.drain(..) {
                query.arg(arg);
            }
            let changed: i64 = query.query(c)?;
            if changed == 0 {
                NativeEvent::Conflict
            } else {
                NativeEvent::Mutation { affected: 1 }
            }
        }
        _ => bail!("This request requires a MongoDB session"),
    };
    bounded(event)
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use std::{io, sync::Mutex};

    struct Script {
        input: io::Cursor<Vec<u8>>,
        writes: Arc<Mutex<Vec<u8>>>,
        dropped: Arc<AtomicBool>,
        read_limit: usize,
        fail_write: bool,
        fail_flush: bool,
    }
    impl Read for Script {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let remaining = self
                .read_limit
                .saturating_sub(self.input.position() as usize);
            if remaining == 0 {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let limit = bytes.len().min(remaining);
            self.input.read(&mut bytes[..limit])
        }
    }
    impl Write for Script {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_write {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            self.writes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush {
                Err(io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(())
            }
        }
    }
    impl Drop for Script {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    fn scripted(input: &[u8]) -> Connection<Script> {
        Connection {
            wire: Some(Script {
                input: io::Cursor::new(input.to_vec()),
                writes: Default::default(),
                dropped: Default::default(),
                read_limit: usize::MAX,
                fail_write: false,
                fail_flush: false,
            }),
            parser: ::redis::Parser::new(),
            db: 0,
        }
    }

    #[test]
    fn partial_read_parse_write_and_flush_failures_close_and_fence_both_request_paths() {
        let command = ::redis::cmd("PING").get_packed_command();
        for failure in 0..4 {
            // The first reply is incomplete at the simulated timeout. A stale
            // complete reply remains queued behind it and must never be reused.
            let mut connection = scripted(if failure == 1 {
                b"?invalid\r\n:9\r\n"
            } else {
                b"$4\r\nPONG\r\n:9\r\n"
            });
            let wire = connection.wire.as_mut().unwrap();
            let writes = wire.writes.clone();
            let dropped = wire.dropped.clone();
            match failure {
                0 => wire.read_limit = 6,
                1 => {}
                2 => wire.fail_write = true,
                _ => wire.fail_flush = true,
            }
            assert!(connection.req_packed_command(&command).is_err());
            assert!(!connection.is_open());
            assert!(dropped.load(Ordering::Acquire));
            let before = writes.lock().unwrap().clone();
            assert!(connection.req_packed_command(&command).is_err());
            assert!(connection.req_packed_commands(&command, 0, 1).is_err());
            assert!(!connection.check_connection());
            assert_eq!(*writes.lock().unwrap(), before);
        }
    }

    #[test]
    fn pipeline_partial_reply_poisoning_does_not_publish_or_reuse_earlier_results() {
        let mut connection = scripted(b":1\r\n$4\r\nPONG\r\n:9\r\n");
        let wire = connection.wire.as_mut().unwrap();
        wire.read_limit = 10;
        let writes = wire.writes.clone();
        let dropped = wire.dropped.clone();
        assert!(connection
            .req_packed_commands(b"two commands", 0, 2)
            .is_err());
        assert!(dropped.load(Ordering::Acquire));
        let before = writes.lock().unwrap().clone();
        assert!(connection
            .req_packed_commands(b"later command", 0, 1)
            .is_err());
        assert_eq!(*writes.lock().unwrap(), before);
    }

    #[test]
    fn complete_server_errors_are_drained_without_poisoning_the_stream() {
        let mut connection = scripted(b"-ERR rejected\r\n:2\r\n:3\r\n");
        assert!(connection
            .req_packed_commands(b"two commands", 0, 2)
            .is_err());
        assert!(connection.is_open());
        assert_eq!(
            connection.req_packed_command(b"next").unwrap(),
            ::redis::Value::Int(3)
        );
    }
}
