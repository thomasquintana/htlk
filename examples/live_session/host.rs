//! Exact per-command dispatch grants and explicit host-only endpoint secrets.

use super::model::{Result, field, text};
use htlk::runtime::{
    Authorization, HostContext, RuntimePolicy,
    failure::{FailureCode, RuntimeFailure},
    mcp::{McpEndpoint, McpPolicy},
    records::SecurityLabels,
};
use htlk_executable::{
    Identifier, JsonDocument, McpBinding, McpBindingKind,
    cbor::{Limits, Value},
    digest::Digest,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

pub(super) type Grant = Arc<Mutex<Option<(String, Digest)>>>;
pub(super) struct Policy {
    pub deployment: String,
    pub grant: Grant,
}

impl RuntimePolicy for Policy {
    fn authorize(
        &self,
        caller: &HostContext,
        action: Authorization<'_>,
    ) -> std::result::Result<(), RuntimeFailure> {
        let denied = || RuntimeFailure::new(FailureCode::Policy);
        if caller.principal() != "live-session-owner" {
            return Err(denied());
        }
        if let Authorization::DispatchMcp {
            binding,
            parameters,
            ..
        } = action
        {
            let McpBindingKind::Tool { name, .. } = binding.kind() else {
                return Err(denied());
            };
            let arguments = field(parameters, "arguments").map_err(|_| denied())?;
            let digest = JsonDocument::from_value(arguments, &Limits::default())
                .map_err(|_| denied())?
                .digest();
            if binding.server().deployment_id() != self.deployment
                || self.grant.lock().map_err(|_| denied())?.as_ref()
                    != Some(&(name.clone(), digest))
            {
                return Err(denied());
            }
        }
        Ok(())
    }

    fn input_labels(
        &self,
        _: &HostContext,
        _: Digest,
        _: &Identifier,
        _: Option<&Value>,
    ) -> std::result::Result<SecurityLabels, RuntimeFailure> {
        Ok(labels())
    }

    fn mcp_policy(
        &self,
        _: &HostContext,
        _: &McpBinding,
    ) -> std::result::Result<McpPolicy, RuntimeFailure> {
        Ok(McpPolicy {
            result_labels: labels(),
            policy_version: "live-session/1".into(),
            ..Default::default()
        })
    }
}

fn labels() -> SecurityLabels {
    SecurityLabels::new(vec!["live-evidence".into()], &Limits::default()).expect("fixed label")
}

pub(super) fn endpoint(config: &Value) -> Result<McpEndpoint> {
    let mut values = BTreeMap::new();
    if let Ok(Value::Map(environment)) = field(config, "environment") {
        for (key, value) in environment.iter() {
            values.insert(key.to_owned(), text(value)?.to_owned());
        }
    }
    if let Ok(Value::Array(names)) = field(config, "inherit_environment") {
        for name in names {
            let name = text(name)?;
            values.insert(
                name.to_owned(),
                std::env::var(name).map_err(|_| "required child environment variable absent")?,
            );
        }
    }
    match text(field(config, "transport")?)? {
        "stdio" => {
            let Value::Array(arguments) = field(config, "arguments")? else {
                return Err("arguments array required".into());
            };
            Ok(McpEndpoint::Stdio {
                program: text(field(config, "program")?)?.into(),
                arguments: arguments
                    .iter()
                    .map(|value| text(value).map(str::to_owned))
                    .collect::<Result<_>>()?,
                environment: values,
            })
        }
        "streamable_http" => {
            let mut headers = BTreeMap::new();
            if let Ok(Value::Map(fields)) = field(config, "headers") {
                for (key, value) in fields.iter() {
                    headers.insert(key.to_owned(), text(value)?.to_owned());
                }
            }
            if let Ok(Value::Map(fields)) = field(config, "header_environment") {
                for (header, variable) in fields.iter() {
                    headers.insert(
                        header.to_owned(),
                        std::env::var(text(variable)?)
                            .map_err(|_| "required header environment variable absent")?,
                    );
                }
            }
            Ok(McpEndpoint::Http {
                url: text(field(config, "url")?)?.into(),
                headers,
            })
        }
        _ => Err("unsupported transport".into()),
    }
}
