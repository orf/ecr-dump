use aws_sdk_ecr::Client;
use globset::GlobSet;
use itertools::Itertools;
use tracing::{debug, instrument};

pub type RepositoryName = String;

pub struct RepositoryLister {
    client: Client,
    include_filter: Option<GlobSet>,
    exclude_filter: Option<GlobSet>,
    page_size: i32,
}

impl RepositoryLister {
    pub fn new(
        client: Client,
        include_filter: Option<GlobSet>,
        exclude_filter: Option<GlobSet>,
    ) -> Self {
        Self::new_with_page_size(client, include_filter, exclude_filter, 1000)
    }

    pub fn new_with_page_size(
        client: Client,
        include_filter: Option<GlobSet>,
        exclude_filter: Option<GlobSet>,
        page_size: i32,
    ) -> Self {
        Self {
            client,
            include_filter,
            exclude_filter,
            page_size,
        }
    }

    #[instrument(name = "List repositories", skip_all)]
    pub async fn list(&self) -> anyhow::Result<Vec<RepositoryName>> {
        let repositories: Result<Vec<_>, _> = self
            .client
            .describe_repositories()
            .set_max_results(Some(self.page_size))
            .into_paginator()
            .items()
            .send()
            .collect()
            .await;
        let repositories = repositories?
            .into_iter()
            .map(|r| r.repository_name.unwrap())
            .filter_map(|name| {
                let has_filter = self.include_filter.is_some() || self.exclude_filter.is_some();
                if !has_filter {
                    return Some(name);
                }
                if let Some(include_filter) = &self.include_filter {
                    if include_filter.is_match(&name) {
                        debug!("Include filter matched {name} - including");
                        return Some(name);
                    }
                }
                if let Some(exclude_filter) = &self.exclude_filter {
                    if !exclude_filter.is_match(&name) {
                        debug!("Exclude filter did not match {name} - including");
                        return Some(name);
                    }
                }
                debug!("No filter match for {name}, skipping");
                None
            })
            .sorted()
            .collect_vec();

        Ok(repositories)
    }
}
