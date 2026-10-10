use super::*;

/// Loads optional authority-local settings at startup. Validation happens
/// before the runtime admits any new delegated work.
pub(crate) fn load_budget_policy(
    home_dir: &std::path::Path,
    db_path: &std::path::Path,
) -> VibexResult<vibex_core::VibexUseBudgetPolicy> {
    let path = home_dir.join("vibex-use.json");
    let settings = match std::fs::File::open(path) {
        Ok(file) => {
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take(16 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| {
                    VibexError::storage(
                        "vibex_use_budget_read_failed",
                        "team budget settings could not be read",
                    )
                })?;
            if bytes.len() > 16 * 1024 {
                return Err(VibexError::validation(
                    "vibex_use_budget_invalid",
                    "team budget settings exceed the size limit",
                ));
            }
            serde_json::from_slice::<vibex_core::VibexUseBudgetSettings>(&bytes).map_err(|_| {
                VibexError::validation(
                    "vibex_use_budget_invalid",
                    "team budget settings contain an invalid field or value",
                )
            })?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
        Err(_) => {
            return Err(VibexError::storage(
                "vibex_use_budget_read_failed",
                "team budget settings could not be opened",
            ));
        }
    };
    let policy = settings.resolve()?;
    let conn = open_database(db_path)?;
    vibex_db::VibexUseBudgetRepository::set_policy(&conn, &policy)?;
    Ok(policy)
}
