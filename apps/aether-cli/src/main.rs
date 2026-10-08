//! `aether` CLI — a thin client of the Aether daemon (ADR-001).
//! Copyright 2026 smart-jemy. Licensed under Apache-2.0 (see LICENSE).
//!
//! Everything here works with **zero** UI crates in the picture:
//! `serve` starts the daemon; every other command is a WebSocket client of
//! the running daemon (kill-the-UI invariant, ADR-001).

mod client;
mod commands;

use aether_core::error::AetherResult;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "aether",
    version,
    about = "Aether — local-first, open-source, developer-first API security proxy",
    author = "smart-jemy"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the Aether daemon (proxy + control plane).
    Serve {
        /// TOML config file (defaults are used when absent).
        #[arg(long)]
        config: Option<PathBuf>,
        /// Proxy listen override, e.g. 127.0.0.1:8888.
        #[arg(long)]
        listen: Option<String>,
        /// Control-plane listen override.
        #[arg(long)]
        control: Option<String>,
        /// Keep history in RAM only (nothing touches the disk).
        #[arg(long)]
        memory: bool,
        /// Data directory (default ~/.aether).
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// CA management (the MITM root; ADR-004).
    Ca {
        #[command(subcommand)]
        command: CaCommand,
    },
    /// Inspect recorded flows.
    Flows {
        #[command(subcommand)]
        command: FlowsCommand,
    },
    /// Manual interception (hold / edit / drop / release).
    Intercept {
        #[command(subcommand)]
        command: InterceptCommand,
    },
    /// Match & replace rules (serialized hooks, ADR-006).
    Rules {
        #[command(subcommand)]
        command: RulesCommand,
    },
    /// Local benchmarks against the storage backend.
    Bench {
        #[command(subcommand)]
        command: BenchCommand,
    },
    /// Run the Phase-0 architectural proof through the real pipeline.
    Demo,
    /// Print the architecture invariants this binary guarantees.
    Info,
    /// Project meta: who builds Aether, under which license.
    About,
}

#[derive(Subcommand)]
enum CaCommand {
    /// Print the PEM of the proxy CA (install it in your browser/OS).
    Export {
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Print where the CA files live.
    Path {
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum FlowsCommand {
    /// List the newest flows (keyset-paginated).
    List {
        #[arg(long, default_value_t = 25)]
        limit: u32,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        method: Option<String>,
        #[arg(long)]
        before: Option<u64>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Show one flow in full (headers + bodies).
    Show {
        id: u64,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Search flows (FTS5-backed on the daemon).
    Search {
        query: String,
        #[arg(long, default_value_t = 25)]
        limit: u32,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
}

#[derive(Subcommand)]
enum InterceptCommand {
    /// List currently held flows.
    Ls {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Dump a held request/response as JSON (edit it, then `release --from-file`).
    Show {
        id: u64,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Drop a held flow (the client gets a 502).
    Drop {
        id: u64,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Forward a held flow — optionally edited.
    Release {
        id: u64,
        /// Full edited payload JSON (as printed by `intercept show`).
        #[arg(long)]
        from_file: Option<PathBuf>,
        /// Add/replace a request header "Name: value" (repeatable).
        #[arg(long = "header")]
        headers: Vec<String>,
        /// Remove a request header (repeatable).
        #[arg(long = "remove-header")]
        remove_headers: Vec<String>,
        /// Replace the request path (including query).
        #[arg(long)]
        path: Option<String>,
        /// Replace the request body with file contents.
        #[arg(long)]
        body_file: Option<PathBuf>,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Toggle interception phases.
    Set {
        #[arg(long, value_parser = clap::value_parser!(bool))]
        request: Option<bool>,
        #[arg(long, value_parser = clap::value_parser!(bool))]
        response: Option<bool>,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
}

#[derive(Subcommand)]
enum RulesCommand {
    /// List rules.
    Ls {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Add a rule from a JSON file (or stdin with `-`).
    Add {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Remove a rule.
    Rm {
        id: u64,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Enable a rule.
    Enable {
        id: u64,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Disable a rule (kept, but not applied).
    Disable {
        id: u64,
        #[arg(long)]
        control: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
}

#[derive(Subcommand)]
enum BenchCommand {
    /// Insert N synthetic flows into SQLite and time pagination/search.
    /// The Milestone-0 scale gate: `aether bench flows --n 100000`.
    Flows {
        #[arg(long, default_value_t = 10_000)]
        n: u64,
        #[arg(long)]
        db: Option<PathBuf>,
        /// Keep the generated database after the run.
        #[arg(long)]
        keep: bool,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Serve {
            config,
            listen,
            control,
            memory,
            data_dir,
        } => commands::serve::run(config, listen, control, memory, data_dir).await,
        Commands::Ca { command } => match command {
            CaCommand::Export {
                out,
                control,
                token,
            } => commands::ca::export(out, control, token).await,
            CaCommand::Path { data_dir } => commands::ca::path(data_dir),
        },
        Commands::Flows { command } => match command {
            FlowsCommand::List {
                limit,
                host,
                method,
                before,
                json,
                control,
                token,
            } => commands::flows::list(limit, host, method, before, json, control, token).await,
            FlowsCommand::Show {
                id,
                json,
                control,
                token,
            } => commands::flows::show(id, json, control, token).await,
            FlowsCommand::Search {
                query,
                limit,
                json,
                control,
                token,
            } => commands::flows::search(query, limit, json, control, token).await,
        },
        Commands::Intercept { command } => match command {
            InterceptCommand::Ls {
                json,
                control,
                token,
            } => commands::intercept::ls(json, control, token).await,
            InterceptCommand::Show { id, control, token } => {
                commands::intercept::show(id, control, token).await
            }
            InterceptCommand::Drop { id, control, token } => {
                commands::intercept::drop(id, control, token).await
            }
            InterceptCommand::Release {
                id,
                from_file,
                headers,
                remove_headers,
                path,
                body_file,
                control,
                token,
            } => {
                commands::intercept::release(
                    id,
                    from_file,
                    headers,
                    remove_headers,
                    path,
                    body_file,
                    control,
                    token,
                )
                .await
            }
            InterceptCommand::Set {
                request,
                response,
                control,
                token,
            } => commands::intercept::set(request, response, control, token).await,
        },
        Commands::Rules { command } => match command {
            RulesCommand::Ls {
                json,
                control,
                token,
            } => commands::rules::ls(json, control, token).await,
            RulesCommand::Add {
                file,
                control,
                token,
            } => commands::rules::add(file, control, token).await,
            RulesCommand::Rm { id, control, token } => {
                commands::rules::rm(id, control, token).await
            }
            RulesCommand::Enable { id, control, token } => {
                commands::rules::set_enabled(id, true, control, token).await
            }
            RulesCommand::Disable { id, control, token } => {
                commands::rules::set_enabled(id, false, control, token).await
            }
        },
        Commands::Bench { command } => match command {
            BenchCommand::Flows { n, db, keep } => commands::bench::flows(n, db, keep).await,
        },
        Commands::Demo => commands::demo::run().await,
        Commands::Info => commands::demo::info(),
        Commands::About => commands::demo::about(),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Shared connection context for remote commands.
pub(crate) struct Conn {
    pub control: Option<String>,
    pub token: Option<String>,
}

pub(crate) fn data_dir(flag: Option<&PathBuf>) -> PathBuf {
    flag.cloned()
        .or_else(|| std::env::var("AETHER_DATA_DIR").ok().map(PathBuf::from))
        .unwrap_or_else(aether_daemon::default_data_dir)
}

pub(crate) async fn connect(conn: &Conn) -> AetherResult<client::ControlClient> {
    let dd = data_dir(None);
    let addr = client::resolve_addr(conn.control.as_deref(), &dd);
    let token = client::resolve_token(conn.token.as_deref(), &dd)?;
    client::ControlClient::connect(&addr, &token).await
}
