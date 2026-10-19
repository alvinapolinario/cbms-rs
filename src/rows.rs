//! Turn a `SELECT *` row into JSON the way Laravel's query builder does: integers as numbers,
//! DECIMAL as strings, dates as `YYYY-MM-DD`, datetimes as `YYYY-MM-DD HH:MM:SS`.

use chrono::{NaiveDate, NaiveDateTime};
use serde_json::{Map, Value};
use sqlx::mysql::MySqlRow;
use sqlx::{Column, Row, TypeInfo};

use crate::money::Money;

pub fn rows_to_json(rows: &[MySqlRow]) -> Vec<Value> {
    rows.iter().map(row_to_json).collect()
}

pub fn row_to_json(row: &MySqlRow) -> Value {
    let mut map = Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        map.insert(col.name().to_string(), cell_to_json(row, i));
    }
    Value::Object(map)
}

fn cell_to_json(row: &MySqlRow, i: usize) -> Value {
    let ty = row.column(i).type_info().name().to_ascii_uppercase();
    match ty.as_str() {
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" | "BIT" | "BOOLEAN" => int_cell(row, i),
        "FLOAT" | "DOUBLE" | "REAL" => match row.try_get::<Option<f64>, _>(i) {
            Ok(Some(n)) => serde_json::json!(n),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
        "DECIMAL" | "NUMERIC" | "NEWDECIMAL" => match row.try_get::<Option<Money>, _>(i) {
            Ok(Some(m)) => Value::String(m.to_db_string()),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
        "DATE" => match row.try_get::<Option<NaiveDate>, _>(i) {
            Ok(Some(d)) => Value::String(d.format("%Y-%m-%d").to_string()),
            _ => Value::Null,
        },
        "DATETIME" | "TIMESTAMP" => match row.try_get::<Option<NaiveDateTime>, _>(i) {
            Ok(Some(d)) => Value::String(d.format("%Y-%m-%d %H:%M:%S").to_string()),
            _ => Value::Null,
        },
        _ => match row.try_get::<Option<String>, _>(i) {
            Ok(Some(s)) => Value::String(s),
            Ok(None) => Value::Null,
            Err(_) => Value::Null,
        },
    }
}

fn int_cell(row: &MySqlRow, i: usize) -> Value {
    if let Ok(v) = row.try_get::<Option<i64>, _>(i) {
        return match v {
            Some(n) => serde_json::json!(n),
            None => Value::Null,
        };
    }
    if let Ok(v) = row.try_get::<Option<u64>, _>(i) {
        return match v {
            Some(n) => serde_json::json!(n),
            None => Value::Null,
        };
    }
    if let Ok(v) = row.try_get::<Option<i32>, _>(i) {
        return match v {
            Some(n) => serde_json::json!(n),
            None => Value::Null,
        };
    }
    if let Ok(v) = row.try_get::<Option<i8>, _>(i) {
        return match v {
            Some(n) => serde_json::json!(n),
            None => Value::Null,
        };
    }
    Value::Null
}
