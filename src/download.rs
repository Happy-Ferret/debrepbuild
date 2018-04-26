use std::{
    env,
    fs::{create_dir_all, File},
    io,
    path::{Path, PathBuf},
    process::Command,
};

use rayon::prelude::*;
use reqwest::{self, header::ContentLength, Client, Response};

use config::{Direct, PackageEntry, Source};

/// Possible errors that may happen when attempting to download Debian packages and source code.
#[derive(Debug, Fail)]
pub enum DownloadError {
    #[fail(display = "unable to download '{}': {}", item, why)]
    Request { item: String, why:  reqwest::Error },
    #[fail(display = "unable to open '{}': {}", item, why)]
    File { item: String, why:  io::Error },
}

#[derive(Debug, Fail)]
pub enum SourceError {
    #[fail(display = "build command failed: {}", why)]
    BuildCommand { why: io::Error },
    #[fail(display = "failed to build from source")]
    BuildFailed,
    #[fail(display = "git command failed")]
    GitFailed,
    #[fail(display = "unable to git '{}': {}", item, why)]
    GitRequest { item: String, why:  io::Error },
    #[fail(display = "unsupported cvs for source: {}", cvs)]
    UnsupportedCVS { cvs: String },
}

/// Possible messages that may be returned when a download has succeeded.
pub enum DownloadResult {
    Downloaded(u64),
    AlreadyExists,
}

pub enum SourceResult {
    BuildSucceeded,
}

/// Given an item with a URL, download the item if the item does not already exist.
fn download<P: PackageEntry>(client: &Client, item: &P) -> Result<DownloadResult, DownloadError> {
    eprintln!(" - {}", item.get_name());

    let parent = item.destination();
    let filename = item.file_name();
    let destination = parent.join(filename);

    let dest_result = if destination.exists() {
        let mut capacity = File::open(&destination)
            .and_then(|file| file.metadata().map(|x| x.len()))
            .unwrap_or(0);

        let response = client
            .head(item.get_url())
            .send()
            .map_err(|why| DownloadError::Request {
                item: item.get_name().to_owned(),
                why,
            })?;

        if check_length(&response, capacity) {
            return Ok(DownloadResult::AlreadyExists);
        }

        File::create(destination)
    } else {
        create_dir_all(&parent).and_then(|_| File::create(destination))
    };

    let mut dest = dest_result.map_err(|why| DownloadError::File {
        item: item.get_name().to_owned(),
        why,
    })?;

    let mut response = client
        .get(item.get_url())
        .send()
        .map_err(|why| DownloadError::Request {
            item: item.get_name().to_owned(),
            why,
        })?;

    response
        .copy_to(&mut dest)
        .map(|x| DownloadResult::Downloaded(x))
        .map_err(|why| DownloadError::Request {
            item: item.get_name().to_owned(),
            why,
        })
}

/// Compares the length reported by the requested header to the length of existing file.
fn check_length(response: &Response, compared: u64) -> bool {
    response
        .headers()
        .get::<ContentLength>()
        .map(|len| **len)
        .unwrap_or(0) == compared
}

/// Attempts to build Debian packages from a given software repository.
fn build(item: &Source, path: &Path, branch: &str) -> Result<SourceResult, SourceError> {
    let _ = env::set_current_dir(path);
    if let Some(ref prebuild) = item.prebuild {
        for command in prebuild {
            let exit_status = Command::new("sh")
                .args(&["-c", command])
                .status()
                .map_err(|why| SourceError::BuildCommand { why })?;

            if !exit_status.success() {
                return Err(SourceError::BuildFailed);
            }
        }
    }

    let exit_status = Command::new("sbuild")
        .arg("--arch-all")
        .arg(format!("--dist={}", branch))
        .arg("--quiet")
        .arg(".")
        .status()
        .map_err(|why| SourceError::BuildCommand { why })?;

    if exit_status.success() {
        Ok(SourceResult::BuildSucceeded)
    } else {
        Err(SourceError::BuildFailed)
    }
}

/// Downloads the source repository via git, then attempts to build it.
fn download_git(item: &Source, branch: &str) -> Result<SourceResult, SourceError> {
    let path = PathBuf::from(["sources/", item.get_name()].concat());

    if path.exists() {
        let exit_status = Command::new("git")
            .args(&["-C", "sources", "pull", "origin", "master"])
            .status()
            .map_err(|why| SourceError::GitRequest {
                item: item.get_name().to_owned(),
                why,
            })?;

        if !exit_status.success() {
            return Err(SourceError::GitFailed);
        }
    } else {
        let exit_status = Command::new("git")
            .args(&["-C", "sources", "clone", item.get_url()])
            .status()
            .map_err(|why| SourceError::GitRequest {
                item: item.get_name().to_owned(),
                why,
            })?;

        if !exit_status.success() {
            return Err(SourceError::GitFailed);
        }
    }

    build(item, &path, branch)
}

/// Downloads pre-built Debian packages in parallel
pub fn parallel(items: &[Direct]) -> Vec<Result<DownloadResult, DownloadError>> {
    eprintln!("downloading packages in parallel");
    let client = Client::new();
    items
        .par_iter()
        .map(|item| download(&client, item))
        .collect()
}

/// Downloads source code repositories and builds them in parallel.
pub fn parallel_sources(items: &[Source], branch: &str) -> Vec<Result<SourceResult, SourceError>> {
    eprintln!("downloading sources in parallel");
    items
        .par_iter()
        .map(|item| match item.cvs.as_str() {
            "git" => download_git(item, branch),
            _ => Err(SourceError::UnsupportedCVS {
                cvs: item.cvs.clone(),
            }),
        })
        .collect()
}
