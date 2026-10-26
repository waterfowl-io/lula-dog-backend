//! The layered `[server]`/`[database]`/`[auth]` loader: `config/default.toml`
//! -> `config/{APP_ENV}.toml` -> env (`APP__SECTION__KEY`) -> `config/local.toml`
//! (gitignored, for pointing local dev at a real provider -- see
//! `docs/architecture.md` L3). Ported from Desde0, which fixed the exact
//! failure this guards against: an env var the separator doesn't split is
//! simply not seen, and the service boots with the default as though
//! nothing were set.
//!
//! Everything else in [`crate::config::Config`] is still a plain env var,
//! read directly in `Config::from_env` -- only these three sections go
//! through this loader (see plan B1).

use config::{Config as Cfg, Environment, File};
use std::env;

use crate::config::Layered;

/// The environment layer, as a function so a test can drive the REAL prefix
/// and separator with an explicit map instead of the process environment.
fn env_source() -> Environment {
    Environment::default()
        .prefix("APP")
        .separator("__")
        .try_parsing(true)
}

/// Which `config/{name}.toml` layer `APP_ENV` selects (`dev` when unset).
pub fn app_env() -> String {
    env::var("APP_ENV").unwrap_or_else(|_| "dev".into())
}

pub fn load_layered() -> Result<Layered, config::ConfigError> {
    let app_env = app_env();
    let cfg = Cfg::builder()
        .add_source(File::with_name("config/default").required(true))
        .add_source(File::with_name(&format!("config/{app_env}")).required(false))
        .add_source(env_source())
        .add_source(File::with_name("config/local").required(false))
        .build()?;
    cfg.try_deserialize()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a failing assertion aborting the test is the mechanism"
)]
mod tests {
    use super::*;
    use crate::config::AuthConfig;
    use std::collections::HashMap;

    /// Deserialize `[auth]` from `config/default.toml` with the given env
    /// vars laid over it, using the same `env_source()` production uses.
    ///
    /// The map is passed to the source explicitly rather than exported into
    /// the process, because `cargo test` shares one process across threads
    /// and a global mutation would make this test's result depend on which
    /// others ran beside it.
    fn auth_with_env(vars: &[(&str, &str)]) -> AuthConfig {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let cfg = Cfg::builder()
            .add_source(File::with_name("config/default").required(true))
            .add_source(env_source().source(Some(map)))
            .build()
            .expect("build");
        cfg.get::<AuthConfig>("auth").expect("deserialize [auth]")
    }

    #[test]
    fn the_jwks_url_env_var_reaches_the_field() {
        let auth = auth_with_env(&[("APP__AUTH__JWKS_URL", "https://example.test/jwks.json")]);
        assert_eq!(
            auth.jwks_url.as_deref(),
            Some("https://example.test/jwks.json")
        );
    }

    #[test]
    fn without_the_env_var_jwks_url_is_the_default_toml_value() {
        let auth = auth_with_env(&[]);
        assert_eq!(auth.jwks_url.as_deref(), Some(""));
    }

    /// The separator is `__`, so the single underscore inside `jwt_secret`
    /// must survive it. A var split on `_` would address `auth.jwt.secret`,
    /// match nothing, and be dropped without a word.
    #[test]
    fn a_single_underscore_var_is_not_a_synonym_and_is_silently_ignored() {
        let auth = auth_with_env(&[("APP_AUTH_JWT_SECRET", "nope")]);
        assert_eq!(
            auth.jwt_secret, None,
            "if this ever starts working, the deploy docs naming the __ form need revisiting"
        );
    }
}
