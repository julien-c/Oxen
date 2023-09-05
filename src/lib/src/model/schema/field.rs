use serde::{Deserialize, Serialize};

use crate::model::schema::DataType;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Field {
    pub name: String,
    pub dtype: String,
    // Users can specify the dtype of the column when reading the data.
    pub dtype_override: Option<String>,
    // You can supply metadata to a column for user driven features.
    pub metadata: Option<String>,
}

impl PartialEq for Field {
    fn eq(&self, other: &Field) -> bool {
        self.name == other.name
            && self.dtype == other.dtype
            && self.metadata == other.metadata
            && self.dtype_override == other.dtype_override
    }
}

impl Field {
    pub fn new(name: &str, dtype: &str) -> Self {
        Field {
            name: name.to_owned(),
            dtype: dtype.to_owned(),
            dtype_override: None,
            metadata: None,
        }
    }

    pub fn to_sql(&self) -> String {
        let dtype = DataType::from_string(&self.dtype).to_sql();
        format!("{} {}", self.name, dtype)
    }

    pub fn all_fields_to_string<V: AsRef<Vec<Field>>>(fields: V) -> String {
        let names: Vec<String> = fields.as_ref().iter().map(|f| f.name.to_owned()).collect();

        let combined_names = names.join(", ");

        format!("[{combined_names}]")
    }

    pub fn fields_from_string(fields: &str) -> Vec<Field> {
        let mut fields_vec: Vec<Field> = vec![];
        for field in fields.split(',') {
            let field = field.trim();
            let field_parts: Vec<&str> = field.split(':').collect();
            if field_parts.len() != 2 {
                panic!("Invalid field: {}", field);
            }
            let name = field_parts[0];
            let dtype = field_parts[1];
            if DataType::from_string(dtype) == DataType::Unknown {
                panic!("Invalid dtype: {}", dtype);
            }

            let field = Field::new(name, dtype);
            fields_vec.push(field);
        }

        fields_vec
    }

    pub fn fields_to_string_with_limit<V: AsRef<Vec<Field>>>(fields: V) -> String {
        let fields = fields.as_ref();
        let max_num = 2;
        if fields.len() > max_num {
            let name_0 = fields[0].name.to_owned();
            let name_3 = fields[fields.len() - 1].name.to_owned();

            let combined_names = [name_0, String::from("..."), name_3].join(", ");
            format!("[{combined_names}]")
        } else {
            let names: Vec<String> = fields.iter().map(|f| f.name.to_owned()).collect();

            let combined_names = names.join(", ");

            format!("[{combined_names}]")
        }
    }
}
