//! Versioned contracts shared by the compiler and database hosts, including WASM.

use crate::{ast::*, typecheck::Context};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt, str::FromStr};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ApiVersion(u64, u64, u64);

impl FromStr for ApiVersion {
    type Err = &'static str;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        const ERROR: &str = "expected a canonical major.minor.patch API version";
        let mut parts = input.split('.');
        let mut component = || {
            let part = parts.next().ok_or(ERROR)?;
            if part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(ERROR);
            }
            part.parse::<u64>().map_err(|_| ERROR)
        };
        let version = Self(component()?, component()?, component()?);
        if parts.next().is_some() {
            return Err(ERROR);
        }
        Ok(version)
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ApiChange {
    #[default]
    Unchanged,
    Minor,
    Major,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiTransition {
    pub from: Option<ApiVersion>,
    pub to: ApiVersion,
    pub change: ApiChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionOverflow;

impl fmt::Display for VersionOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("schema API version overflow")
    }
}

impl std::error::Error for VersionOverflow {}

/// The first finalized schema establishes 0.0.0; subsequent versions compare
/// two typechecked standalone schemas, with no dependence on SQL or rollout metadata.
pub fn transition(
    predecessor: Option<(ApiVersion, &Context)>,
    target: &Context,
) -> Result<ApiTransition, VersionOverflow> {
    let Some((from, previous)) = predecessor else {
        return Ok(ApiTransition {
            from: None,
            to: ApiVersion::default(),
            change: ApiChange::Unchanged,
        });
    };
    let change = classify(&Projection::new(previous), &Projection::new(target));
    let to = match change {
        ApiChange::Unchanged => from,
        ApiChange::Minor => ApiVersion(from.0, from.1.checked_add(1).ok_or(VersionOverflow)?, 0),
        ApiChange::Major => ApiVersion(from.0.checked_add(1).ok_or(VersionOverflow)?, 0, 0),
    };
    Ok(ApiTransition {
        from: Some(from),
        to,
        change,
    })
}

/// Fingerprint of a typechecked standalone schema. Independent of artifact hash,
/// API version, grace, and per-database deployment information.
pub fn runtime_fingerprint(context: &Context) -> String {
    let bytes =
        serde_json::to_vec(&Projection::new(context)).expect("schema projection serializes");
    let mut digest = Sha256::new();
    digest.update(b"pyre-runtime-schema\0v1\0");
    digest.update(bytes);
    format!("v1:{:x}", digest.finalize())
}

#[derive(Serialize)]
struct Projection {
    namespace: String,
    sync_mode: &'static str,
    identities: Vec<ExternalIdentity>,
    session: Fields,
    records: BTreeMap<String, Record>,
    unions: BTreeMap<String, BTreeMap<String, Fields>>,
}

type Fields = BTreeMap<String, ColumnShape>;

#[derive(Serialize)]
struct Record {
    columns: Fields,
    links: BTreeMap<String, Value>,
    permissions: Vec<Value>,
    singleton: bool,
    watched: Vec<Value>,
    table: String,
    indexes: Vec<Value>,
}

#[derive(Serialize)]
struct ColumnShape {
    type_: Value,
    nullable: bool,
    primary: bool,
    immutable: bool,
    managed: Option<&'static str>,
    default: Option<Value>,
    unique: bool,
    index: bool,
}

impl ColumnShape {
    fn omittable(&self) -> bool {
        self.nullable || self.default.is_some() || self.managed.is_some()
    }

    fn existing_change(&self, new: &Self) -> ApiChange {
        use ApiChange::*;
        if self.type_ != new.type_
            || self.nullable != new.nullable
            || self.primary != new.primary
            || self.managed != new.managed
            || (!self.immutable && new.immutable)
            || (!self.unique && new.unique)
            || (self.default.is_some() && self.default != new.default)
        {
            Major
        } else if self.immutable != new.immutable
            || self.unique != new.unique
            || self.default != new.default
        {
            Minor
        } else {
            Unchanged
        }
    }
}

impl Projection {
    fn new(context: &Context) -> Self {
        let [schema] = context.database.schemas.as_slice() else {
            panic!("schema contracts require a typechecked standalone schema");
        };
        let namespace = &schema.namespace;
        let mut identities = context.external_identities.clone();
        identities.sort_by(|a, b| identity_key(a).cmp(&identity_key(b)));
        let mut records = BTreeMap::new();
        let mut unions = BTreeMap::new();
        for definition in schema.files.iter().flat_map(|file| &file.definitions) {
            match definition {
                Definition::Record { name, fields, .. } => {
                    let mut fields = fields.clone();
                    expand_record_fields(&mut fields);
                    records.insert(name.clone(), Record::new(name, fields, namespace));
                }
                Definition::Tagged { name, variants, .. } => {
                    unions.insert(
                        name.clone(),
                        variants
                            .iter()
                            .map(|variant| {
                                (
                                    variant.name.clone(),
                                    columns(
                                        variant.fields.as_deref().unwrap_or_default(),
                                        namespace,
                                    ),
                                )
                            })
                            .collect(),
                    );
                }
                Definition::Lines { .. }
                | Definition::Comment { .. }
                | Definition::Session(_)
                | Definition::SyncMode(_) => {}
            }
        }
        Self {
            namespace: namespace.clone(),
            sync_mode: schema.sync_mode.as_str(),
            identities,
            session: columns(
                context.session.as_ref().map_or(&[], |s| &s.fields),
                namespace,
            ),
            records,
            unions,
        }
    }
}

impl Record {
    fn new(name: &str, fields: Vec<Field>, namespace: &str) -> Self {
        let record = RecordDetails {
            name: name.into(),
            fields,
            start: None,
            end: None,
            start_name: None,
            end_name: None,
        };
        let mut links = BTreeMap::new();
        let mut watched = Vec::new();
        let mut indexes = Vec::new();
        let mut singleton = false;
        for field in &record.fields {
            match field {
                Field::FieldDirective(FieldDirective::Link(link)) => {
                    links.insert(
                        link.link_name.clone(),
                        json!([
                            link.local_ids,
                            link.foreign.schema,
                            link.foreign.table,
                            link.foreign.fields
                        ]),
                    );
                }
                Field::FieldDirective(FieldDirective::Watched(watch)) => {
                    watched.push(json!([
                        watch.selects,
                        watch.inserts,
                        watch.updates,
                        watch.deletes
                    ]));
                }
                Field::FieldDirective(
                    d @ (FieldDirective::Index(index) | FieldDirective::Unique(index)),
                ) => {
                    let key: Vec<_> = index
                        .columns
                        .iter()
                        .map(|column| {
                            json!([
                                column.name,
                                match column.direction {
                                    SortDirection::Asc => "asc",
                                    SortDirection::Desc => "desc",
                                }
                            ])
                        })
                        .collect();
                    indexes.push(json!([
                        matches!(d, FieldDirective::Unique(_)),
                        key,
                        index.where_.as_ref().map(predicate)
                    ]));
                }
                Field::FieldDirective(FieldDirective::Singleton) => singleton = true,
                Field::FieldDirective(
                    FieldDirective::TableName(_)
                    | FieldDirective::Permissions(_)
                    | FieldDirective::Timestamps,
                )
                | Field::Column(_)
                | Field::ColumnLines { .. }
                | Field::ColumnComment { .. } => {}
            }
        }
        let permissions = [
            QueryOperation::Query,
            QueryOperation::Insert,
            QueryOperation::Update,
            QueryOperation::Delete,
        ]
        .iter()
        .map(|op| {
            get_permissions(&record, op)
                .as_ref()
                .map(predicate)
                .unwrap_or(Value::Null)
        })
        .collect();
        Self {
            columns: columns(&record.fields, namespace),
            links,
            permissions,
            singleton,
            watched: sorted(watched),
            table: get_tablename(name, &record.fields),
            indexes: sorted(indexes),
        }
    }
}

fn identity_key(identity: &ExternalIdentity) -> (&str, &str, &str) {
    (&identity.namespace, &identity.record, &identity.field)
}

fn columns(fields: &[Field], namespace: &str) -> Fields {
    fields
        .iter()
        .filter_map(|field| {
            let Field::Column(c) = field else {
                return None;
            };
            let mut shape = ColumnShape {
                type_: column_type(&c.type_, namespace),
                nullable: c.nullable,
                primary: false,
                immutable: false,
                managed: None,
                default: None,
                unique: false,
                index: false,
            };
            for directive in &c.directives {
                match directive {
                    ColumnDirective::PrimaryKey => shape.primary = true,
                    ColumnDirective::Immutable => shape.immutable = true,
                    ColumnDirective::CreatedAt => shape.managed = Some("createdAt"),
                    ColumnDirective::UpdatedAt => shape.managed = Some("updatedAt"),
                    ColumnDirective::Unique => shape.unique = true,
                    ColumnDirective::Index => shape.index = true,
                    ColumnDirective::Default { value, .. } => {
                        shape.default = Some(match value {
                            DefaultValue::Now => json!(["now"]),
                            DefaultValue::Value(v) => query_value(v),
                        })
                    }
                }
            }
            if is_sequence(c) {
                shape.managed = Some("sequence");
            }
            Some((c.name.clone(), shape))
        })
        .collect()
}

fn column_type(column: &ColumnType, namespace: &str) -> Value {
    match column {
        ColumnType::ForeignKey {
            schema,
            table,
            field,
            ..
        } => json!([
            "identityRef",
            schema.as_deref().unwrap_or(namespace),
            table,
            field
        ]),
        ColumnType::JsonTyped(inner) => json!(["json", column_type(inner, namespace)]),
        ColumnType::List(inner) => json!(["list", column_type(inner, namespace)]),
        ColumnType::Dict(inner) => json!(["dict", column_type(inner, namespace)]),
        ColumnType::Nullable(inner) => json!(["nullable", column_type(inner, namespace)]),
        ColumnType::IdInt { table } => json!(["Id.Int", table]),
        ColumnType::IdUuid { table } => json!(["Id.Uuid", table]),
        ColumnType::Custom(name) => json!(["custom", name]),
        primitive => json!(primitive.to_string()),
    }
}

fn sorted(mut entries: Vec<Value>) -> Vec<Value> {
    entries.sort_by_cached_key(Value::to_string);
    entries
}

fn predicate(value: &WhereArg) -> Value {
    match value {
        WhereArg::Constant(v) => json!(v),
        WhereArg::Column(session, path, op, value, _) => json!([
            "compare",
            session,
            path.segments,
            operator(op),
            query_value(value)
        ]),
        WhereArg::Exists(path, nested) => json!([
            "exists",
            path.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            predicate(nested)
        ]),
        WhereArg::And(items) => combine_predicates("and", items),
        WhereArg::Or(items) => combine_predicates("or", items),
    }
}

fn combine_predicates(kind: &str, items: &[WhereArg]) -> Value {
    let mut entries = Vec::new();
    for item in items {
        let value = predicate(item);
        if value.get(0).and_then(Value::as_str) == Some(kind) {
            entries.extend(value[1].as_array().unwrap().iter().cloned());
        } else {
            entries.push(value);
        }
    }
    json!([kind, sorted(entries)])
}

fn operator(op: &Operator) -> &'static str {
    match op {
        Operator::Equal => "eq",
        Operator::NotEqual => "ne",
        Operator::GreaterThan => "gt",
        Operator::LessThan => "lt",
        Operator::GreaterThanOrEqual => "ge",
        Operator::LessThanOrEqual => "le",
        Operator::In => "in",
        Operator::NotIn => "notIn",
        Operator::Like => "like",
        Operator::NotLike => "notLike",
    }
}

fn query_value(value: &QueryValue) -> Value {
    match value {
        QueryValue::Fn(f) => json!([
            "fn",
            f.name,
            f.args.iter().map(query_value).collect::<Vec<_>>()
        ]),
        QueryValue::LiteralTypeValue((_, tagged)) => {
            let fields: BTreeMap<_, _> = tagged
                .fields
                .iter()
                .flatten()
                .map(|(k, v)| (k, query_value(v)))
                .collect();
            json!(["variant", tagged.name, fields])
        }
        QueryValue::Variable((_, variable)) => {
            json!(["variable", variable.name, variable.session_field])
        }
        QueryValue::String((_, v)) => json!(["string", v]),
        QueryValue::Int((_, v)) => json!(["int", v]),
        QueryValue::Float((_, v)) => json!(["float32", format!("{:08x}", v.to_bits())]),
        QueryValue::Bool((_, v)) => json!(["bool", v]),
        QueryValue::Null(_) => json!(["null"]),
    }
}

fn compare_map<T>(
    old: &BTreeMap<String, T>,
    new: &BTreeMap<String, T>,
    added: impl Fn(&T) -> ApiChange,
    changed: impl Fn(&T, &T) -> ApiChange,
) -> ApiChange {
    let mut change = ApiChange::Unchanged;
    for (name, value) in old {
        let Some(target) = new.get(name) else {
            return ApiChange::Major;
        };
        change = change.max(changed(value, target));
    }
    for (name, value) in new {
        if !old.contains_key(name) {
            change = change.max(added(value));
        }
    }
    change
}

fn compare_fields(old: &Fields, new: &Fields) -> ApiChange {
    compare_map(
        old,
        new,
        |c| {
            if c.omittable() && !c.primary {
                ApiChange::Minor
            } else {
                ApiChange::Major
            }
        },
        ColumnShape::existing_change,
    )
}

fn classify(old: &Projection, new: &Projection) -> ApiChange {
    use ApiChange::*;
    if old.namespace != new.namespace || old.sync_mode != new.sync_mode {
        return Major;
    }
    let mut change = compare_fields(&old.session, &new.session);
    change = change.max(compare_map(
        &old.unions,
        &new.unions,
        |_| Minor,
        |a, b| compare_map(a, b, |_| Major, compare_fields),
    ));
    change = change.max(compare_map(
        &old.records,
        &new.records,
        |_| Minor,
        |a, b| {
            if a.singleton != b.singleton
                || a.watched != b.watched
                || a.permissions
                    .iter()
                    .zip(&b.permissions)
                    .any(|(old, new)| old != new && !new.is_null())
                || b.indexes
                    .iter()
                    .any(|index| index[0] == true && !a.indexes.contains(index))
            {
                return Major;
            }
            let mut change = compare_fields(&a.columns, &b.columns);
            change = change.max(compare_map(
                &a.links,
                &b.links,
                |_| Minor,
                |a, b| if a == b { Unchanged } else { Major },
            ));
            if a.permissions != b.permissions
                || a.indexes
                    .iter()
                    .any(|index| index[0] == true && !b.indexes.contains(index))
            {
                change = change.max(Minor);
            }
            change
        },
    ));
    let kinds = |identities: &[ExternalIdentity]| {
        identities
            .iter()
            .map(|i| (format!("{}.{}.{}", i.namespace, i.record, i.field), i.kind))
            .collect::<BTreeMap<_, _>>()
    };
    change.max(compare_map(
        &kinds(&old.identities),
        &kinds(&new.identities),
        |_| Minor,
        |a, b| if a == b { Unchanged } else { Major },
    ))
}
