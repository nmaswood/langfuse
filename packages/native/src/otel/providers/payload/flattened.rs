//! Rebuild nested JSON from dotted attribute names.
//!
//! With prefix `gen_ai.prompt`, `gen_ai.prompt.0.role = "user"` produces
//! `[{"role":"user"}]`. `flattened_payload` accepts paths within the nesting and array
//! allocation limits. A truthy value at the exact prefix takes precedence over dotted keys.

use std::collections::HashSet;

use super::super::super::attributes::is_js_truthy;
use super::super::super::types::JsonObject;
use super::super::super::value::{JsString, Value};

pub(super) const MAX_ARRAY_SLOTS: usize = 10_001;
const MAX_PATH_DEPTH: usize = 64;

pub(super) fn flattened_payload(attributes: &JsonObject, prefix: &str) -> Option<Value> {
    // Use a truthy exact value when present; otherwise reconstruct from dotted keys.
    if let Some(value) = attributes.get(prefix).filter(|value| is_js_truthy(value)) {
        return Some(value.clone());
    }
    let marker = format!("{prefix}.");
    let paths = attributes
        .iter()
        .filter_map(|(key, value)| {
            let parts = split_wtf8_path(&key.strip_prefix(&marker)?);
            let valid = parts.len() <= MAX_PATH_DEPTH
                && parts.iter().all(|part| {
                    !part.is_empty()
                        && !matches!(
                            part.as_utf8(),
                            Some("__proto__" | "constructor" | "prototype")
                        )
                        && canonical_array_index_js(part)
                            .is_none_or(|index| index < MAX_ARRAY_SLOTS)
                });
            valid.then_some((parts, value))
        })
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return None;
    }
    let mut root = if paths
        .iter()
        .any(|(parts, _)| parts.len() > 1 && canonical_array_index_js(&parts[0]).is_some())
    {
        Value::Array(Vec::new())
    } else {
        Value::Object(JsonObject::new())
    };
    let mut created = HashSet::new();
    let mut array_slots = 0usize;
    for (parts, value) in paths {
        insert_path(
            &mut root,
            &parts,
            value.clone(),
            &mut created,
            &[],
            &mut array_slots,
        );
    }
    Some(root)
}

fn split_wtf8_path(value: &JsString) -> Vec<JsString> {
    value
        .as_bytes()
        .split(|byte| *byte == b'.')
        .map(|part| JsString::from_wtf8(part.to_vec()).expect("WTF-8 path segment"))
        .collect()
}

fn canonical_array_index(value: &str) -> Option<usize> {
    if value == "0"
        || (value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_digit() && *byte != b'0')
            && value.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return value
            .parse::<usize>()
            .ok()
            .filter(|index| *index < usize::MAX);
    }
    None
}

fn canonical_array_index_js(value: &JsString) -> Option<usize> {
    value.as_utf8().and_then(canonical_array_index)
}

fn insert_path(
    current: &mut Value,
    parts: &[JsString],
    value: Value,
    created: &mut HashSet<Vec<JsString>>,
    current_path: &[JsString],
    array_slots: &mut usize,
) {
    if current.is_null() {
        if !created.contains(&current_path.to_vec()) {
            return;
        }
        *current = if parts.first().and_then(canonical_array_index_js).is_some() {
            Value::Array(Vec::new())
        } else {
            Value::Object(JsonObject::new())
        };
    }
    let Some(key) = parts.first() else {
        return;
    };
    let (slot, key, missing) = match current {
        Value::Object(object) => {
            let missing = !object.contains_key(key);
            (
                object.entry(key.clone()).or_insert(Value::Null),
                key.clone(),
                missing,
            )
        }
        Value::Array(array) => {
            let Some(index) =
                canonical_array_index_js(key).filter(|index| *index < MAX_ARRAY_SLOTS)
            else {
                return;
            };
            let missing = index >= array.len();
            if missing {
                let old_len = array.len();
                let additional = index + 1 - old_len;
                // Count every new array slot in `array_slots` so total allocation stays within
                // `MAX_ARRAY_SLOTS`, including across multiple sparse arrays.
                let Some(total) = array_slots.checked_add(additional) else {
                    return;
                };
                if total > MAX_ARRAY_SLOTS {
                    return;
                }
                *array_slots = total;
                array.resize_with(index + 1, || Value::Null);
                for hole in old_len..index {
                    let mut hole_path = current_path.to_vec();
                    hole_path.push(hole.to_string().into());
                    created.insert(hole_path);
                }
            }
            (&mut array[index], index.to_string().into(), missing)
        }
        _ => return,
    };
    if parts.len() == 1 {
        if slot.is_object() || slot.is_array() {
            return;
        }
        *slot = value;
        let mut slot_path = current_path.to_vec();
        slot_path.push(key);
        created.remove(&slot_path);
        return;
    }
    let mut child_path = current_path.to_vec();
    child_path.push(key.clone());
    if missing {
        *slot = if canonical_array_index_js(&parts[1]).is_some() {
            Value::Array(Vec::new())
        } else {
            Value::Object(JsonObject::new())
        };
        created.insert(child_path.clone());
    } else if !created.contains(&child_path) {
        return;
    }
    insert_path(slot, &parts[1..], value, created, &child_path, array_slots)
}
