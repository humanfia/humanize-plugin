use super::{FlowExportFormat, FlowLock};

pub(super) fn flow_export(lock: &FlowLock, format: FlowExportFormat) -> String {
    match format {
        FlowExportFormat::Json => {
            serde_json::to_string_pretty(lock).expect("flow lock serialization should not fail")
        }
        FlowExportFormat::Yaml => {
            let value =
                serde_json::to_value(lock).expect("flow lock serialization should not fail");
            let mut output = String::new();
            write_yaml_value(&value, 0, &mut output);
            output
        }
    }
}

fn write_yaml_value(value: &serde_json::Value, indent: usize, output: &mut String) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                output.push_str(&" ".repeat(indent));
                output.push_str(key);
                if is_yaml_scalar(value) {
                    output.push_str(": ");
                    output.push_str(&yaml_value(value));
                    output.push('\n');
                } else if value.as_array().is_some_and(Vec::is_empty) {
                    output.push_str(": []\n");
                } else if value.as_object().is_some_and(serde_json::Map::is_empty) {
                    output.push_str(": {}\n");
                } else {
                    output.push_str(":\n");
                    write_yaml_value(value, indent + 2, output);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                output.push_str(&" ".repeat(indent));
                output.push('-');
                if is_yaml_scalar(value) {
                    output.push(' ');
                    output.push_str(&yaml_value(value));
                    output.push('\n');
                } else {
                    output.push('\n');
                    write_yaml_value(value, indent + 2, output);
                }
            }
        }
        _ => {
            output.push_str(&" ".repeat(indent));
            output.push_str(&yaml_value(value));
            output.push('\n');
        }
    }
}

fn is_yaml_scalar(value: &serde_json::Value) -> bool {
    matches!(
        value,
        serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_)
    )
}

fn yaml_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => serde_json::to_string(value).unwrap(),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => unreachable!(),
    }
}
