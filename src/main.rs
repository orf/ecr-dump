use anyhow::bail;
use aws_sdk_ecr::types::{DescribeImagesFilter, ImageDetail, ImageIdentifier, TagStatus};
use aws_sdk_ecr::Client;
use clap::Parser;
use futures_util::stream::{self as stream, StreamExt};
use indicatif::{MultiProgress, ProgressBar, ProgressIterator, ProgressStyle};
use itertools::Itertools;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser, Debug)]
pub struct Args {
    output: PathBuf,
    #[arg(short, long, default_value = "10")]
    concurrency: usize,
}

#[derive(Serialize)]
pub struct ImageOutput {
    repository_name: String,
    image_digest: Option<String>,
    image_tag: Option<String>,
    manifest: Value,
    image_pushed_at: i64,
    last_recorded_pull_time: Option<i64>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let mut writer = BufWriter::new(File::create(args.output)?);

    let progress = MultiProgress::new();

    let template = ProgressStyle::with_template("{msg} {wide_bar} {pos}/{len}")?;

    let shared_config = aws_config::load_from_env().await;
    let client = Client::new(&shared_config);
    let repositories: Result<Vec<_>, _> = client
        .describe_repositories()
        .set_max_results(Some(1000))
        .into_paginator()
        .items()
        .send()
        .collect()
        .await;
    let repositories = repositories?;
    println!("Found {} repositories", repositories.len());

    let mut images = Vec::with_capacity(repositories.len());

    let repo_bar = progress.add(
        ProgressBar::new(repositories.len() as u64)
            .with_message("Processing repositories")
            .with_style(template.clone()),
    );

    for repository in repositories.into_iter().progress_with(repo_bar) {
        let repo_images: Result<Vec<_>, _> = client
            .describe_images()
            .set_repository_name(repository.repository_name)
            .set_max_results(Some(1000))
            .filter(
                DescribeImagesFilter::builder()
                    .set_tag_status(Some(TagStatus::Any))
                    .build(),
            )
            .into_paginator()
            .items()
            .send()
            .collect()
            .await;
        let repo_images = repo_images?;
        images.extend(repo_images);
    }

    println!("Found {} images", images.len());

    let bar = progress.add(
        ProgressBar::new(images.len() as u64)
            .with_message("Processing images by repository")
            .with_style(template.clone()),
    );

    for (repo_name, images) in images
        .into_iter()
        .progress_with(bar)
        .chunk_by(|i| i.repository_name.clone())
        .into_iter()
    {
        let images = images.collect_vec();
        let image_bar = progress.add(
            ProgressBar::new(images.len() as u64)
                .with_message("Processing images")
                .with_style(template.clone()),
        );
        let iter = images.into_iter().progress_with(image_bar).chunks(100);
        let mut stream = stream::iter(iter.into_iter())
            .map(|image_chunks| {
                let image_chunks: HashMap<String, ImageDetail> = image_chunks
                    .into_iter()
                    .map(|i| (i.image_digest.clone().unwrap(), i.clone()))
                    .collect();
                image_chunks
            })
            .map(|image_chunks| fetch_image_data(&client, image_chunks, repo_name.clone()))
            .buffer_unordered(args.concurrency);

        while let Some(output) = stream.next().await {
            for item in output? {
                serde_json::to_writer(&mut writer, &item)?;
                writeln!(&mut writer)?;
            }
        }
    }
    Ok(())
}

async fn fetch_image_data(
    client: &Client,
    image_chunks: HashMap<String, ImageDetail>,
    repo_name: Option<String>,
) -> anyhow::Result<Vec<ImageOutput>> {
    let image_ids: Vec<_> = image_chunks
        .values()
        .map(|d| {
            ImageIdentifier::builder()
                .image_digest(d.image_digest.as_ref().unwrap())
                .build()
        })
        .collect();
    let response = client
        .batch_get_image()
        .set_repository_name(repo_name)
        .set_image_ids(Some(image_ids))
        .send()
        .await?;
    if let Some(ref failures) = response.failures {
        if !failures.is_empty() {
            bail!("Failures: {:#?}", failures);
        }
    }

    let results: Result<Vec<_>, _> = response
        .images
        .unwrap()
        .into_iter()
        .map(|image| {
            let image_id = image.image_id.unwrap();
            let digest = image_id.image_digest().unwrap();
            let image_detail = image_chunks.get(digest).unwrap();
            let manifest: Value = serde_json::from_str(&image.image_manifest.unwrap())?;
            Ok::<_, anyhow::Error>(ImageOutput {
                repository_name: image.repository_name.unwrap(),
                image_digest: image_id.image_digest,
                image_tag: image_id.image_tag,
                manifest,
                image_pushed_at: image_detail.image_pushed_at.unwrap().secs(),
                last_recorded_pull_time: image_detail.last_recorded_pull_time.map(|v| v.secs()),
            })
        })
        .collect();

    results
}
