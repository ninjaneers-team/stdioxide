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
