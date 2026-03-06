use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "rho-cli-stub", about = "Legacy rho CLI stub", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Model ID to use (registry ID like "claude-sonnet", or raw model ID)
    #[arg(long, default_value = "claude-sonnet")]
    model: String,

    /// Thinking level (off, minimal, low, medium, high)
    #[arg(long, default_value = "off")]
    thinking: String,

    /// Show thinking output on stderr
    #[arg(long)]
    show_thinking: bool,

    /// Override API key (default: env var or keychain)
    #[arg(long)]
    api_key: Option<String>,

    /// Working directory
    #[arg(short = 'C', long)]
    directory: Option<String>,

    /// Read prompt from file
    #[arg(long)]
    prompt_file: Option<String>,

    /// Restrict available tools (comma-separated names)
    #[arg(long, value_delimiter = ',')]
    tools: Option<Vec<String>>,

    /// Append to system prompt
    #[arg(long)]
    system_append: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run autonomous loop (Ralph pattern)
    Loop {
        /// Loop mode: "build" or "plan"
        #[arg(long, default_value = "build")]
        mode: String,
        /// Path to the implementation plan
        #[arg(long, default_value = "IMPLEMENTATION_PLAN.md")]
        plan: String,
        /// Maximum number of iterations
        #[arg(long, default_value_t = 50)]
        max_iterations: usize,
        /// Seconds to sleep between iterations
        #[arg(long, default_value_t = 5)]
        sleep: u64,
        /// Model ID override
        #[arg(long)]
        model: Option<String>,
        /// Thinking level override
        #[arg(long)]
        thinking: Option<String>,
        /// Override API key
        #[arg(long)]
        api_key: Option<String>,
        /// Working directory
        #[arg(short = 'C', long)]
        directory: Option<String>,
    },
}

fn main() {
    let _cli = Cli::parse();
    eprintln!(
        "rho-cli-stub is a legacy placeholder. Use the top-level `rho-cli` binary from the \
rho-agent package."
    );
}
