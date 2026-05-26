use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use wiki_craft::config::{
    DEFAULT_CONFIG_PATH, KnowledgeBaseCreateInput, activate_knowledge_base, create_knowledge_base,
    list_knowledge_bases,
};
use wiki_craft::knowledge::initialize_project;
use wiki_craft::runtime;
use wiki_craft::search::{SearchOptions, render_text_response, search_configured};

#[derive(Debug, Parser)]
#[command(
    name = "wiki_craft",
    version,
    about = "Markdown-first knowledge base maintenance agent"
)]
struct Cli {
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init,
    Ingest {
        #[arg(long)]
        once: bool,
    },
    Serve,
    Status {
        #[arg(long)]
        json: bool,
    },
    Search {
        #[arg(long)]
        knowledge_base: Option<String>,
        #[arg(long)]
        query: String,
        #[arg(long, default_value_t = 5)]
        top_k: usize,
        #[arg(long)]
        json: bool,
    },
    Metrics {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        prometheus: bool,
    },
    Candidates {
        #[command(subcommand)]
        command: CandidateCommand,
    },
    Knowledge {
        #[command(subcommand)]
        command: KnowledgeCommand,
    },
    KnowledgeBase {
        #[command(subcommand)]
        command: KnowledgeBaseCommand,
    },
}

#[derive(Debug, Subcommand)]
enum CandidateCommand {
    List,
    Diff { run_id: String },
    Summaries { run_id: String },
    Approve { run_id: String },
    Merge { run_id: String },
    Reject { run_id: String },
}

#[derive(Debug, Subcommand)]
enum KnowledgeCommand {
    Reorganize,
}

#[derive(Debug, Subcommand)]
enum KnowledgeBaseCommand {
    List,
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        focus: String,
    },
    Activate {
        id: String,
    },
}

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();

    match cli.command {
        Command::Init => {
            let report = initialize_project(&cli.config)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Ingest { once } => {
            if !once {
                bail!("use `ingest --once` for one-time sources or `serve` for periodic sources");
            }
            let outcome = runtime::run_production_ingest(&cli.config)?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
        }
        Command::Serve => {
            runtime::serve(&cli.config)?;
        }
        Command::Status { json } => {
            let status = runtime::status(&cli.config)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("Wiki Craft status");
                println!("pending_candidates: {}", status.pending_candidates);
                println!("compaction_count: {}", status.compaction_count);
                println!("{}", status.prompt_cache_stats.summary_line());
                if let Some(run) = status.last_run {
                    println!("last_run: {:?} - {}", run.kind, run.message);
                    if let Some(run_id) = run.run_id {
                        println!("last_candidate: {run_id}");
                    }
                } else {
                    println!("last_run: none");
                }
            }
        }
        Command::Search {
            knowledge_base,
            query,
            top_k,
            json,
        } => {
            let response = search_configured(
                &cli.config,
                SearchOptions {
                    knowledge_base_id: knowledge_base,
                    query,
                    top_k,
                },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                println!("{}", render_text_response(&response));
            }
        }
        Command::Metrics { json, prometheus } => {
            if prometheus {
                print!("{}", runtime::metrics_prometheus(&cli.config)?);
            } else {
                let snapshot = runtime::metrics_snapshot(&cli.config)?;
                let _ = json;
                println!("{}", serde_json::to_string_pretty(&snapshot)?);
            }
        }
        Command::Candidates { command } => match command {
            CandidateCommand::List => {
                let candidates = runtime::list(&cli.config)?;
                println!("{}", serde_json::to_string_pretty(&candidates)?);
            }
            CandidateCommand::Diff { run_id } => {
                println!("{}", runtime::candidate_diff(&cli.config, &run_id)?);
            }
            CandidateCommand::Summaries { run_id } => {
                println!("{}", runtime::candidate_summaries(&cli.config, &run_id)?);
            }
            CandidateCommand::Approve { run_id } => {
                let outcome = runtime::approve(&cli.config, &run_id)?;
                println!("{}", outcome.message);
            }
            CandidateCommand::Merge { run_id } => {
                let outcome = runtime::merge(&cli.config, &run_id)?;
                println!("{}", outcome.message);
            }
            CandidateCommand::Reject { run_id } => {
                runtime::reject(&cli.config, &run_id)?;
                println!("rejected {run_id}");
            }
        },
        Command::Knowledge { command } => match command {
            KnowledgeCommand::Reorganize => {
                let outcome = runtime::reorganize(&cli.config)?;
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            }
        },
        Command::KnowledgeBase { command } => match command {
            KnowledgeBaseCommand::List => {
                let knowledge_bases = list_knowledge_bases(&cli.config)?;
                println!("{}", serde_json::to_string_pretty(&knowledge_bases)?);
            }
            KnowledgeBaseCommand::Create { name, focus } => {
                let record =
                    create_knowledge_base(&cli.config, KnowledgeBaseCreateInput { name, focus })?;
                println!("{}", serde_json::to_string_pretty(&record)?);
            }
            KnowledgeBaseCommand::Activate { id } => {
                let record = activate_knowledge_base(&cli.config, &id)?;
                println!("activated {}", record.id);
            }
        },
    }

    Ok(())
}
