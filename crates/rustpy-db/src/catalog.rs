//! Provider catalog SQL supplements standard ODBC table/column/key metadata.
use crate::workspace::Dialect;
pub fn extras(d: Dialect) -> Vec<(&'static str, &'static str)> {
    match d{
Dialect::Postgres|Dialect::Redshift=>vec![("ROUTINE","SELECT routine_catalog,routine_schema,routine_name FROM information_schema.routines"),("SEQUENCE","SELECT sequence_catalog,sequence_schema,sequence_name FROM information_schema.sequences"),("USER","SELECT '', '', usename FROM pg_catalog.pg_user")],
Dialect::Mysql=>vec![("ROUTINE","SELECT ROUTINE_CATALOG,ROUTINE_SCHEMA,ROUTINE_NAME FROM information_schema.ROUTINES"),("USER","SELECT '', '', User FROM mysql.user")],
Dialect::Oracle=>vec![("ROUTINE","SELECT '',OWNER,OBJECT_NAME FROM ALL_PROCEDURES WHERE PROCEDURE_NAME IS NULL"),("SEQUENCE","SELECT '',SEQUENCE_OWNER,SEQUENCE_NAME FROM ALL_SEQUENCES"),("USER","SELECT '', '', USERNAME FROM ALL_USERS")],
Dialect::SqlServer=>vec![("ROUTINE","SELECT DB_NAME(),SCHEMA_NAME(schema_id),name FROM sys.procedures"),("SEQUENCE","SELECT DB_NAME(),SCHEMA_NAME(schema_id),name FROM sys.sequences"),("USER","SELECT DB_NAME(),'',name FROM sys.database_principals")],
Dialect::Db2=>vec![("ROUTINE","SELECT '',ROUTINESCHEMA,ROUTINENAME FROM SYSCAT.ROUTINES"),("SEQUENCE","SELECT '',SEQSCHEMA,SEQNAME FROM SYSCAT.SEQUENCES")],
Dialect::Snowflake|Dialect::BigQuery|Dialect::DuckDb=>vec![("ROUTINE","SELECT routine_catalog,routine_schema,routine_name FROM information_schema.routines"),("SEQUENCE","SELECT sequence_catalog,sequence_schema,sequence_name FROM information_schema.sequences")],
Dialect::ClickHouse=>vec![("ROUTINE","SELECT '', '', name FROM system.functions"),("USER","SELECT '', '', name FROM system.users")],_=>vec![]}
}
/// Queries are provider-specific and remain explicit user requests, never startup probes.
pub fn object_details(d: Dialect, object: &crate::workspace::Object) -> Vec<(String, String)> {
    use crate::workspace::dialect_literal;
    let text = |s: &str| dialect_literal(&crate::Value::Text(s.into()), d).unwrap();
    let table = d.qualified(&object.qualified());
    let name = text(&object.name);
    let schema = text(&object.schema);
    let base = vec![(
        "Keys / checks".into(),
        format!(
            "SELECT * FROM information_schema.table_constraints WHERE table_name={name}{}",
            if object.schema.is_empty() {
                String::new()
            } else {
                format!(" AND table_schema={schema}")
            }
        ),
    )];
    match d{
Dialect::Sqlite=>vec![("Indexes".into(),format!("PRAGMA index_list({})",d.quote(&object.name))),("Foreign keys".into(),format!("PRAGMA foreign_key_list({})",d.quote(&object.name))),("Checks / triggers / DDL".into(),format!("SELECT type,name,sql FROM sqlite_master WHERE tbl_name={name}"))],
Dialect::Postgres|Dialect::Redshift=>{let mut out=base;out.push(("Indexes".into(),format!("SELECT indexname,indexdef FROM pg_indexes WHERE tablename={name} AND schemaname={schema}")));out.push(("Partitions".into(),format!("SELECT c.relname AS child FROM pg_inherits i JOIN pg_class c ON c.oid=i.inhrelid JOIN pg_class p ON p.oid=i.inhparent WHERE p.relname={name}")));out.push(("Check definitions".into(),format!("SELECT conname,pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid={name}::regclass")));out},
Dialect::Mysql=>vec![("Indexes".into(),format!("SHOW INDEX FROM {table}")),("Keys / checks / partitions / DDL".into(),format!("SHOW CREATE {} {table}",if object.kind.eq_ignore_ascii_case("VIEW"){"VIEW"}else if object.kind=="ROUTINE"{"PROCEDURE"}else{"TABLE"})),("Partitions".into(),format!("SELECT * FROM information_schema.PARTITIONS WHERE TABLE_NAME={name} AND TABLE_SCHEMA={schema}"))],
Dialect::SqlServer=>vec![("Indexes".into(),format!("SELECT i.name,i.is_unique,c.name AS column_name,ic.key_ordinal FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id=i.object_id AND ic.index_id=i.index_id JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id WHERE i.object_id=OBJECT_ID({})",text(&object.qualified()))),("Checks / keys".into(),format!("SELECT name,type_desc,OBJECT_DEFINITION(object_id) AS definition FROM sys.objects WHERE parent_object_id=OBJECT_ID({})",text(&object.qualified()))),("Partitions".into(),format!("SELECT * FROM sys.partitions WHERE object_id=OBJECT_ID({})",text(&object.qualified())))],
Dialect::Oracle=>vec![("Indexes".into(),format!("SELECT i.INDEX_NAME,i.UNIQUENESS,c.COLUMN_NAME,c.COLUMN_POSITION FROM ALL_INDEXES i JOIN ALL_IND_COLUMNS c ON c.INDEX_OWNER=i.OWNER AND c.INDEX_NAME=i.INDEX_NAME WHERE i.TABLE_NAME={name} AND i.TABLE_OWNER={schema} ORDER BY i.INDEX_NAME,c.COLUMN_POSITION")),("Keys / checks".into(),format!("SELECT * FROM ALL_CONSTRAINTS WHERE TABLE_NAME={name} AND OWNER={schema}")),("Partitions".into(),format!("SELECT * FROM ALL_TAB_PARTITIONS WHERE TABLE_NAME={name} AND TABLE_OWNER={schema}")),("DDL".into(),format!("SELECT DBMS_METADATA.GET_DDL({}, {name}, {schema}) FROM DUAL",text(if object.kind=="ROUTINE"{"PROCEDURE"}else if object.kind.eq_ignore_ascii_case("VIEW"){"VIEW"}else{"TABLE"})))],
Dialect::Db2=>vec![("Indexes".into(),format!("SELECT * FROM SYSCAT.INDEXES WHERE TABNAME={name} AND TABSCHEMA={schema}")),("Checks".into(),format!("SELECT * FROM SYSCAT.CHECKS WHERE TABNAME={name} AND TABSCHEMA={schema}")),("Keys".into(),format!("SELECT * FROM SYSCAT.TABCONST WHERE TABNAME={name} AND TABSCHEMA={schema}")),("Partitions".into(),format!("SELECT * FROM SYSCAT.DATAPARTITIONS WHERE TABNAME={name} AND TABSCHEMA={schema}"))],
Dialect::DuckDb=>vec![("Indexes".into(),format!("SELECT * FROM duckdb_indexes() WHERE table_name={name}")),("Constraints".into(),format!("SELECT * FROM duckdb_constraints() WHERE table_name={name}")),("DDL".into(),format!("SELECT sql FROM duckdb_tables() WHERE table_name={name}"))],
Dialect::ClickHouse=>vec![("Indexes / partitions / DDL".into(),format!("SELECT create_table_query,partition_key,sorting_key,primary_key FROM system.tables WHERE name={name} AND database={schema}"))],
Dialect::Snowflake=>{let mut out=base;out.push(("DDL".into(),format!("SELECT GET_DDL({}, {})",text(&object.kind),text(&object.qualified()))));out},Dialect::BigQuery=>{let mut out=base;out.push(("DDL".into(),format!("SELECT ddl FROM {}.INFORMATION_SCHEMA.TABLES WHERE table_name={name}",d.qualified(&object.schema))));out},_=>base}
}
