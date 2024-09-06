mod images;
mod progress;
mod repos;

use crate::images::{ImageFetcher, ImageWithManifests};
use crate::repos::{RepositoryLister, RepositoryName};
use anyhow::Context;
use aws_sdk_ecr::Client;
use clap::Parser;
use futures_util::stream::{self as stream, StreamExt};
use globset::{Glob, GlobSet};
use oci_spec::image::Descriptor;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, instrument, Level};
use tracing_indicatif::span_ext::IndicatifSpanExt;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::filter::Directive;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
pub struct Args {
    output: PathBuf,

    #[arg(short, long, default_value = "10")]
    concurrency: usize,

    #[arg(long)]
    include: Option<Vec<Glob>>,

    #[arg(long)]
    exclude: Option<Vec<Glob>>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RepositoryImage {
    repository_name: String,
    image_tags: Option<Vec<String>>,
    image_digest: String,
    image_pushed_at: i64,
    last_recorded_pull_time: Option<i64>,
    layers: Vec<Descriptor>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let indicatif_layer = IndicatifLayer::new().with_max_progress_bars(14, None);
    let env_builder = EnvFilter::builder()
        .with_default_directive(Directive::from(Level::INFO))
        .from_env()?;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_thread_names(true)
                .with_writer(indicatif_layer.get_stderr_writer()),
        )
        .with(indicatif_layer)
        .with(env_builder)
        .init();

    info!("Started");

    let shared_config = aws_config::load_from_env().await;
    let client = Client::new(&shared_config);

    let include_filter = args.include.map(build_globset).transpose()?;
    let exclude_filter = args.exclude.map(build_globset).transpose()?;

    let repo_lister = RepositoryLister::new(client.clone(), include_filter, exclude_filter);
    let repo_names = repo_lister.list().await?;
    info!("Discovered {} repositories", repo_names.len());
    debug!("Repo names: {:?}", repo_names);

    let output = tokio::io::BufWriter::new(tokio::fs::File::create(args.output).await?);
    run(client, repo_names, output, args.concurrency).await?;

    Ok(())
}

#[instrument(skip_all)]
async fn run(
    client: Client,
    repo_names: Vec<String>,
    mut output: tokio::io::BufWriter<tokio::fs::File>,
    concurrency: usize,
) -> anyhow::Result<()> {
    let span = progress::set_span_progress("repos", repo_names.len());

    let mut stream = stream::iter(
        repo_names
            .into_iter()
            .map(|val| fetch_repo(client.clone(), val, concurrency)),
    )
    .buffer_unordered(concurrency);

    let mut buffer = vec![];
    while let Some(repo_result) = stream.next().await {
        let (name, repo_images) = repo_result?;
        info!(
            "Discovered {} images in repository {name}",
            repo_images.len()
        );
        for image in repo_images {
            serde_json::to_writer(&mut buffer, &image)?;
            buffer.push(b'\n');
            output.write_all(&buffer).await?;
            buffer.clear();
        }
        span.pb_inc(1);
        output.flush().await?;
    }
    output.flush().await?;
    Ok(())
}

#[instrument(skip(client))]
async fn fetch_repo(
    client: Client,
    repo_name: RepositoryName,
    concurrency: usize,
) -> anyhow::Result<(RepositoryName, Vec<ImageWithManifests>)> {
    let image_fetcher = ImageFetcher::new_with_concurrency(client, repo_name.clone(), concurrency);
    let images = image_fetcher.fetch_images().await?;
    debug!("Found {} images:", images.len());
    let resolved = image_fetcher
        .resolve_images(&images)
        .await
        .with_context(|| format!("Resolving {repo_name}"))?;
    debug!("Resolved {} images with manifests", resolved.len());
    Ok((repo_name, resolved))
}

fn build_globset(globs: Vec<Glob>) -> anyhow::Result<GlobSet> {
    let mut builder = globset::GlobSetBuilder::new();
    for glob in globs {
        builder.add(glob);
    }
    Ok(builder.build()?)
}
