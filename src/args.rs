use std::{
    env::{self, args_os},
    ffi::OsString,
};

use clap::Parser;

/// Trait for abstracting environment variable access.
///
/// This allows dependency injection of environment providers in tests,
/// avoiding the need to mutate process-wide environment variables.
pub trait Env {
    /// Get an environment variable value.
    fn var(&self, key: &str) -> Option<String>;
}

/// Production environment provider that reads from the process environment.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        env::var(key).ok()
    }
}

/// Command-line arguments for stdioxide.
///
/// Configures the TCP ports and child process to launch. Ports can be specified
/// via command-line flags or environment variables (flags take precedence).
///
/// # Example
///
/// ```bash
/// stdioxide --protocol-port 7000 --stderr-port 7001 --health-port 7002 python script.py --arg1 --arg2
/// ```
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
#[non_exhaustive]
pub struct Args {
    /// The port to use for forwarding stdin and stdout.
    #[arg(long, default_value_t = 7000)]
    pub protocol_port: u16,

    /// The port to use for forwarding stderr.
    #[arg(long, default_value_t = 7001)]
    pub stderr_port: u16,

    /// The port to use for health checks.
    #[arg(long, default_value_t = 7002)]
    pub health_port: u16,

    /// The command to run as a subprocess.
    #[arg(required = true)]
    pub command: String,

    /// The arguments to pass to the command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<String>,
}

impl Args {
    /// Parse arguments from the process environment and command line.
    ///
    /// This is the main entry point for production use.
    #[expect(
        clippy::same_name_method,
        reason = "We want to mimic the API of clap’s `parse()` method while adding environment variable support"
    )]
    #[must_use]
    pub fn parse() -> Self {
        Self::parse_from_env(&ProcessEnv)
    }

    /// Parse arguments using a custom environment provider.
    ///
    /// This method allows dependency injection of environment variables,
    /// which is useful for testing without mutating global state.
    pub fn parse_from_env<E: Env>(env: &E) -> Self {
        Self::parse_from_env_and_args(env, args_os())
    }

    /// Parse arguments from a custom environment and argument iterator.
    ///
    /// This is the most flexible parsing method, used internally and in tests.
    pub fn parse_from_env_and_args<E: Env, S: Into<OsString>, A: IntoIterator<Item = S>>(
        env: &E,
        args: A,
    ) -> Self {
        // Build argument list with environment variable defaults injected.
        let mut arg_vec: Vec<OsString> = args.into_iter().map(Into::into).collect();

        // Check if ports are provided via CLI; if not, inject from environment.
        let has_protocol_port = arg_vec
            .iter()
            .any(|arg| arg.to_str() == Some("--protocol-port"));
        let has_stderr_port = arg_vec
            .iter()
            .any(|arg| arg.to_str() == Some("--stderr-port"));
        let has_health_port = arg_vec
            .iter()
            .any(|arg| arg.to_str() == Some("--health-port"));

        // Inject environment variables as CLI args if not already present.
        let mut insertions = Vec::new();

        if !has_protocol_port && let Some(port) = env.var("STDIOXIDE_PROTOCOL_PORT") {
            insertions.push("--protocol-port".to_owned());
            insertions.push(port);
        }
        if !has_stderr_port && let Some(port) = env.var("STDIOXIDE_STDERR_PORT") {
            insertions.push("--stderr-port".to_owned());
            insertions.push(port);
        }
        if !has_health_port && let Some(port) = env.var("STDIOXIDE_HEALTH_PORT") {
            insertions.push("--health-port".to_owned());
            insertions.push(port);
        }
        // Insert environment-derived args after the program name but before other args.
        if !insertions.is_empty() && !arg_vec.is_empty() {
            let rest = arg_vec.split_off(1);
            arg_vec.extend(insertions.into_iter().map(Into::into));
            arg_vec.extend(rest);
        }

        Self::try_parse_from(arg_vec).unwrap_or_else(|error| error.exit())
    }
}

#[cfg(test)]
impl Args {
    /// Parse arguments from a custom environment and argument iterator for testing.
    ///
    /// Returns a Result instead of exiting on error, allowing tests to verify failure cases.
    fn try_parse_from_env_and_args<E: Env, S: Into<OsString>, A: IntoIterator<Item = S>>(
        env: &E,
        args: A,
    ) -> Result<Self, clap::Error> {
        // Build argument list with environment variable defaults injected
        let mut arg_vec: Vec<OsString> = args.into_iter().map(Into::into).collect();

        // Check if ports are provided via CLI; if not, inject from environment
        let has_protocol_port = arg_vec
            .iter()
            .any(|arg| arg.to_str() == Some("--protocol-port"));
        let has_stderr_port = arg_vec
            .iter()
            .any(|arg| arg.to_str() == Some("--stderr-port"));
        let has_health_port = arg_vec
            .iter()
            .any(|arg| arg.to_str() == Some("--health-port"));

        // Inject environment variables as CLI args if not already present
        let mut insertions = Vec::new();

        if !has_protocol_port && let Some(port) = env.var("STDIOXIDE_PROTOCOL_PORT") {
            insertions.push("--protocol-port".to_owned());
            insertions.push(port);
        }
        if !has_stderr_port && let Some(port) = env.var("STDIOXIDE_STDERR_PORT") {
            insertions.push("--stderr-port".to_owned());
            insertions.push(port);
        }
        if !has_health_port && let Some(port) = env.var("STDIOXIDE_HEALTH_PORT") {
            insertions.push("--health-port".to_owned());
            insertions.push(port);
        }

        // Insert environment-derived args after the program name but before other args.
        if !insertions.is_empty() && !arg_vec.is_empty() {
            let rest = arg_vec.split_off(1);
            arg_vec.extend(insertions.into_iter().map(Into::into));
            arg_vec.extend(rest);
        }

        Self::try_parse_from(arg_vec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Test environment provider backed by an in-memory hashmap.
    ///
    /// This allows testing environment variable behavior without mutating
    /// the process-wide environment state.
    #[derive(Debug, Default)]
    struct TestEnv {
        values: HashMap<String, String>,
    }

    impl TestEnv {
        fn new(values: impl IntoIterator<Item = (&'static str, &'static str)>) -> Self {
            Self {
                values: values
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .collect(),
            }
        }
    }

    impl Env for TestEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.values.get(key).cloned()
        }
    }

    #[test]
    fn test_default_port_values() -> Result<(), anyhow::Error> {
        let env = TestEnv::default();
        let args = Args::try_parse_from_env_and_args(&env, ["stdioxide", "echo"])?;
        assert_eq!(args.protocol_port, 7000);
        assert_eq!(args.stderr_port, 7001);
        assert_eq!(args.health_port, 7002);
        Ok(())
    }

    #[test]
    fn test_custom_port_values_via_args() -> Result<(), anyhow::Error> {
        let env = TestEnv::default();
        let args = Args::try_parse_from_env_and_args(
            &env,
            [
                "stdioxide",
                "--protocol-port",
                "8000",
                "--stderr-port",
                "8001",
                "--health-port",
                "8002",
                "echo",
            ],
        )?;
        assert_eq!(args.protocol_port, 8000);
        assert_eq!(args.stderr_port, 8001);
        assert_eq!(args.health_port, 8002);
        Ok(())
    }

    #[test]
    fn test_command_and_args() -> Result<(), anyhow::Error> {
        let env = TestEnv::default();
        let args =
            Args::try_parse_from_env_and_args(&env, ["stdioxide", "python", "-m", "http.server"])?;
        assert_eq!(args.command, "python");
        assert_eq!(args.args, vec!["-m", "http.server"]);
        Ok(())
    }

    #[test]
    fn test_args_with_hyphen_values() -> Result<(), anyhow::Error> {
        let env = TestEnv::default();
        let args =
            Args::try_parse_from_env_and_args(&env, ["stdioxide", "myapp", "--flag", "-value"])?;
        assert_eq!(args.command, "myapp");
        assert_eq!(args.args, vec!["--flag", "-value"]);
        Ok(())
    }

    #[test]
    fn test_empty_args() -> Result<(), anyhow::Error> {
        let env = TestEnv::default();
        let args = Args::try_parse_from_env_and_args(&env, ["stdioxide", "echo"])?;
        assert_eq!(args.command, "echo");
        assert!(args.args.is_empty());
        Ok(())
    }

    #[test]
    fn test_missing_command_fails() {
        let env = TestEnv::default();
        let result = Args::try_parse_from_env_and_args(&env, ["stdioxide"]);
        assert!(
            result.is_err(),
            "Expected parsing to fail when command is missing"
        );
    }

    #[test]
    fn test_env_var_protocol_port() -> Result<(), anyhow::Error> {
        let env = TestEnv::new([("STDIOXIDE_PROTOCOL_PORT", "9000")]);
        let args = Args::try_parse_from_env_and_args(&env, ["stdioxide", "echo"])?;
        assert_eq!(args.protocol_port, 9000);
        Ok(())
    }

    #[test]
    fn test_env_var_stderr_port() -> Result<(), anyhow::Error> {
        let env = TestEnv::new([("STDIOXIDE_STDERR_PORT", "9001")]);
        let args = Args::try_parse_from_env_and_args(&env, ["stdioxide", "echo"])?;
        assert_eq!(args.stderr_port, 9001);
        Ok(())
    }

    #[test]
    fn test_env_var_health_port() -> Result<(), anyhow::Error> {
        let env = TestEnv::new([("STDIOXIDE_HEALTH_PORT", "9002")]);
        let args = Args::try_parse_from_env_and_args(&env, ["stdioxide", "echo"])?;
        assert_eq!(args.health_port, 9002);
        Ok(())
    }

    #[test]
    fn test_cli_args_override_env_vars() -> Result<(), anyhow::Error> {
        let env = TestEnv::new([
            ("STDIOXIDE_PROTOCOL_PORT", "9000"),
            ("STDIOXIDE_STDERR_PORT", "9001"),
            ("STDIOXIDE_HEALTH_PORT", "9002"),
        ]);

        let args = Args::try_parse_from_env_and_args(
            &env,
            [
                "stdioxide",
                "--protocol-port",
                "8000",
                "--stderr-port",
                "8001",
                "--health-port",
                "8002",
                "echo",
            ],
        )?;

        assert_eq!(args.protocol_port, 8000);
        assert_eq!(args.stderr_port, 8001);
        assert_eq!(args.health_port, 8002);
        Ok(())
    }
}
