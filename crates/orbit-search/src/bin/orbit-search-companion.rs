// Legacy companion binary surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![allow(
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::private_intra_doc_links
)]

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use clap::Parser;
use fastembed::{EmbeddingModel, ModelTrait, TextEmbedding, TextInitOptions};
use orbit_common::OrbitError;
use orbit_search::{
    CompanionPaths, ModelSpec, RpcError, RpcRequest, RpcResponse, RpcResult, unparsed_request_id,
};

#[derive(Debug, Parser)]
#[command(name = "orbit-search-companion")]
struct Args {
    #[arg(long, default_value = orbit_search::DEFAULT_MODEL)]
    model: String,
    #[arg(long)]
    model_path: Option<PathBuf>,
    #[arg(long)]
    download_model: bool,
    #[arg(long)]
    version_info: bool,
}

struct FastembedServer {
    model_id: String,
    dim: usize,
    max_input_tokens: usize,
    model: TextEmbedding,
}

impl FastembedServer {
    fn load(args: &Args) -> Result<Self, OrbitError> {
        let spec = ModelSpec::parse(&args.model)?;
        let fastembed_model = parse_fastembed_model(spec.fastembed_name)?;
        let cache_dir = args
            .model_path
            .clone()
            .unwrap_or_else(|| default_model_dir(spec.alias));
        let options = TextInitOptions::new(fastembed_model.clone())
            .with_cache_dir(cache_dir)
            .with_max_length(spec.max_input_tokens)
            .with_show_download_progress(args.download_model);
        let model = TextEmbedding::try_new(options).map_err(|error| {
            OrbitError::Execution(format!("failed to load embedding model: {error}"))
        })?;
        let dim = EmbeddingModel::get_model_info(&fastembed_model)
            .map(|info| info.dim)
            .unwrap_or(spec.dim);
        Ok(Self {
            model_id: spec.alias.to_string(),
            dim,
            max_input_tokens: spec.max_input_tokens,
            model,
        })
    }

    fn handle(&mut self, request: RpcRequest) -> RpcResponse {
        let id = request.id();
        match request {
            RpcRequest::Info { .. } => RpcResponse::Result {
                id,
                result: RpcResult::Info {
                    model_id: self.model_id.clone(),
                    dim: self.dim,
                    max_input_tokens: self.max_input_tokens,
                    version: Some(env!("CARGO_PKG_VERSION").to_string()),
                },
            },
            RpcRequest::Embed { texts, .. } => {
                let refs = texts.iter().map(String::as_str).collect::<Vec<_>>();
                match self.model.embed(refs, None) {
                    Ok(vectors) => RpcResponse::Result {
                        id,
                        result: RpcResult::Embed { vectors },
                    },
                    Err(error) => error_response(id, "embed_failed", error.to_string()),
                }
            }
            RpcRequest::TokenCount { text, .. } => match self.model.tokenizer.encode(text, true) {
                Ok(encoding) => RpcResponse::Result {
                    id,
                    result: RpcResult::TokenCount {
                        tokens: encoding.len(),
                    },
                },
                Err(error) => error_response(id, "token_count_failed", error.to_string()),
            },
            RpcRequest::TokenBoundaries { text, .. } => {
                match self.model.tokenizer.encode(text, true) {
                    Ok(encoding) => RpcResponse::Result {
                        id,
                        result: RpcResult::TokenBoundaries {
                            ends: encoding
                                .get_offsets()
                                .iter()
                                .filter_map(|(start, end)| (end > start).then_some(*end))
                                .collect(),
                        },
                    },
                    Err(error) => error_response(id, "token_boundaries_failed", error.to_string()),
                }
            }
            RpcRequest::Exit { .. } => RpcResponse::Result {
                id,
                result: RpcResult::Exit { ok: true },
            },
        }
    }
}

fn main() {
    if let Err(error) = run() {
        if is_broken_pipe(&error) {
            return;
        }
        let _ = writeln!(io::stderr(), "{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), OrbitError> {
    let args = Args::parse();
    if args.version_info {
        write_json_line(&RpcResponse::Result {
            id: 0,
            result: RpcResult::Info {
                model_id: args.model.clone(),
                dim: 0,
                max_input_tokens: 0,
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            },
        })?;
        return Ok(());
    }

    let mut server = FastembedServer::load(&args)?;
    if args.download_model {
        write_install_marker(&args)?;
        return Ok(());
    }

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| OrbitError::Execution(error.to_string()))?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Result<RpcRequest, _> = serde_json::from_str(&line);
        let response = match request {
            Ok(request) => {
                let should_exit = matches!(request, RpcRequest::Exit { .. });
                let response = server.handle(request);
                write_json_line(&response)?;
                if should_exit {
                    break;
                }
                continue;
            }
            // Echo the caller's own id when the line is trustworthy enough to
            // carry one, so a client can correlate the rejection instead of
            // reading it as a desynchronized stream.
            Err(error) => error_response(
                unparsed_request_id(&line),
                "invalid_request",
                error.to_string(),
            ),
        };
        write_json_line(&response)?;
    }
    Ok(())
}

fn write_json_line(response: &RpcResponse) -> Result<(), OrbitError> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, response)
        .map_err(|error| OrbitError::Execution(error.to_string()))?;
    stdout
        .write_all(b"\n")
        .and_then(|_| stdout.flush())
        .map_err(|error| OrbitError::Execution(error.to_string()))
}

fn parse_fastembed_model(name: &str) -> Result<EmbeddingModel, OrbitError> {
    name.parse::<EmbeddingModel>()
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))
}

fn default_model_dir(model_id: &str) -> PathBuf {
    CompanionPaths::default_under_home()
        .map(|paths| paths.model_dir(model_id))
        .unwrap_or_else(|_| PathBuf::from(".orbit/embed/models").join(model_id))
}

fn write_install_marker(args: &Args) -> Result<(), OrbitError> {
    let spec = ModelSpec::parse(&args.model)?;
    let model_dir = args
        .model_path
        .clone()
        .unwrap_or_else(|| default_model_dir(spec.alias));
    fs::create_dir_all(&model_dir).map_err(|error| OrbitError::Io(error.to_string()))?;
    let marker = serde_json::json!({
        "model": spec.alias,
        "fastembed_model": spec.fastembed_name,
        "version": env!("CARGO_PKG_VERSION"),
    });
    fs::write(
        model_dir.join("orbit-model.json"),
        serde_json::to_vec_pretty(&marker)
            .map_err(|error| OrbitError::Execution(error.to_string()))?,
    )
    .map_err(|error| OrbitError::Io(error.to_string()))
}

fn error_response(id: u64, code: &str, message: String) -> RpcResponse {
    RpcResponse::Error {
        id,
        error: RpcError {
            code: code.to_string(),
            message,
        },
    }
}

fn is_broken_pipe(error: &OrbitError) -> bool {
    let message = error.to_string();
    message.contains("Broken pipe") || message.contains("os error 32")
}
