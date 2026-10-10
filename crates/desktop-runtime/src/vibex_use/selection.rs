use super::*;

/// Resolves optional fields against the choices advertised with one exact
/// account/model option. Labels and provider defaults are never identities.
pub(super) fn select_option(
    option: &SessionRuntimeOption,
    target: &serde_json::Value,
) -> VibexResult<Option<SessionRuntimeSelection>> {
    let mut selection = option.selection.clone();
    let invalid = || {
        VibexError::validation(
            use_codes::REQUEST_INVALID,
            "target configuration is invalid",
        )
    };
    if let Some(agent) = target.get("agentId").and_then(serde_json::Value::as_str)
        && selection.agent_id.as_str() != agent
    {
        return Ok(None);
    }
    if let Some(profile) = target
        .get("providerProfileId")
        .and_then(serde_json::Value::as_str)
        && selection.provider_profile_id().map(|id| id.as_str()) != Some(profile)
    {
        return Ok(None);
    }
    if let Some(auth) = target.get("authSource") {
        let auth: vibex_core::RuntimeAuthSource =
            serde_json::from_value(auth.clone()).map_err(|_| invalid())?;
        if selection.auth_source != auth {
            return Ok(None);
        }
    }
    if let Some(model) = target.get("modelSelection") {
        let model: vibex_core::RuntimeModelSelection =
            serde_json::from_value(model.clone()).map_err(|_| invalid())?;
        if selection.model != model {
            return Ok(None);
        }
    }
    if let Some(model) = target.get("model").and_then(serde_json::Value::as_str)
        && selection.model_id() != Some(model)
        && !(model == "agent-default" && selection.model_id().is_none())
    {
        return Ok(None);
    }
    for (key, values, current) in [
        (
            "reasoningEffort",
            &option.reasoning_efforts,
            &mut selection.reasoning_effort,
        ),
        ("modeId", &option.modes, &mut selection.mode_id),
    ] {
        if let Some(value) = target.get(key) {
            if value.is_null() {
                *current = None;
                continue;
            }
            let value = value.as_str().ok_or_else(invalid)?;
            if current.as_deref() != Some(value)
                && !values.iter().any(|choice| choice.value == value)
            {
                return Ok(None);
            }
            *current = Some(value.to_owned());
        }
    }
    if let Some(config) = target.get("configValues") {
        let config: BTreeMap<String, String> =
            serde_json::from_value(config.clone()).map_err(|_| invalid())?;
        if config.len() > 64 {
            return Err(invalid());
        }
        for (key, value) in config {
            if !option
                .features
                .iter()
                .any(|feature| feature.id == key && feature.accepts_value(&value))
                && selection.config_values.get(&key) != Some(&value)
            {
                return Ok(None);
            }
            selection.config_values.insert(key, value);
        }
    }
    Ok(Some(selection))
}
