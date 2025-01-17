use std::fmt;
use std::path::PathBuf;

use polars::prelude::DataFrame;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::core::df::tabular;
use crate::{
    error::OxenError,
    model::{CommitEntry, ContentType, Schema},
    opts::DFOpts,
};

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub enum ModType {
    Append,
    Delete,
    Modify,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct NewMod {
    pub content_type: ContentType,
    pub mod_type: ModType,
    pub entry: CommitEntry, // can only modify a committed file
    pub data: String,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ModEntry {
    pub uuid: String,
    pub modification_type: ModType, // append, delete, modify
    pub content_type: ContentType,  // text, json, csv
    pub schema: Option<Schema>,
    pub data: String,
    pub path: PathBuf,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
}

impl ModEntry {
    pub fn to_df(&self) -> Result<DataFrame, OxenError> {
        const ID_COL: &str = "_id";
        // right now we always have a schema, might support unstructured text/data mods later
        let schema = self.schema.clone().unwrap();
        let mut df =
            tabular::parse_data_into_df(&self.data, &schema, self.content_type.to_owned())?;
        // this puts "_id" as the last column, we want to display as the first
        df = tabular::add_col(df, ID_COL, &self.uuid, "str").unwrap();
        // Insert at first
        let mut columns = schema.fields_names();
        columns.insert(0, ID_COL.to_string());
        // Transform
        let opts = DFOpts::from_column_names(columns);
        df = tabular::transform(df, opts)?;

        Ok(df)
    }
}

impl fmt::Display for ModType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModType::Append => write!(f, "Append"),
            ModType::Delete => write!(f, "Delete"),
            ModType::Modify => write!(f, "Modify"),
        }
    }
}
