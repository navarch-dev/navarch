use crate::{Role, StoreError};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
struct Front {
    agent: String,
    model: Option<String>,
    mode: Option<String>,
}
pub(crate) fn validate(name: &str) -> Result<(), StoreError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(StoreError::Invalid(
            "role name must contain only letters, digits, '-' or '_'".into(),
        ));
    }
    Ok(())
}
pub(crate) fn parse(text: &str) -> Result<Role, StoreError> {
    let text = text
        .strip_prefix("+++\n")
        .ok_or_else(|| StoreError::Invalid("role must start with +++ TOML front matter".into()))?;
    let (front, preferences) = text
        .split_once("\n+++\n")
        .ok_or_else(|| StoreError::Invalid("role front matter is not closed".into()))?;
    let front: Front = toml::from_str(front).map_err(StoreError::from)?;
    Ok(Role {
        agent: front.agent,
        model: front.model,
        mode: front.mode,
        preferences: preferences.into(),
    })
}
pub(crate) fn format(role: Role) -> Result<String, StoreError> {
    let front = toml::to_string(&Front {
        agent: role.agent,
        model: role.model,
        mode: role.mode,
    })
    .map_err(StoreError::from)?;
    Ok(format!("+++\n{front}\n+++\n{}", role.preferences))
}
