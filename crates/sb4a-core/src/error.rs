// SPDX-License-Identifier: GPL-3.0-or-later
//! Stable error codes. Never carry raw stderr, configs or credentials.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code(pub &'static str);

impl Code {
    pub const fn new(code: &'static str) -> Self {
        Self(code)
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Code {}

macro_rules! codes {
    ($($name:ident => $code:literal),* $(,)?) => {
        $(pub const $name: Code = Code::new($code);)*
    };
}

codes! {
    ARGUMENTS_INVALID      => "arguments_invalid",
    ROOT_REQUIRED          => "root_required",
    CGROUP_UNAVAILABLE     => "cgroup_unavailable",
    CGROUP_MOVE_FAILED     => "cgroup_move_failed",
    CONFIG_MISSING         => "config_missing",
    CONFIG_INVALID         => "config_invalid",
    SNAPSHOT_INVALID       => "snapshot_invalid",
    SNAPSHOT_TOO_LARGE     => "snapshot_too_large",
    TRAILING_INPUT         => "trailing_input",
    REVISION_INVALID       => "revision_invalid",
    REVISION_CONFLICT      => "revision_conflict",
    CURRENT_INVALID        => "current_invalid",
    TOO_MANY_FILES         => "too_many_files",
    UNSAFE_PATH            => "unsafe_path",
    FILE_TOO_LARGE         => "file_too_large",
    FILES_TOO_LARGE        => "files_too_large",
    UNSAFE_DIRECTORY       => "unsafe_directory",
    UNSAFE_CONFIG_PATH     => "unsafe_config_path",
    CONFIG_RESOURCE_MISSING => "config_resource_missing",
    DNS_REQUIRED           => "dns_required",
    APP_PATH_NOT_PORTABLE  => "app_path_not_portable",
    PLUGIN_UNSUPPORTED     => "plugin_unsupported",
    PLUGIN_INVALID         => "plugin_invalid",
    PLUGIN_CONFIG_MISSING  => "plugin_config_missing",
    PLUGIN_CERT_MISSING    => "plugin_cert_missing",
    MODULE_BUSY            => "module_busy",
    MODULE_DISABLED        => "module_disabled_or_missing",
    STATE_WRITE_FAILED     => "state_write_failed",
    SUPERVISOR_START_FAILED => "supervisor_start_failed",
    CORE_START_FAILED      => "core_start_failed",
    CORE_IDENTITY_FAILED   => "core_identity_failed",
    STARTUP_TIMEOUT        => "startup_timeout",
    CLEANUP_TIMEOUT        => "cleanup_timeout_no_restart",
    CLEANUP_REQUIRED       => "cleanup_required",
    RETRY_LIMIT            => "retry_limit_reached",
    ROLLBACK_MISSING       => "rollback_missing",
    ROLLBACK_INVALID       => "rollback_invalid",
    BOOT_TIMEOUT           => "boot_timeout",
    BOOT_SETTINGS_INVALID  => "boot_settings_invalid",
    PROCESS_DEAD           => "process_dead",
    STAT_INVALID           => "stat_invalid",
    PID_INVALID            => "pid_invalid",
    RULESET_UNKNOWN        => "ruleset_unknown",
    RULESET_FETCH_FAILED   => "ruleset_fetch_failed",
    RULESET_CORRUPT        => "ruleset_corrupt",
    RULESET_URL_INVALID    => "ruleset_url_invalid",
    MODULE_OPERATION_FAILED => "module_operation_failed",
}

/// Only `^[a-z_]{1,80}$` codes may reach the wire.
pub fn is_valid_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 80
        && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}
