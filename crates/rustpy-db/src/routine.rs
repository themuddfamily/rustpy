//! Introspected routine signatures and quoted, parameterized invocation SQL.
use crate::workspace::Dialect;
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutineKind {
    Function,
    Procedure,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutineResultShape {
    #[default]
    Unknown,
    Scalar,
    Table,
    None,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParameterMode {
    In,
    Out,
    InOut,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineParameter {
    pub name: String,
    pub data_type: String,
    pub mode: ParameterMode,
    pub ordinal: usize,
    pub has_default: bool,
    pub variadic: bool,
    /// PostgreSQL's actual type identity, quoted separately from display text.
    pub type_schema: String,
    pub type_name: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routine {
    pub kind: RoutineKind,
    #[serde(default)]
    pub result_shape: RoutineResultShape,
    pub specific_name: String,
    pub parameters: Vec<RoutineParameter>,
    /// Input arities proven unambiguous against a complete captured overload
    /// family. None (including legacy JSON) never authorizes default omission.
    #[serde(default)]
    pub allowed_default_input_counts: Option<Vec<usize>>,
    /// Captured Oracle server version, published only after the selected live
    /// ODBC connection actually returned the implicit-result sentinel.
    #[serde(default)]
    pub oracle_implicit_results: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub sql: String,
    /// Safe generated marker names; values are entered through the console's
    /// existing literal-only parameter substitution, never concatenated here.
    pub parameters: Vec<String>,
}
pub fn invocation(
    dialect: Dialect,
    schema: &str,
    name: &str,
    routine: &Routine,
    omit_defaulted: bool,
) -> Result<Invocation> {
    invocation_with_prefix(dialect, schema, name, routine, omit_defaulted, ":")
}
pub fn invocation_with_prefix(
    dialect: Dialect,
    schema: &str,
    name: &str,
    routine: &Routine,
    omit_defaulted: bool,
    prefix: &str,
) -> Result<Invocation> {
    ensure!(
        !prefix.is_empty() && prefix.len() <= 8 && prefix.chars().all(|c| ":@$?%".contains(c)),
        "Choose a SQL parameter marker prefix such as :, @, $, ? or %"
    );
    ensure!(
        !name.is_empty() && !name.contains('\0') && !schema.contains('\0'),
        "Invalid routine identity"
    );
    ensure!(
        routine.result_shape != RoutineResultShape::Unknown,
        "Refresh this routine's signature to determine its result shape before invocation"
    );
    let target = if schema.is_empty() {
        dialect.quote(name)
    } else {
        format!("{}.{}", dialect.quote(schema), dialect.quote(name))
    };
    let mut ordered = routine.parameters.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|p| p.ordinal);
    ensure!(
        ordered.windows(2).all(|w| w[0].ordinal != w[1].ordinal),
        "Duplicate routine argument ordinal"
    );
    if routine.kind == RoutineKind::Procedure
        && dialect == Dialect::Oracle
        && ordered.iter().any(|p| p.mode != ParameterMode::In)
    {
        return oracle_output_invocation(&target, routine, &ordered, omit_defaulted, prefix);
    }
    if routine.kind == RoutineKind::Procedure
        && matches!(dialect, Dialect::Mysql | Dialect::SqlServer)
        && ordered
            .iter()
            .any(|p| p.mode != ParameterMode::In || sql_server_binary(dialect, p))
    {
        return output_invocation(dialect, &target, routine, &ordered, omit_defaulted, prefix);
    }
    let mut args = Vec::new();
    let mut parameters = Vec::new();
    let mut omitted = false;
    for arg in ordered {
        if arg.mode == ParameterMode::Out {
            if routine.kind == RoutineKind::Procedure {
                ensure!(
                    matches!(dialect, Dialect::Postgres | Dialect::Redshift),
                    "This provider requires an output-variable execution binding"
                );
                ensure!(
                    !omitted,
                    "Cannot omit a positional default before an output argument"
                );
                args.push("NULL".into());
            }
            continue;
        }
        if omit_defaulted && arg.has_default {
            omitted = true;
            continue;
        }
        ensure!(!omitted, "Only trailing defaulted inputs may be omitted");
        ensure!(
            arg.mode != ParameterMode::InOut
                || matches!(dialect, Dialect::Postgres | Dialect::Redshift),
            "This provider requires an output-variable execution binding for INOUT parameters"
        );
        let marker = format!("arg_{}", arg.ordinal);
        let mut value = format!("{prefix}{marker}");
        if sql_server_binary(dialect, arg) {
            // Parameter values are SQL literals ('text', numbers, NULL); SQL
            // Server refuses implicit varchar-to-binary conversion.
            value = format!("CONVERT({}, {value})", sql_server_type(arg)?);
        }
        if matches!(dialect, Dialect::Postgres | Dialect::Redshift) && !arg.type_name.is_empty() {
            ensure!(
                !arg.type_name.contains('\0') && !arg.type_schema.contains('\0'),
                "Invalid routine type identity"
            );
            let ty = if arg.type_schema.is_empty() {
                dialect.quote(&arg.type_name)
            } else {
                format!(
                    "{}.{}",
                    dialect.quote(&arg.type_schema),
                    dialect.quote(&arg.type_name)
                )
            };
            value = format!("CAST({value} AS {ty})");
        }
        if arg.variadic {
            ensure!(
                matches!(dialect, Dialect::Postgres | Dialect::Redshift),
                "Unsupported provider variadic invocation"
            );
            value = format!("VARIADIC {value}");
        }
        args.push(value);
        parameters.push(marker);
    }
    if omitted {
        ensure!(
            routine
                .allowed_default_input_counts
                .as_ref()
                .is_some_and(|counts| counts.contains(&parameters.len())),
            "Default omission is ambiguous or its complete overload family is unavailable; supply all input arguments"
        );
    }
    let arguments = args.join(", ");
    let sql = match (routine.kind, dialect) {
        (RoutineKind::Function, Dialect::SqlServer)
            if routine.result_shape == RoutineResultShape::Table =>
        {
            format!("SELECT * FROM {target}({arguments})")
        }
        (RoutineKind::Function, Dialect::Oracle) => {
            format!("SELECT {target}({arguments}) FROM dual")
        }
        (RoutineKind::Function, Dialect::Postgres | Dialect::Redshift) => {
            format!("SELECT * FROM {target}({arguments})")
        }
        (RoutineKind::Function, _) => format!("SELECT {target}({arguments})"),
        (RoutineKind::Procedure, Dialect::SqlServer) => format!(
            "EXEC {target}{}{}",
            if args.is_empty() { "" } else { " " },
            arguments
        ),
        (RoutineKind::Procedure, Dialect::Oracle) => format!("CALL {target}({arguments})"),
        (
            RoutineKind::Procedure,
            Dialect::Sqlite
            | Dialect::Mongo
            | Dialect::Redis
            | Dialect::Cassandra
            | Dialect::DuckDb,
        ) => bail!("This dialect has no SQL procedure invocation"),
        (RoutineKind::Procedure, _) => format!("CALL {target}({arguments})"),
    };
    Ok(Invocation { sql, parameters })
}

pub(crate) const ORACLE_OUTPUT_MARKER: &str = "DECLARE /* rustpy_oracle_outputs */";
pub(crate) const ORACLE_IMPLICIT_PROBE: &str = "DECLARE c SYS_REFCURSOR; BEGIN OPEN c FOR SELECT 'rustpy_implicit_result_v1' FROM dual; DBMS_SQL.RETURN_RESULT(c); END;";

pub(crate) fn oracle_server_version_supported(version: &str) -> bool {
    version
        .split('.')
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .is_some_and(|n| n >= 12)
}

fn oracle_scalar_type(arg: &RoutineParameter) -> Result<String> {
    ensure!(
        arg.type_name.is_empty() && arg.type_schema.is_empty(),
        "Oracle object/collection output bindings require a supported native type adapter"
    );
    // Use wide PL/SQL scalar storage, not an interpolated catalog declaration.
    // Unsupported LOB, collection and cursor values never become fake scalars.
    Ok(match arg.data_type.as_str() {
        "NUMBER" | "FLOAT" | "INTEGER" | "PLS_INTEGER" | "BINARY_INTEGER" => "NUMBER",
        "BINARY_FLOAT" => "BINARY_FLOAT",
        "BINARY_DOUBLE" => "BINARY_DOUBLE",
        "DATE" => "DATE",
        "VARCHAR2" | "VARCHAR" | "CHAR" => "VARCHAR2(32767)",
        "NVARCHAR2" | "NCHAR" => "NVARCHAR2(16383)",
        "RAW" => "RAW(32767)",
        "TIMESTAMP" | "TIMESTAMP(6)" => "TIMESTAMP",
        "TIMESTAMP WITH TIME ZONE" | "TIMESTAMP(6) WITH TIME ZONE" => "TIMESTAMP WITH TIME ZONE",
        "TIMESTAMP WITH LOCAL TIME ZONE" | "TIMESTAMP(6) WITH LOCAL TIME ZONE" => {
            "TIMESTAMP WITH LOCAL TIME ZONE"
        }
        _ => bail!("Unsupported Oracle scalar output binding type"),
    }
    .into())
}

fn oracle_output_invocation(
    target: &str,
    routine: &Routine,
    ordered: &[&RoutineParameter],
    omit_defaulted: bool,
    prefix: &str,
) -> Result<Invocation> {
    ensure!(routine.oracle_implicit_results.as_deref().is_some_and(oracle_server_version_supported),
        "Refresh Oracle metadata using a server and ODBC driver with verified implicit-result support");
    let mut declarations = vec!["rustpy_cursor SYS_REFCURSOR".to_owned()];
    let mut args = Vec::new();
    let mut outputs = Vec::new();
    let mut parameters = Vec::new();
    let mut omitted = false;
    for arg in ordered {
        ensure!(!arg.variadic, "Unsupported Oracle variadic binding");
        if omit_defaulted && arg.has_default && arg.mode == ParameterMode::In {
            omitted = true;
            continue;
        }
        ensure!(
            !omitted,
            "Cannot omit a positional default before another argument"
        );
        let marker = format!("arg_{}", arg.ordinal);
        let value = if arg.mode == ParameterMode::Out {
            "NULL".into()
        } else {
            parameters.push(marker.clone());
            format!("{prefix}{marker}")
        };
        if arg.mode == ParameterMode::In {
            args.push(value);
        } else {
            let variable = format!("rustpy_out_{}", arg.ordinal);
            declarations.push(format!(
                "{variable} {} := {value}",
                oracle_scalar_type(arg)?
            ));
            args.push(variable.clone());
            outputs.push(format!("{variable} AS {}", Dialect::Oracle.quote(&marker)));
        }
    }
    if omitted {
        ensure!(
            routine
                .allowed_default_input_counts
                .as_ref()
                .is_some_and(|counts| counts.contains(&parameters.len())),
            "Default omission is ambiguous or its complete overload family is unavailable"
        );
    }
    Ok(Invocation { sql: format!("{ORACLE_OUTPUT_MARKER} {}; BEGIN {target}({}); OPEN rustpy_cursor FOR SELECT {} FROM dual; DBMS_SQL.RETURN_RESULT(rustpy_cursor); END;", declarations.join("; "), args.join(", "), outputs.join(", ")), parameters })
}

// SQL Server declarations must come from understood scalar catalog types. Display
// text is never executable SQL; in particular legacy truncated varchar metadata
// cannot silently allocate a one-character output variable.
fn sql_server_type(arg: &RoutineParameter) -> Result<String> {
    ensure!(
        arg.type_schema == "sys",
        "Refresh a supported scalar SQL Server parameter type"
    );
    let ty = arg.data_type.to_ascii_lowercase();
    let (base, suffix) = ty
        .split_once('(')
        .map_or((ty.as_str(), None), |(b, s)| (b, Some(s)));
    ensure!(
        base == arg.type_name.to_ascii_lowercase(),
        "Inconsistent SQL Server type identity"
    );
    match base {
        "bigint" | "int" | "smallint" | "tinyint" | "bit" | "real" | "float" | "money"
        | "smallmoney" | "date" | "datetime" | "smalldatetime" | "uniqueidentifier" => {
            ensure!(suffix.is_none(), "Unexpected scalar type modifier");
        }
        "varchar" | "nvarchar" | "char" | "nchar" | "binary" | "varbinary" | "decimal"
        | "numeric" | "time" | "datetime2" | "datetimeoffset" => {
            let modifier = suffix
                .and_then(|s| s.strip_suffix(')'))
                .ok_or_else(|| anyhow::anyhow!("Refresh complete SQL Server type modifiers"))?;
            let values = modifier.split(',').collect::<Vec<_>>();
            let number = |s: &str| s.parse::<u16>().ok();
            let valid = match base {
                "decimal" | "numeric" => {
                    values.len() == 2
                        && number(values[0]).is_some_and(|n| (1..=38).contains(&n))
                        && number(values[1])
                            .zip(number(values[0]))
                            .is_some_and(|(s, p)| s <= p)
                }
                "time" | "datetime2" | "datetimeoffset" => {
                    values.len() == 1 && number(values[0]).is_some_and(|n| n <= 7)
                }
                _ => {
                    values.len() == 1
                        && (number(values[0]).is_some_and(|n| {
                            n > 0
                                && n <= if matches!(base, "nvarchar" | "nchar") {
                                    4000
                                } else {
                                    8000
                                }
                        }) || values[0] == "max"
                            && matches!(base, "varchar" | "nvarchar" | "varbinary"))
                }
            };
            ensure!(valid, "Unsupported SQL Server type modifier");
        }
        _ => bail!("Unsupported SQL Server scalar output binding type"),
    }
    Ok(ty)
}

/// SQL Server binary parameters cannot receive the text literals that routine
/// parameter values are restricted to without an explicit CONVERT.
fn sql_server_binary(dialect: Dialect, arg: &RoutineParameter) -> bool {
    dialect == Dialect::SqlServer
        && arg.mode != ParameterMode::Out
        && matches!(
            arg.data_type
                .split('(')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "binary" | "varbinary"
        )
}

fn output_invocation(
    dialect: Dialect,
    target: &str,
    routine: &Routine,
    ordered: &[&RoutineParameter],
    omit_defaulted: bool,
    prefix: &str,
) -> Result<Invocation> {
    let mut parameters = Vec::new();
    let mut inputs = Vec::new();
    let mut declarations = Vec::new();
    let mut setup = Vec::new();
    let mut args = Vec::new();
    let mut outputs = Vec::new();
    let mut omitted = false;
    for arg in ordered {
        ensure!(!arg.variadic, "Unsupported provider variadic invocation");
        if omit_defaulted && arg.has_default && arg.mode == ParameterMode::In {
            omitted = true;
            continue;
        }
        ensure!(
            !omitted,
            "Cannot omit a positional default before another argument"
        );
        let marker = format!("arg_{}", arg.ordinal);
        let input = if arg.mode != ParameterMode::Out {
            parameters.push(marker.clone());
            Some(format!("{prefix}{marker}"))
        } else {
            None
        };
        if dialect == Dialect::Mysql {
            if arg.mode == ParameterMode::In {
                args.push(input.unwrap());
            } else {
                // Quoted user-variable names cannot collide with the console's
                // configurable literal markers, even when its prefix is @.
                let variable = format!("@'rustpy_out_{}'", arg.ordinal);
                setup.push(format!(
                    "SET {variable} = {}",
                    input.as_deref().unwrap_or("NULL")
                ));
                args.push(variable.clone());
                outputs.push(format!("{variable} AS {}", dialect.quote(&marker)));
            }
        } else {
            let ty = sql_server_type(arg)?;
            let variable = format!("@rustpy_arg_{}", arg.ordinal);
            if let Some(input) = input {
                if sql_server_binary(dialect, arg) {
                    // sp_executesql values must be constants and SQL Server
                    // refuses implicit varchar-to-binary conversion, so a
                    // text literal arrives as varchar and is converted to
                    // its bytes inside the batch.
                    let raw = format!("@rustpy_in_{}", arg.ordinal);
                    declarations.push(format!("{raw} varchar(max)"));
                    setup.push(format!("DECLARE {variable} {ty} = CONVERT({ty}, {raw})"));
                } else {
                    declarations.push(format!("{variable} {ty}"));
                }
                inputs.push(input);
            } else {
                setup.push(format!("DECLARE {variable} {ty}"));
            }
            args.push(format!(
                "{variable}{}",
                if arg.mode == ParameterMode::In {
                    ""
                } else {
                    " OUTPUT"
                }
            ));
            if arg.mode != ParameterMode::In {
                outputs.push(format!("{variable} AS {}", dialect.quote(&marker)));
            }
        }
    }
    if omitted {
        ensure!(
            routine
                .allowed_default_input_counts
                .as_ref()
                .is_some_and(|counts| counts.contains(&parameters.len())),
            "Default omission is ambiguous or its complete overload family is unavailable"
        );
    }
    let sql = if dialect == Dialect::Mysql {
        setup.push(format!("CALL {target}({})", args.join(", ")));
        setup.push(format!("SELECT {}", outputs.join(", ")));
        setup.join("; ")
    } else {
        setup.push(format!("EXEC {target} {}", args.join(", ")));
        if !outputs.is_empty() {
            setup.push(format!("SELECT {}", outputs.join(", ")));
        }
        let literal = |s: &str| format!("N'{}'", s.replace('\'', "''"));
        let mut sql = format!("EXEC sys.sp_executesql {}", literal(&setup.join("; ")));
        if !inputs.is_empty() {
            sql.push_str(&format!(
                ", {}, {}",
                literal(&declarations.join(", ")),
                inputs.join(", ")
            ));
        }
        sql
    };
    Ok(Invocation { sql, parameters })
}

/// Real PostgreSQL function/procedure identities, including overload signatures,
/// IN/OUT/INOUT/variadic arguments and the trailing-input default boundary.
pub(crate) const POSTGRES_SIGNATURES: &str = r#"
SELECT current_database(), n.nspname, p.proname, p.prokind::text, p.oid::text,
 COALESCE(a.ordinality,0)::text, COALESCE(p.proargnames[a.ordinality],''),
 COALESCE(p.proargmodes[a.ordinality],'i')::text,
 COALESCE(pg_catalog.format_type(a.type_oid,NULL),''),
 COALESCE(tn.nspname,''), COALESCE(t.typname,''),
 CASE WHEN a.ordinality IS NOT NULL AND COALESCE(p.proargmodes[a.ordinality],'i') IN ('i','b','v')
 AND (SELECT count(*) FROM unnest(COALESCE(p.proallargtypes,p.proargtypes::oid[])) WITH ORDINALITY x(oid,ord)
 WHERE x.ord<=a.ordinality AND COALESCE(p.proargmodes[x.ord],'i') IN ('i','b','v')) > p.pronargs-p.pronargdefaults
 THEN 'true' ELSE 'false' END,
 COALESCE(array_length(p.proallargtypes,1),p.pronargs)::text, count(*) OVER()::text,
 CASE WHEN p.prokind='p' THEN 'none' WHEN p.proretset OR rt.typtype='c' OR rt.typname='record' THEN 'table' ELSE 'scalar' END
FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
JOIN pg_catalog.pg_type rt ON rt.oid=p.prorettype
LEFT JOIN LATERAL unnest(COALESCE(p.proallargtypes,p.proargtypes::oid[])) WITH ORDINALITY a(type_oid,ordinality) ON true
LEFT JOIN pg_catalog.pg_type t ON t.oid=a.type_oid
LEFT JOIN pg_catalog.pg_namespace tn ON tn.oid=t.typnamespace
WHERE p.prokind IN ('f','p') AND n.nspname NOT IN ('pg_catalog','information_schema')
ORDER BY p.oid,a.ordinality
"#;

pub(crate) fn signature_objects(
    rows: Vec<Vec<String>>,
    schemas: &[String],
) -> Result<Vec<crate::workspace::Object>> {
    use crate::workspace::Object;
    let mut objects: std::collections::BTreeMap<String, Object> = std::collections::BTreeMap::new();
    let mut expected = std::collections::BTreeMap::new();
    let total = rows.len();
    for row in rows {
        ensure!(row.len() >= 15, "Incomplete provider routine metadata");
        ensure!(
            row[13].parse::<usize>()? == total,
            "Incomplete provider overload family (catalog row limit reached)"
        );
        if !schemas.is_empty() && !schemas.contains(&row[1]) {
            continue;
        }
        let kind = match row[3].as_str() {
            "f" => RoutineKind::Function,
            "p" => RoutineKind::Procedure,
            _ => bail!("Unknown provider routine kind"),
        };
        let result_shape = match row[14].as_str() {
            "scalar" => RoutineResultShape::Scalar,
            "table" => RoutineResultShape::Table,
            "none" => RoutineResultShape::None,
            _ => bail!("Unknown provider routine result shape"),
        };
        ensure!(
            (kind == RoutineKind::Procedure) == (result_shape == RoutineResultShape::None),
            "Inconsistent provider routine kind/result shape"
        );
        let arity: usize = row[12].parse()?;
        if let Some(prior) = expected.insert(row[4].clone(), arity) {
            ensure!(prior == arity, "Inconsistent provider routine signature");
        }
        let object = objects.entry(row[4].clone()).or_insert_with(|| Object {
            routine: Some(Routine {
                kind,
                result_shape,
                specific_name: row[4].clone(),
                parameters: Vec::new(),
                allowed_default_input_counts: None,
                oracle_implicit_results: None,
            }),
            related_ddl: Vec::new(),
            catalog: row[0].clone(),
            schema: row[1].clone(),
            name: row[2].clone(),
            kind: "ROUTINE".into(),
            columns: Vec::new(),
            keys: Vec::new(),
            ddl: "-- Use object details for the provider routine definition".into(),
        });
        let signature = object.routine.as_ref().unwrap();
        ensure!(
            signature.kind == kind && signature.result_shape == result_shape,
            "Inconsistent provider routine signature identity"
        );
        let ordinal: usize = row[5].parse()?;
        if ordinal == 0 {
            continue;
        }
        let mode = match row[7].as_str() {
            "i" | "v" => ParameterMode::In,
            "o" | "t" => ParameterMode::Out,
            "b" => ParameterMode::InOut,
            _ => bail!("Unknown provider routine parameter mode"),
        };
        object
            .routine
            .as_mut()
            .unwrap()
            .parameters
            .push(RoutineParameter {
                name: row[6].clone(),
                data_type: row[8].clone(),
                mode,
                ordinal,
                has_default: row[11] == "true",
                variadic: row[7] == "v",
                type_schema: row[9].clone(),
                type_name: row[10].clone(),
            });
    }
    for (identity, object) in &objects {
        ensure!(
            object.routine.as_ref().unwrap().parameters.len() == expected[identity],
            "Incomplete provider routine signature (catalog row limit reached)"
        );
    }
    authorize_default_omissions(&mut objects);
    Ok(objects.into_values().collect())
}

fn authorize_default_omissions(
    objects: &mut std::collections::BTreeMap<String, crate::workspace::Object>,
) {
    let mut authorizations = Vec::new();
    for (identity, object) in objects.iter() {
        let selected = object.routine.as_ref().unwrap();
        let inputs = selected
            .parameters
            .iter()
            .filter(|p| p.mode != ParameterMode::Out)
            .collect::<Vec<_>>();
        let mut allowed = Vec::new();
        for count in 0..inputs.len() {
            if !inputs[count..].iter().all(|p| p.has_default) {
                continue;
            }
            let ambiguous = objects.iter().any(|(other_id, other)| {
                if other_id == identity
                    || other.schema != object.schema
                    || other.name != object.name
                {
                    return false;
                }
                let sibling = other.routine.as_ref().unwrap();
                if sibling.kind != selected.kind {
                    return false;
                }
                let args = sibling
                    .parameters
                    .iter()
                    .filter(|p| p.mode != ParameterMode::Out)
                    .collect::<Vec<_>>();
                // A variadic sibling can also consume zero or many inputs.
                if args.iter().any(|p| p.variadic) {
                    return true;
                }
                args.len() >= count
                    && args[count..].iter().all(|p| p.has_default)
                    && args[..count]
                        .iter()
                        .zip(&inputs[..count])
                        .all(|(a, b)| a.type_schema == b.type_schema && a.type_name == b.type_name)
            });
            if !ambiguous {
                allowed.push(count);
            }
        }
        authorizations.push((identity.clone(), allowed));
    }
    for (identity, allowed) in authorizations {
        objects
            .get_mut(&identity)
            .unwrap()
            .routine
            .as_mut()
            .unwrap()
            .allowed_default_input_counts = Some(allowed);
    }
}

/// Signature queries expose actual arity; a missing/denied metadata result never
/// becomes a guessed zero-argument procedure. Optional/default availability is
/// conservative where a provider does not publish the original declaration.
pub(crate) fn metadata_query(dialect: Dialect) -> Option<&'static str> {
    match dialect {
        Dialect::Postgres => Some(POSTGRES_SIGNATURES),
        Dialect::SqlServer => Some(
            r#"
SELECT DB_NAME(),s.name,o.name,CASE WHEN o.type IN ('P','PC') THEN 'p' ELSE 'f' END,
 CAST(o.object_id AS varchar(20)),CAST(COALESCE(p.parameter_id,0) AS varchar(20)),
 COALESCE(p.name,''),CASE WHEN p.is_output=1 THEN 'b' ELSE 'i' END,
 CASE WHEN t.name IN ('varchar','char','binary','varbinary','nvarchar','nchar') THEN t.name+'('+CASE WHEN p.max_length=-1 THEN 'max' ELSE CAST(CASE WHEN t.name IN ('nvarchar','nchar') THEN p.max_length/2 ELSE p.max_length END AS varchar(10)) END+')'
 WHEN t.name IN ('decimal','numeric') THEN t.name+'('+CAST(p.precision AS varchar(10))+','+CAST(p.scale AS varchar(10))+')'
 WHEN t.name IN ('time','datetime2','datetimeoffset') THEN t.name+'('+CAST(p.scale AS varchar(10))+')'
 ELSE COALESCE(t.name,'') END,COALESCE(SCHEMA_NAME(t.schema_id),''),COALESCE(t.name,''),
 'false',CAST(COUNT(p.parameter_id) OVER(PARTITION BY o.object_id) AS varchar(20)),
 CAST(COUNT(*) OVER() AS varchar(20)),
 CASE WHEN o.type IN ('IF','TF','FT') THEN 'table' WHEN o.type IN ('P','PC') THEN 'none' ELSE 'scalar' END
FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id
LEFT JOIN sys.parameters p ON p.object_id=o.object_id AND p.parameter_id>0
LEFT JOIN sys.types t ON t.user_type_id=p.user_type_id
WHERE o.type IN ('P','PC','FN','IF','TF','FS','FT') AND o.is_ms_shipped=0
ORDER BY o.object_id,p.parameter_id
"#,
        ),
        Dialect::Mysql => Some(
            r#"
SELECT r.ROUTINE_CATALOG,r.ROUTINE_SCHEMA,r.ROUTINE_NAME,
 CASE WHEN r.ROUTINE_TYPE='FUNCTION' THEN 'f' ELSE 'p' END,
 CONCAT(r.ROUTINE_SCHEMA,'.',r.SPECIFIC_NAME,':',r.ROUTINE_TYPE),
 CAST(COALESCE(p.ORDINAL_POSITION,0) AS CHAR),COALESCE(p.PARAMETER_NAME,''),
 CASE p.PARAMETER_MODE WHEN 'OUT' THEN 'o' WHEN 'INOUT' THEN 'b' ELSE 'i' END,
 COALESCE(p.DTD_IDENTIFIER,''),'','','false',
 CAST(COUNT(p.ORDINAL_POSITION) OVER(PARTITION BY r.ROUTINE_CATALOG,r.ROUTINE_SCHEMA,r.SPECIFIC_NAME,r.ROUTINE_TYPE) AS CHAR),
 CAST(COUNT(*) OVER() AS CHAR),CASE WHEN r.ROUTINE_TYPE='FUNCTION' THEN 'scalar' ELSE 'none' END
FROM information_schema.ROUTINES r LEFT JOIN information_schema.PARAMETERS p
 ON p.SPECIFIC_CATALOG=r.ROUTINE_CATALOG AND p.SPECIFIC_SCHEMA=r.ROUTINE_SCHEMA
 AND p.SPECIFIC_NAME=r.SPECIFIC_NAME AND p.ROUTINE_TYPE=r.ROUTINE_TYPE AND p.ORDINAL_POSITION>0
ORDER BY r.ROUTINE_SCHEMA,r.SPECIFIC_NAME,p.ORDINAL_POSITION
"#,
        ),
        Dialect::Oracle => Some(
            r#"
SELECT '',p.OWNER,p.OBJECT_NAME,CASE WHEN p.OBJECT_TYPE='FUNCTION' THEN 'f' ELSE 'p' END,
 TO_CHAR(p.OBJECT_ID)||':'||TO_CHAR(p.SUBPROGRAM_ID),TO_CHAR(COALESCE(a.POSITION,0)),
 COALESCE(a.ARGUMENT_NAME,''),CASE a.IN_OUT WHEN 'OUT' THEN 'o' WHEN 'IN/OUT' THEN 'b' ELSE 'i' END,
 COALESCE(a.DATA_TYPE,''),COALESCE(a.TYPE_OWNER,''),COALESCE(a.TYPE_NAME,''),
 CASE WHEN a.DEFAULTED='Y' THEN 'true' ELSE 'false' END,
 TO_CHAR(COUNT(a.POSITION) OVER(PARTITION BY p.OBJECT_ID,p.SUBPROGRAM_ID)),TO_CHAR(COUNT(*) OVER()),
 CASE WHEN p.OBJECT_TYPE='FUNCTION' THEN 'scalar' ELSE 'none' END
FROM ALL_PROCEDURES p LEFT JOIN ALL_ARGUMENTS a ON a.OBJECT_ID=p.OBJECT_ID
 AND a.SUBPROGRAM_ID=p.SUBPROGRAM_ID AND a.DATA_LEVEL=0 AND a.POSITION>0
WHERE p.OBJECT_TYPE IN ('FUNCTION','PROCEDURE') AND p.PROCEDURE_NAME IS NULL
ORDER BY p.OBJECT_ID,p.SUBPROGRAM_ID,a.POSITION
"#,
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn arg(ordinal: usize, mode: ParameterMode, has_default: bool) -> RoutineParameter {
        RoutineParameter {
            name: format!("arg{ordinal}"),
            data_type: "integer".into(),
            mode,
            ordinal,
            has_default,
            variadic: false,
            type_schema: "pg_catalog".into(),
            type_name: "int4".into(),
        }
    }
    fn routine(kind: RoutineKind, parameters: Vec<RoutineParameter>) -> Routine {
        let allowed_default_input_counts = Some((0..parameters.len()).collect());
        Routine {
            kind,
            result_shape: if kind == RoutineKind::Procedure {
                RoutineResultShape::None
            } else {
                RoutineResultShape::Scalar
            },
            parameters,
            specific_name: "123".into(),
            allowed_default_input_counts,
            oracle_implicit_results: None,
        }
    }
    #[test]
    fn oracle_outputs_require_actual_driver_capability_and_safe_scalar_types() {
        let mut input = arg(1, ParameterMode::In, false);
        input.data_type = "NUMBER".into();
        input.type_schema.clear();
        input.type_name.clear();
        let mut output = input.clone();
        output.ordinal = 2;
        output.mode = ParameterMode::Out;
        let mut inout = output.clone();
        inout.ordinal = 3;
        inout.mode = ParameterMode::InOut;
        inout.data_type = "VARCHAR2".into();
        let mut p = routine(RoutineKind::Procedure, vec![input, output, inout]);
        assert!(invocation(Dialect::Oracle, "OWNED", "P", &p, false).is_err());
        p.oracle_implicit_results = Some("11.2.0.4.0".into());
        assert!(invocation(Dialect::Oracle, "OWNED", "P", &p, false).is_err());
        p.oracle_implicit_results = Some("19.0.0.0.0".into());
        let call = invocation_with_prefix(Dialect::Oracle, "OWNED", "P", &p, false, "@").unwrap();
        assert_eq!(call.parameters, ["arg_1", "arg_3"]);
        assert!(call.sql.contains("rustpy_out_2 NUMBER := NULL"));
        assert!(call.sql.contains("rustpy_out_3 VARCHAR2(32767) := @arg_3"));
        assert!(call.sql.contains("OPEN rustpy_cursor FOR SELECT rustpy_out_2 AS \"arg_2\", rustpy_out_3 AS \"arg_3\" FROM dual"));
        let json = serde_json::to_value(&p).unwrap();
        let mut legacy = json.as_object().unwrap().clone();
        legacy.remove("oracle_implicit_results");
        let legacy: Routine = serde_json::from_value(legacy.into()).unwrap();
        assert!(invocation(Dialect::Oracle, "OWNED", "P", &legacy, false).is_err());
        p.parameters[1].data_type = "NUMBER; END; DROP TABLE x".into();
        assert!(invocation(Dialect::Oracle, "OWNED", "P", &p, false).is_err());
        p.parameters[1].data_type = "NUMBER".into();
        p.parameters[1].type_name = "CUSTOM_OBJECT".into();
        assert!(invocation(Dialect::Oracle, "OWNED", "P", &p, false).is_err());
    }

    #[test]
    fn mysql_output_variables_reset_and_bind_only_actual_inputs() {
        let p = routine(
            RoutineKind::Procedure,
            vec![
                arg(1, ParameterMode::In, false),
                arg(2, ParameterMode::Out, false),
                arg(3, ParameterMode::InOut, false),
            ],
        );
        let call =
            invocation_with_prefix(Dialect::Mysql, "odd`schema", "p", &p, false, "@").unwrap();
        assert_eq!(call.parameters, ["arg_1", "arg_3"]);
        assert_eq!(call.sql, "SET @'rustpy_out_2' = NULL; SET @'rustpy_out_3' = @arg_3; CALL `odd``schema`.`p`(@arg_1, @'rustpy_out_2', @'rustpy_out_3'); SELECT @'rustpy_out_2' AS `arg_2`, @'rustpy_out_3' AS `arg_3`");
        let values = [
            ("arg_1".into(), "7".into()),
            ("arg_3".into(), "'owned'".into()),
        ]
        .into_iter()
        .collect();
        let bound = crate::injection::bind_user_parameters(&call.sql, "@", &values).unwrap();
        assert!(bound.contains("SET @'rustpy_out_3' = 'owned'"));
        assert_eq!(crate::split_statements(&bound).len(), 4);
        assert!(invocation(Dialect::Oracle, "s", "p", &p, false).is_err());
    }

    #[test]
    fn sql_server_pure_output_uses_local_storage_and_preserves_default_guard() {
        let mut output = arg(1, ParameterMode::Out, false);
        output.type_schema = "sys".into();
        output.type_name = "int".into();
        output.data_type = "int".into();
        let p = routine(RoutineKind::Procedure, vec![output.clone()]);
        let call = invocation(Dialect::SqlServer, "dbo", "p", &p, false).unwrap();
        assert!(call.parameters.is_empty());
        assert_eq!(call.sql, "EXEC sys.sp_executesql N'DECLARE @rustpy_arg_1 int; EXEC [dbo].[p] @rustpy_arg_1 OUTPUT; SELECT @rustpy_arg_1 AS [arg_1]'");
        let mut default = output.clone();
        default.mode = ParameterMode::In;
        default.has_default = true;
        output.ordinal = 2;
        let p = routine(RoutineKind::Procedure, vec![default, output]);
        assert!(invocation(Dialect::SqlServer, "dbo", "p", &p, true).is_err());
    }

    #[test]
    fn sql_server_binary_inputs_convert_text_literals_explicitly() {
        let mut bytes = arg(1, ParameterMode::In, false);
        bytes.type_schema = "sys".into();
        bytes.type_name = "varbinary".into();
        bytes.data_type = "varbinary(16)".into();
        let mut output = bytes.clone();
        output.ordinal = 2;
        output.mode = ParameterMode::InOut;
        let values = [
            ("arg_1".into(), "'AB'".into()),
            ("arg_2".into(), "'CD'".into()),
        ]
        .into_iter()
        .collect();
        // In-only procedure: routed through the typed sp_executesql batch.
        let p = routine(RoutineKind::Procedure, vec![bytes.clone()]);
        let call = invocation_with_prefix(Dialect::SqlServer, "dbo", "p", &p, false, "@").unwrap();
        assert_eq!(call.sql, "EXEC sys.sp_executesql N'DECLARE @rustpy_arg_1 varbinary(16) = CONVERT(varbinary(16), @rustpy_in_1); EXEC [dbo].[p] @rustpy_arg_1', N'@rustpy_in_1 varchar(max)', @arg_1");
        let p = routine(RoutineKind::Procedure, vec![bytes.clone(), output]);
        let call = invocation_with_prefix(Dialect::SqlServer, "dbo", "p", &p, false, "@").unwrap();
        assert!(call.sql.contains("DECLARE @rustpy_arg_1 varbinary(16) = CONVERT(varbinary(16), @rustpy_in_1); DECLARE @rustpy_arg_2 varbinary(16) = CONVERT(varbinary(16), @rustpy_in_2); EXEC [dbo].[p] @rustpy_arg_1, @rustpy_arg_2 OUTPUT; SELECT @rustpy_arg_2 AS [arg_2]"));
        assert!(call
            .sql
            .contains("N'@rustpy_in_1 varchar(max), @rustpy_in_2 varchar(max)'"));
        let bound = crate::injection::bind_user_parameters(&call.sql, "@", &values).unwrap();
        assert!(bound.ends_with(", 'AB', 'CD'"));
        assert_eq!(crate::split_statements(&bound).len(), 1);
        // Scalar function: an explicit CONVERT around the bound literal.
        let mut f = routine(RoutineKind::Function, vec![bytes]);
        f.result_shape = RoutineResultShape::Scalar;
        let call = invocation_with_prefix(Dialect::SqlServer, "dbo", "f", &f, false, "@").unwrap();
        assert_eq!(call.sql, "SELECT [dbo].[f](CONVERT(varbinary(16), @arg_1))");
    }

    #[test]
    fn sql_server_catalog_types_drive_output_batch_without_marker_collision() {
        let rows = vec![
            vec![
                "db",
                "dbo",
                "p'quoted",
                "p",
                "42",
                "1",
                "@text",
                "b",
                "nvarchar(120)",
                "sys",
                "nvarchar",
                "false",
                "2",
                "2",
                "none",
            ],
            vec![
                "db",
                "dbo",
                "p'quoted",
                "p",
                "42",
                "2",
                "@amount",
                "b",
                "decimal(18,4)",
                "sys",
                "decimal",
                "false",
                "2",
                "2",
                "none",
            ],
        ];
        let objects = signature_objects(
            rows.into_iter()
                .map(|r| r.into_iter().map(str::to_owned).collect())
                .collect(),
            &[],
        )
        .unwrap();
        let p = objects[0].routine.as_ref().unwrap();
        let call =
            invocation_with_prefix(Dialect::SqlServer, "dbo", "p'quoted", p, false, "@").unwrap();
        assert_eq!(call.parameters, ["arg_1", "arg_2"]);
        assert!(call
            .sql
            .contains("EXEC [dbo].[p''quoted] @rustpy_arg_1 OUTPUT, @rustpy_arg_2 OUTPUT"));
        assert!(call
            .sql
            .contains("N'@rustpy_arg_1 nvarchar(120), @rustpy_arg_2 decimal(18,4)'"));
        let values = [
            ("arg_1".into(), "'hello'".into()),
            ("arg_2".into(), "12.5".into()),
        ]
        .into_iter()
        .collect();
        let bound = crate::injection::bind_user_parameters(&call.sql, "@", &values).unwrap();
        assert!(bound.ends_with(", 'hello', 12.5"));
        assert_eq!(crate::split_statements(&bound).len(), 1);
        let mut invalid = p.clone();
        invalid.parameters[0].data_type = "nvarchar".into();
        assert!(invocation(Dialect::SqlServer, "dbo", "p", &invalid, false).is_err());
        invalid.parameters[0].data_type = "nvarchar(120); DROP TABLE x".into();
        assert!(invocation(Dialect::SqlServer, "dbo", "p", &invalid, false).is_err());
        invalid.parameters[0].data_type = "nvarchar(120)".into();
        invalid.parameters[0].type_schema = "custom".into();
        assert!(invocation(Dialect::SqlServer, "dbo", "p", &invalid, false).is_err());
    }

    #[test]
    fn function_procedure_modes_defaults_and_quoted_identity_generate_distinct_calls() {
        let f = routine(
            RoutineKind::Function,
            vec![
                arg(3, ParameterMode::Out, false),
                arg(2, ParameterMode::In, true),
                arg(1, ParameterMode::In, false),
            ],
        );
        let call = invocation(Dialect::Postgres, "odd.schema", "f\"name", &f, true).unwrap();
        assert_eq!(
            call.sql,
            "SELECT * FROM \"odd.schema\".\"f\"\"name\"(CAST(:arg_1 AS \"pg_catalog\".\"int4\"))"
        );
        assert_eq!(call.parameters, ["arg_1"]);
        let all = invocation(Dialect::Postgres, "public", "f", &f, false).unwrap();
        assert_eq!(all.parameters, ["arg_1", "arg_2"]);
        let p = routine(
            RoutineKind::Procedure,
            vec![
                arg(1, ParameterMode::In, false),
                arg(2, ParameterMode::Out, false),
                arg(3, ParameterMode::InOut, false),
            ],
        );
        let call = invocation(Dialect::Postgres, "s", "p", &p, false).unwrap();
        assert_eq!(
            call.sql,
            "CALL \"s\".\"p\"(CAST(:arg_1 AS \"pg_catalog\".\"int4\"), NULL, CAST(:arg_3 AS \"pg_catalog\".\"int4\"))"
        );
        assert_eq!(call.parameters, ["arg_1", "arg_3"]);
        assert_eq!(
            invocation(Dialect::Mysql, "s", "p", &p, false)
                .unwrap()
                .parameters,
            ["arg_1", "arg_3"]
        );
        let bad = routine(
            RoutineKind::Function,
            vec![
                arg(1, ParameterMode::In, true),
                arg(2, ParameterMode::In, false),
            ],
        );
        assert!(invocation(Dialect::Postgres, "s", "f", &bad, true).is_err());
    }
    #[test]
    fn provider_function_and_procedure_syntax_handles_zero_and_multiple_inputs() {
        let f = routine(RoutineKind::Function, Vec::new());
        assert_eq!(
            invocation(Dialect::Oracle, "s", "f", &f, false)
                .unwrap()
                .sql,
            "SELECT \"s\".\"f\"() FROM dual"
        );
        let p = routine(RoutineKind::Procedure, Vec::new());
        assert_eq!(
            invocation(Dialect::SqlServer, "dbo", "p", &p, false)
                .unwrap()
                .sql,
            "EXEC [dbo].[p]"
        );
        let p = routine(
            RoutineKind::Procedure,
            vec![
                arg(1, ParameterMode::In, false),
                arg(2, ParameterMode::In, false),
            ],
        );
        assert_eq!(
            invocation(Dialect::SqlServer, "dbo", "p", &p, false)
                .unwrap()
                .sql,
            "EXEC [dbo].[p] :arg_1, :arg_2"
        );
        assert_eq!(
            invocation(Dialect::Mysql, "s", "p", &p, false).unwrap().sql,
            "CALL `s`.`p`(:arg_1, :arg_2)"
        );
    }
    #[test]
    fn legacy_objects_and_typed_overloads_keep_metadata_and_serialization_authority() {
        let legacy = serde_json::json!({"catalog":"","schema":"s","name":"f","kind":"ROUTINE","columns":[],"keys":[],"ddl":""});
        let object: crate::workspace::Object = serde_json::from_value(legacy).unwrap();
        assert!(object.routine.is_none());
        let rows = vec![
            vec![
                "db",
                "s",
                "f",
                "f",
                "10",
                "1",
                "n",
                "i",
                "integer",
                "pg_catalog",
                "int4",
                "false",
                "1",
                "2",
                "scalar",
            ],
            vec![
                "db", "s", "f", "f", "11", "0", "", "i", "", "", "", "false", "0", "2", "scalar",
            ],
        ];
        let objects = signature_objects(
            rows.into_iter()
                .map(|r| r.into_iter().map(str::to_owned).collect())
                .collect(),
            &["s".into()],
        )
        .unwrap();
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].routine.as_ref().unwrap().parameters.len(), 1);
        assert!(objects[1].routine.as_ref().unwrap().parameters.is_empty());
        let roundtrip: crate::workspace::Object =
            serde_json::from_slice(&serde_json::to_vec(&objects[0]).unwrap()).unwrap();
        assert_eq!(roundtrip.routine, objects[0].routine);
    }
    #[test]
    fn partial_metadata_cannot_publish_an_incomplete_invocation() {
        let row = [
            "db",
            "s",
            "f",
            "f",
            "10",
            "1",
            "a",
            "i",
            "integer",
            "pg_catalog",
            "int4",
            "false",
            "2",
            "1",
            "scalar",
        ];
        assert!(
            signature_objects(vec![row.into_iter().map(str::to_owned).collect()], &[]).is_err()
        );
        let f = routine(
            RoutineKind::Function,
            vec![arg(1, ParameterMode::In, false)],
        );
        let call =
            invocation_with_prefix(Dialect::Postgres, "s", "name:arg_1", &f, false, "@").unwrap();
        assert!(call.sql.contains("\"name:arg_1\"(CAST(@arg_1"));
        let values = [("arg_1".to_owned(), "'not an integer'".to_owned())]
            .into_iter()
            .collect();
        let bound = crate::injection::bind_user_parameters(&call.sql, "@", &values).unwrap();
        assert!(bound.contains("\"name:arg_1\"(CAST('not an integer'"));
        assert!(invocation_with_prefix(Dialect::Postgres, "s", "f", &f, false, "';").is_err());
    }
    #[test]
    fn same_prefix_overload_blocks_default_omission_without_losing_full_signature() {
        let rows = [
            [
                "db",
                "s",
                "f",
                "f",
                "10",
                "1",
                "a",
                "i",
                "integer",
                "pg_catalog",
                "int4",
                "false",
                "2",
                "3",
                "scalar",
            ],
            [
                "db",
                "s",
                "f",
                "f",
                "10",
                "2",
                "b",
                "i",
                "integer",
                "pg_catalog",
                "int4",
                "true",
                "2",
                "3",
                "scalar",
            ],
            [
                "db",
                "s",
                "f",
                "f",
                "11",
                "1",
                "a",
                "i",
                "integer",
                "pg_catalog",
                "int4",
                "false",
                "1",
                "3",
                "scalar",
            ],
        ];
        let objects = signature_objects(
            rows.into_iter()
                .map(|r| r.into_iter().map(str::to_owned).collect())
                .collect(),
            &[],
        )
        .unwrap();
        let selected = objects
            .iter()
            .find(|o| o.routine.as_ref().unwrap().specific_name == "10")
            .unwrap()
            .routine
            .as_ref()
            .unwrap();
        assert!(invocation(Dialect::Postgres, "s", "f", selected, true).is_err());
        assert_eq!(
            invocation(Dialect::Postgres, "s", "f", selected, false)
                .unwrap()
                .parameters,
            ["arg_1", "arg_2"]
        );
        let mut unknown = selected.clone();
        unknown.allowed_default_input_counts = None;
        assert!(invocation(Dialect::Postgres, "s", "f", &unknown, true).is_err());
    }
    #[test]
    fn provider_zero_arity_requires_real_metadata_and_keeps_exec_route() {
        let zero = [
            "db", "dbo", "work", "p", "125", "0", "", "i", "", "", "", "false", "0", "1", "none",
        ];
        let objects =
            signature_objects(vec![zero.into_iter().map(str::to_owned).collect()], &[]).unwrap();
        let routine = objects[0].routine.as_ref().unwrap();
        assert!(routine.parameters.is_empty());
        assert_eq!(
            invocation(Dialect::SqlServer, "dbo", "work", routine, false)
                .unwrap()
                .sql,
            "EXEC [dbo].[work]"
        );
        let missing = [
            "db", "dbo", "work", "p", "125", "0", "", "i", "", "", "", "false",
        ];
        assert!(
            signature_objects(vec![missing.into_iter().map(str::to_owned).collect()], &[]).is_err()
        );
        for provider in [Dialect::SqlServer, Dialect::Mysql, Dialect::Oracle] {
            assert!(metadata_query(provider).is_some());
        }
        let call = invocation(Dialect::Oracle, "APP", "WORK", routine, false).unwrap();
        assert_eq!(crate::split_statements(&call.sql).len(), 1);
        assert_eq!(call.sql, "CALL \"APP\".\"WORK\"()");
    }
}

#[cfg(test)]
mod table_function_tests {
    use super::*;
    #[test]
    fn sqlserver_table_functions_keep_zero_and_multiple_argument_table_calls() {
        let rows = [
            [
                "db",
                "odd]schema",
                "zero]items",
                "f",
                "200",
                "0",
                "",
                "i",
                "",
                "",
                "",
                "false",
                "0",
                "3",
                "table",
            ],
            [
                "db", "dbo", "items", "f", "201", "1", "@low", "i", "int", "sys", "int", "false",
                "2", "3", "table",
            ],
            [
                "db", "dbo", "items", "f", "201", "2", "@label", "i", "nvarchar", "sys",
                "nvarchar", "false", "2", "3", "table",
            ],
        ];
        let objects = signature_objects(
            rows.into_iter()
                .map(|r| r.into_iter().map(str::to_owned).collect())
                .collect(),
            &[],
        )
        .unwrap();
        let zero = objects.iter().find(|o| o.name == "zero]items").unwrap();
        let signature = zero.routine.as_ref().unwrap();
        assert_eq!(signature.result_shape, RoutineResultShape::Table);
        let call = invocation(
            Dialect::SqlServer,
            &zero.schema,
            &zero.name,
            signature,
            false,
        )
        .unwrap();
        assert_eq!(call.sql, "SELECT * FROM [odd]]schema].[zero]]items]()");
        assert!(call.parameters.is_empty());
        let items = objects.iter().find(|o| o.name == "items").unwrap();
        let signature = items.routine.as_ref().unwrap();
        let call = invocation(
            Dialect::SqlServer,
            &items.schema,
            &items.name,
            signature,
            false,
        )
        .unwrap();
        assert_eq!(call.sql, "SELECT * FROM [dbo].[items](:arg_1, :arg_2)");
        let values = [
            ("arg_1".into(), "3".into()),
            ("arg_2".into(), "'quote '''' ; stays data'".into()),
        ]
        .into_iter()
        .collect();
        let bound = crate::injection::bind_user_parameters(&call.sql, ":", &values).unwrap();
        assert_eq!(
            bound,
            "SELECT * FROM [dbo].[items](3, 'quote '''' ; stays data')"
        );
        let mut scalar = signature.clone();
        scalar.result_shape = RoutineResultShape::Scalar;
        assert_eq!(
            invocation(Dialect::SqlServer, "dbo", "scalar", &scalar, false)
                .unwrap()
                .sql,
            "SELECT [dbo].[scalar](:arg_1, :arg_2)"
        );
        let restored: Routine =
            serde_json::from_slice(&serde_json::to_vec(signature).unwrap()).unwrap();
        assert_eq!(restored.result_shape, RoutineResultShape::Table);
        let mut legacy = serde_json::to_value(signature).unwrap();
        legacy.as_object_mut().unwrap().remove("result_shape");
        let legacy: Routine = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.result_shape, RoutineResultShape::Unknown);
        assert!(invocation(Dialect::SqlServer, "dbo", "items", &legacy, false).is_err());
    }
}
