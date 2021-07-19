use synth_core::{Namespace, Name, Content};
use async_std::task;
use std::str::FromStr;
use anyhow::{Result, Context};
use log::debug;
use synth_core::schema::{FieldRef, NumberContent, Id, SameAsContent, OptionalMergeStrategy, ObjectContent, ArrayContent, RangeStep, OneOfContent, VariantContent, FieldContent};
use synth_core::schema::content::number_content::U64;
use std::convert::TryFrom;
use serde_json::Value;
use crate::cli::json::synth_val_to_json;
use crate::datasource::DataSource;
use crate::datasource::relational_datasource::{ColumnInfo, RelationalDataSource};

#[derive(Debug)]
pub(crate) struct Collection {
    pub(crate) collection: Content,
}

/// Wrapper around `FieldContent` since we cant' impl `TryFrom` on a struct in a non-owned crate
struct FieldContentWrapper(FieldContent);

pub(crate) fn build_namespace_import<T: DataSource + RelationalDataSource>(datasource: &T)
                                                                           -> Result<Namespace> {
    let table_names = task::block_on(datasource.get_table_names())
        .with_context(|| "Failed to get table names".to_string())?;

    let mut namespace = Namespace::default();

    info!("Building namespace collections...");
    populate_namespace_collections(&mut namespace, &table_names, datasource)?;

    info!("Building namespace primary keys...");
    populate_namespace_primary_keys(&mut namespace, &table_names, datasource)?;

    info!("Building namespace foreign keys...");
    populate_namespace_foreign_keys(&mut namespace, datasource)?;

    info!("Building namespace values...");
    populate_namespace_values(&mut namespace, &table_names, datasource)?;

    Ok(namespace)
}

fn populate_namespace_collections<T: DataSource + RelationalDataSource>(
    namespace: &mut Namespace, table_names: &[String], datasource: &T) -> Result<()> {
    for table_name in table_names.iter() {
        info!("Building {} collection...", table_name);

        let column_infos = task::block_on(datasource.get_columns_infos(table_name))?;

        namespace.put_collection(
            &Name::from_str(table_name)?,
            Collection::try_from((datasource, column_infos))?.collection,
        )?;
    }

    Ok(())
}

fn populate_namespace_primary_keys<T: DataSource + RelationalDataSource>(
    namespace: &mut Namespace, table_names: &[String], datasource: &T) -> Result<()> {
    for table_name in table_names.iter() {
        let primary_keys = task::block_on(datasource.get_primary_keys(table_name))?;

        if primary_keys.len() > 1 {
            bail!("{} primary keys found at collection {}. Synth does not currently support \
            composite primary keys.", primary_keys.len(), table_name)
        }

        if let Some(primary_key) = primary_keys.get(0) {
            let field = FieldRef::new(&format!(
                "{}.content.{}",
                table_name, primary_key.column_name
            ))?;
            let node = namespace.get_s_node_mut(&field)?;
            *node = Content::Number(NumberContent::U64(U64::Id(Id::default())));
        }
    }

    Ok(())
}

fn populate_namespace_foreign_keys<T: DataSource + RelationalDataSource>(
    namespace: &mut Namespace, datasource: &T) -> Result<()> {
    let foreign_keys = task::block_on(datasource.get_foreign_keys())?;

    debug!("{} foreign keys found.", foreign_keys.len());

    for fk in foreign_keys {
        let from_field =
            FieldRef::new(&format!("{}.content.{}", fk.from_table, fk.from_column))?;
        let to_field = FieldRef::new(&format!("{}.content.{}", fk.to_table, fk.to_column))?;
        let node = namespace.get_s_node_mut(&from_field)?;
        *node = Content::SameAs(SameAsContent { ref_: to_field });
    }

    Ok(())
}

fn populate_namespace_values<T: DataSource + RelationalDataSource>(
    namespace: &mut Namespace, table_names: &[String], datasource: &T) -> Result<()> {
    task::block_on(datasource.set_seed())?;

    for table in table_names {
        let values = task::block_on(datasource.get_deterministic_samples(&table))?;
        // This is temporary while we replace JSON as the core data model in namespaces.
        // namespace::try_update should take `synth_core::Value`s
        let json_values: Vec<Value> = values.into_iter().map(|v| synth_val_to_json(v)).collect();

        namespace.try_update(
            OptionalMergeStrategy,
            &Name::from_str(&table).unwrap(),
            &Value::from(json_values),
        )?;
    }

    Ok(())
}

impl<T: RelationalDataSource + DataSource> TryFrom<(&T, Vec<ColumnInfo>)> for Collection {
    type Error = anyhow::Error;

    fn try_from(columns_meta: (&T, Vec<ColumnInfo>)) -> Result<Self> {
        let mut collection = ObjectContent::default();

        for column_info in columns_meta.1 {
            let content = FieldContentWrapper::try_from((columns_meta.0, &column_info))?.0;

            collection
                .fields
                .insert(column_info.column_name.clone(), content);
        }

        Ok(Collection {
            collection: Content::Array(ArrayContent {
                length: Box::new(Content::Number(NumberContent::U64(U64::Range(RangeStep {
                    low: 1,
                    high: 2,
                    step: 1,
                })))),
                content: Box::new(Content::Object(collection)),
            }),
        })
    }
}

impl<T: RelationalDataSource + DataSource> TryFrom<(&T, &ColumnInfo)> for FieldContentWrapper {
    type Error = anyhow::Error;

    fn try_from(column_meta: (&T, &ColumnInfo)) -> Result<Self> {
        let data_type = &column_meta.1.data_type;
        let mut content = column_meta.0.decode_to_content(data_type, column_meta.1.character_maximum_length)?;

        // This happens because an `optional` field in a Synth schema
        // won't show up as a key during generation. Whereas what we
        // want instead is a null field.
        if column_meta.1.is_nullable {
            content = Content::OneOf(OneOfContent {
                variants: vec![
                    VariantContent::new(content),
                    VariantContent::new(Content::Null),
                ],
            })
        }

        Ok(FieldContentWrapper(FieldContent {
            optional: false,
            content: Box::new(content),
        }))
    }
}
