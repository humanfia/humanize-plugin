use serde::Serialize;
use serde_json::{Value, json};

#[cfg(test)]
use super::registry::advertised_spec_for;
use super::registry::{
    CallerKind, ToolCategory, ToolSpec, advertised_spec, advertised_specs, advertised_specs_for,
    advertised_specs_in,
};

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct McpSurface;

impl McpSurface {
    pub fn runtime_tools(&self) -> Vec<McpToolDescriptor> {
        advertised_specs_in(ToolCategory::Runtime)
            .map(descriptor_for)
            .collect()
    }

    pub fn authoring_tools(&self) -> Vec<McpToolDescriptor> {
        advertised_specs_in(ToolCategory::Authoring)
            .map(descriptor_for)
            .collect()
    }

    pub fn review_tools(&self) -> Vec<McpToolDescriptor> {
        advertised_specs_in(ToolCategory::Review)
            .map(descriptor_for)
            .collect()
    }

    pub fn tools(&self) -> Vec<McpToolDescriptor> {
        advertised_specs().map(descriptor_for).collect()
    }

    pub fn lookup(&self, name: &str) -> Option<McpToolDescriptor> {
        advertised_spec(name).map(descriptor_for)
    }

    pub fn tools_list_json(&self) -> Value {
        json!({ "tools": self.tools() })
    }

    pub(super) fn tools_list_json_for(&self, caller: CallerKind) -> Value {
        json!({
            "tools": advertised_specs_for(caller)
                .map(|spec| descriptor_for_caller(spec, caller))
                .collect::<Vec<_>>()
        })
    }

    #[cfg(test)]
    pub(super) fn lookup_for(&self, name: &str, caller: CallerKind) -> Option<McpToolDescriptor> {
        advertised_spec_for(name, caller).map(|spec| descriptor_for_caller(spec, caller))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpToolDescriptor {
    name: &'static str,
    description: &'static str,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

impl McpToolDescriptor {
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn description(&self) -> &'static str {
        self.description
    }

    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }
}

fn descriptor_for(spec: &ToolSpec) -> McpToolDescriptor {
    descriptor_for_caller(spec, CallerKind::Operator)
}

fn descriptor_for_caller(spec: &ToolSpec, caller: CallerKind) -> McpToolDescriptor {
    McpToolDescriptor {
        name: spec.name,
        description: spec.description,
        input_schema: spec.input_schema_for(caller),
    }
}
