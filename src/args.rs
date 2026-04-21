use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// The port to use for forwarding stdin and stdout.
    #[arg(long, env = "STDIOXIDE_PROTOCOL_PORT", default_value_t = 7000)]
    pub protocol_port: u16,

    /// The port to use for forwarding stderr.
    #[arg(long, env = "STDIOXIDE_STDERR_PORT", default_value_t = 7001)]
    pub stderr_port: u16,

    /// The port to use for health checks.
    #[arg(long, env = "STDIOXIDE_HEALTH_PORT", default_value_t = 7002)]
    pub health_port: u16,

    /// The command to run as a subprocess.
    #[arg(required = true)]
    pub command: String,

    /// The arguments to pass to the command.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub args: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to serialize tests that modify environment variables.
    // This prevents interference between parallel test execution.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// RAII helper for temporarily setting environment variables in tests.
    /// Automatically restores the original state when dropped.
    struct EnvVar {
        key: String,
        original_value: Option<String>,
    }

    impl EnvVar {
        /// Set an environment variable temporarily.
        fn set(key: impl Into<String>, value: impl AsRef<str>) -> Self {
            let key = key.into();
            let original_value = std::env::var(&key).ok();
            unsafe {
                std::env::set_var(&key, value.as_ref());
            }
            Self {
                key,
                original_value,
            }
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            unsafe {
                match &self.original_value {
                    Some(value) => std::env::set_var(&self.key, value),
                    None => std::env::remove_var(&self.key),
                }
            }
        }
    }

    #[test]
    fn test_default_port_values() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let args = Args::try_parse_from(["stdioxide", "echo"]).unwrap();
        assert_eq!(args.protocol_port, 7000);
        assert_eq!(args.stderr_port, 7001);
        assert_eq!(args.health_port, 7002);
    }

    #[test]
    fn test_custom_port_values_via_args() {
        let args = Args::try_parse_from([
            "stdioxide",
            "--protocol-port",
            "8000",
            "--stderr-port",
            "8001",
            "--health-port",
            "8002",
            "echo",
        ])
        .unwrap();
        assert_eq!(args.protocol_port, 8000);
        assert_eq!(args.stderr_port, 8001);
        assert_eq!(args.health_port, 8002);
    }

    #[test]
    fn test_command_and_args() {
        let args = Args::try_parse_from(["stdioxide", "python", "-m", "http.server"]).unwrap();
        assert_eq!(args.command, "python");
        assert_eq!(args.args, vec!["-m", "http.server"]);
    }

    #[test]
    fn test_args_with_hyphen_values() {
        let args = Args::try_parse_from(["stdioxide", "myapp", "--flag", "-value"]).unwrap();
        assert_eq!(args.command, "myapp");
        assert_eq!(args.args, vec!["--flag", "-value"]);
    }

    #[test]
    fn test_empty_args() {
        let args = Args::try_parse_from(["stdioxide", "echo"]).unwrap();
        assert_eq!(args.command, "echo");
        assert!(args.args.is_empty());
    }

    #[test]
    fn test_missing_command_fails() {
        let result = Args::try_parse_from(["stdioxide"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_env_var_protocol_port() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _env = EnvVar::set("STDIOXIDE_PROTOCOL_PORT", "9000");
        let args = Args::try_parse_from(["stdioxide", "echo"]).unwrap();
        assert_eq!(args.protocol_port, 9000);
    }

    #[test]
    fn test_env_var_stderr_port() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _env = EnvVar::set("STDIOXIDE_STDERR_PORT", "9001");
        let args = Args::try_parse_from(["stdioxide", "echo"]).unwrap();
        assert_eq!(args.stderr_port, 9001);
    }

    #[test]
    fn test_env_var_health_port() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _env = EnvVar::set("STDIOXIDE_HEALTH_PORT", "9002");
        let args = Args::try_parse_from(["stdioxide", "echo"]).unwrap();
        assert_eq!(args.health_port, 9002);
    }

    #[test]
    fn test_cli_args_override_env_vars() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let _env1 = EnvVar::set("STDIOXIDE_PROTOCOL_PORT", "9000");
        let _env2 = EnvVar::set("STDIOXIDE_STDERR_PORT", "9001");
        let _env3 = EnvVar::set("STDIOXIDE_HEALTH_PORT", "9002");

        let args = Args::try_parse_from([
            "stdioxide",
            "--protocol-port",
            "8000",
            "--stderr-port",
            "8001",
            "--health-port",
            "8002",
            "echo",
        ])
        .unwrap();

        assert_eq!(args.protocol_port, 8000);
        assert_eq!(args.stderr_port, 8001);
        assert_eq!(args.health_port, 8002);
    }
}
