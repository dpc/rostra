/// Check if env variable is set and not equal `0` or `false` which are common
/// ways to disable something.
pub fn is_env_var_set(var: &str) -> bool {
    std::env::var_os(var).is_some_and(|v| v != "0" && v != "false")
}

pub fn is_rostra_dev_mode_set() -> bool {
    is_env_var_set("ROSTRA_DEV_MODE")
}
