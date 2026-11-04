//! Agent-side injection: env vars for child processes and peer process detection.
//!
//! | Module | Role |
//! |--------|------|
//! | [`mod@env`] | `HTTP_PROXY`, SDK base URLs, session id for `capture run` children |
//! | [`peer`] | platform-native lookup of the TCP peer behind proxied connections |
//! | [`host_identity`] | one-way `machine_fp` from hostname + identity IP |

pub mod env;
pub mod host_identity;
pub mod peer;
#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) mod process;
#[cfg(any(target_os = "linux", all(test, unix)))]
mod procfs;

pub use env::{
    CAPTURE_PROXY_ENV_KEYS, capture_openai_v1_base, client_gateway_config_args, proxy_environment,
    proxy_environment_with_local_auth,
};
