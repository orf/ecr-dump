use crate::progress::{set_span_progress, span_set_spinner};
use crate::repos::RepositoryName;
use anyhow::{bail, Context};
use aws_sdk_ecr::types::{DescribeImagesFilter, ImageDetail, ImageIdentifier, TagStatus};
use aws_sdk_ecr::Client;
use aws_smithy_types_convert::date_time::DateTimeExt;
use chrono::{DateTime, Utc};
use futures_util::stream::{self as stream, StreamExt};
use futures_util::TryStreamExt;
use itertools::Itertools;
use oci_spec::image::{Descriptor, ImageIndex, ImageManifest};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use tracing::{debug, instrument, trace};
use tracing_indicatif::span_ext::IndicatifSpanExt;

#[derive(Debug, Hash, Eq, PartialEq, Copy, Clone, strum::Display, Serialize)]
pub enum ManifestType {
    Image,
    List,
}

impl ManifestType {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "application/vnd.oci.image.manifest.v1+json"
            | "application/vnd.docker.distribution.manifest.v2+json" => Some(Self::Image),
            "application/vnd.oci.image.index.v1+json"
            | "application/vnd.docker.distribution.manifest.list.v2+json" => Some(Self::List),
            _ => None,
        }
    }
}

pub type ManifestDigest = String;

#[derive(Debug, Hash, Eq, PartialEq, Clone, Serialize)]
pub struct RepositoryImage {
    repository_name: RepositoryName,
    manifest_digest: ManifestDigest,
    manifest_type: ManifestType,
    image_tags: Vec<String>,
    image_pushed_at: DateTime<Utc>,
    last_recorded_pull_time: Option<DateTime<Utc>>,
}

impl Display for RepositoryImage {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} digest={} type={} tags={:?}",
            self.repository_name, self.manifest_digest, self.manifest_type, self.image_tags
        )
    }
}

impl RepositoryImage {
    pub fn from_image_detail(detail: ImageDetail) -> Option<Self> {
        if let Some(manifest_type) = ManifestType::from_str(detail.image_manifest_media_type()?) {
            Some(Self {
                repository_name: detail.repository_name?,
                manifest_digest: detail.image_digest?,
                manifest_type,
                image_tags: detail.image_tags.unwrap_or_default(),
                image_pushed_at: detail.image_pushed_at?.to_chrono_utc().unwrap(),
                last_recorded_pull_time: detail
                    .last_recorded_pull_time
                    .map(|v| v.to_chrono_utc().unwrap()),
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ImageManifestWithDescriptor {
    pub manifest: ImageManifest,
    pub descriptor: Option<Descriptor>,
}

#[derive(Debug, Serialize)]
pub struct ImageWithManifests {
    pub image: RepositoryImage,
    pub manifests: Vec<ImageManifestWithDescriptor>,
}

pub struct ImageFetcher {
    client: Client,
    repo_name: RepositoryName,
    page_size: i32,
    chunk_size: usize,
    pub concurrency: usize,
}

impl Display for ImageFetcher {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.repo_name)
    }
}

impl ImageFetcher {
    #[allow(dead_code)]
    pub fn new(client: Client, repo_name: RepositoryName) -> Self {
        Self::new_with_config(client, repo_name, 1000, 100, 10)
    }

    #[allow(dead_code)]
    pub fn new_with_concurrency(client: Client, repo_name: RepositoryName, concurrency: usize) -> Self {
        Self::new_with_config(client, repo_name, 1000, 100, concurrency)
    }

    pub fn new_with_config(
        client: Client,
        repo_name: RepositoryName,
        page_size: i32,
        chunk_size: usize,
        concurrency: usize,
    ) -> Self {
        Self {
            repo_name,
            client,
            page_size,
            chunk_size,
            concurrency
        }
    }

    #[instrument(skip_all, fields(repo = %self))]
    pub async fn fetch_images(&self) -> anyhow::Result<Vec<RepositoryImage>> {
        let mut image_details = vec![];
        let span = span_set_spinner();
        let mut stream = self
            .client
            .describe_images()
            .set_repository_name(Some(self.repo_name.clone()))
            .set_max_results(Some(self.page_size))
            .filter(
                DescribeImagesFilter::builder()
                    .set_tag_status(Some(TagStatus::Any))
                    .build(),
            )
            .into_paginator()
            .items()
            .send();

        while let Some(item) = stream.next().await {
            image_details.push(item?);
            span.pb_inc(1);
        }

        Ok(image_details
            .into_iter()
            .filter_map(RepositoryImage::from_image_detail)
            .collect())
    }

    #[instrument(skip_all, fields(repo = %self))]
    pub async fn resolve_images<'a>(
        &'a self,
        images: &'a [RepositoryImage],
    ) -> anyhow::Result<Vec<ImageWithManifests>> {
        let (mut resolved_images, images_with_manifest_lists) = self
            .resolve_image_descriptors(images)
            .await
            .context("Resolve image descriptors")?;
        debug!(
            "Resolved {} images, and {} images with manifest lists to be resolved",
            resolved_images.len(),
            images_with_manifest_lists.len()
        );
        if !images_with_manifest_lists.is_empty() {
            let mut manifest_list_resolved = self
                .resolve_image_manifests(images_with_manifest_lists)
                .await
                .context("Resolve image manifests")?;
            debug!(
                "Resolved {} images from images with manifest lists",
                manifest_list_resolved.len()
            );
            resolved_images.append(&mut manifest_list_resolved);
        }

        Ok(resolved_images)
    }

    #[instrument(skip_all, fields(repo = %self))]
    pub async fn resolve_image_manifests<'a>(
        &'a self,
        images_with_manifest_lists: Vec<(&'a RepositoryImage, Vec<Descriptor>)>,
    ) -> anyhow::Result<Vec<ImageWithManifests>> {
        let mut resolved_images = Vec::with_capacity(images_with_manifest_lists.len());

        let span = set_span_progress(images_with_manifest_lists.len());

        let all_results: Vec<_> = stream::iter(images_with_manifest_lists.iter())
            .map(|(image, descriptors)| {
                let manifest_digests_map: HashMap<_, _> = descriptors
                    .iter()
                    .map(|d| (d.digest(), (*image, d)))
                    .collect();
                async move {
                    self.batch_resolve_image_manifests(manifest_digests_map)
                        .await
                        .with_context(|| format!("Error resolving manifest for image {image:?}"))
                }
            })
            .buffer_unordered(self.concurrency)
            .inspect(|_| span.pb_inc(1))
            .try_collect()
            .await?;

        let grouping_map = all_results
            .into_iter()
            .flatten()
            .into_group_map_by(|((img, _), _, _)| *img);
        for (image, results) in grouping_map {
            let mut parsed_manifests = vec![];
            for ((_, descriptor), _, resolved_manifest) in results {
                match ManifestType::from_str(&resolved_manifest.media_type) {
                    Some(ManifestType::Image) => {
                        let manifest: ImageManifest =
                            serde_json::from_str(&resolved_manifest.manifest)?;
                        parsed_manifests.push(ImageManifestWithDescriptor {
                            manifest,
                            descriptor: Some(descriptor.clone()),
                        });
                    }
                    Some(_) => {
                        bail!("Manifest list item resolved to another manifest list!")
                    }
                    None => {}
                }
            }
            resolved_images.push(ImageWithManifests {
                image: image.clone(),
                manifests: parsed_manifests,
            });
        }

        Ok(resolved_images)
    }

    #[instrument(skip_all, fields(repo = %self))]
    pub async fn resolve_image_descriptors<'a>(
        &'a self,
        images: &'a [RepositoryImage],
    ) -> anyhow::Result<(
        Vec<ImageWithManifests>,
        Vec<(&'a RepositoryImage, Vec<Descriptor>)>,
    )> {
        let mut resolved_images = vec![];
        let mut images_with_manifest_lists = vec![];

        let span = set_span_progress(images.len());

        let all_results: Vec<_> = stream::iter(images.chunks(self.chunk_size))
            .map(|chunk| {
                let map: HashMap<_, _> = chunk.iter().map(|v| (&v.manifest_digest, v)).collect();
                async {
                    self.batch_resolve_image_manifests(map)
                        .await
                        .context("Error resolving manifest")
                }
            })
            .buffer_unordered(self.concurrency)
            .inspect(|_| span.pb_inc(self.chunk_size as u64))
            .try_collect()
            .await?;

        for results in all_results.into_iter() {
            for (repo_image, _, resolved_manifest) in results {
                match ManifestType::from_str(&resolved_manifest.media_type) {
                    None => {}
                    Some(ManifestType::Image) => {
                        let manifest: ImageManifest =
                            serde_json::from_str(&resolved_manifest.manifest)?;
                        resolved_images.push(ImageWithManifests {
                            image: repo_image.clone(),
                            manifests: vec![ImageManifestWithDescriptor {
                                manifest,
                                descriptor: None,
                            }],
                        })
                    }
                    Some(ManifestType::List) => {
                        let parsed: ImageIndex = serde_json::from_str(&resolved_manifest.manifest)?;
                        images_with_manifest_lists.push((repo_image, parsed.manifests().clone()));
                    }
                }
            }
        }

        Ok((resolved_images, images_with_manifest_lists))
    }

    #[instrument(skip_all, fields(repo = %self), level = "trace")]
    async fn batch_resolve_image_manifests<T: std::fmt::Debug + Clone>(
        &self,
        digests: HashMap<&String, T>,
    ) -> anyhow::Result<Vec<(T, ManifestDigest, ResolvedManifest)>> {
        let identifiers = digests
            .keys()
            .map(|digest| {
                ImageIdentifier::builder()
                    .image_digest(digest.to_string())
                    .build()
            })
            .collect_vec();
        trace!("identifiers={identifiers:#?}");
        let response = self
            .client
            .batch_get_image()
            .set_repository_name(Some(self.repo_name.clone()))
            .set_image_ids(Some(identifiers))
            .send()
            .await?;
        let images = response.images.expect("No image output");

        trace!("digests={digests:#?}");
        trace!("images={images:#?}");

        let unique_images: Vec<_> = images
            .into_iter()
            .map(|img| {
                (
                    img.image_id.unwrap().image_digest.unwrap(),
                    img.image_manifest.unwrap(),
                    img.image_manifest_media_type.unwrap(),
                )
            })
            .unique()
            .collect();

        let mut results = vec![];
        for (digest, manifest, media_type) in unique_images {
            let d_value = digests
                .get(&digest)
                .with_context(|| format!("Digest {digest} not present in map"))?;
            trace!("Found digest={digest} for d_value={d_value:?}");
            results.push((
                d_value.clone(),
                digest,
                ResolvedManifest {
                    manifest,
                    media_type,
                },
            ))
        }
        Ok(results)
    }
}

struct ResolvedManifest {
    manifest: String,
    media_type: String,
}
