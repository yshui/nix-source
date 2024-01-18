use anyhow::Context;
use argh::FromArgs;
use chrono::{DateTime, FixedOffset};
use std::collections::HashMap;
use std::io::{Seek, Write};
use std::os::unix::ffi::OsStrExt;

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug, Eq, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum SourceType {
    Tarball,
    File,
}

impl std::str::FromStr for SourceType {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tarball" => Ok(SourceType::Tarball),
            "file" => Ok(SourceType::File),
            _ => Err(anyhow::anyhow!("invalid source type")),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct Source {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    hash: Option<ssri::Integrity>,
    url: url::Url,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    last_modified: Option<DateTime<FixedOffset>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    etag: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none", default)]
    ty: Option<SourceType>,
}

impl Source {
    fn new(url: url::Url) -> Self {
        Self {
            hash: None,
            url,
            last_modified: None,
            etag: None,
            ty: None,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Sources {
    #[serde(default)]
    sources: HashMap<String, Source>,
}

trait Command {
    fn execute(self, sources: std::path::PathBuf) -> anyhow::Result<()>;
}

/// add a source to the sources file
#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "add")]
struct AddCommand {
    /// name of the source
    #[argh(positional)]
    name: String,
    /// url of the source
    #[argh(positional)]
    url: url::Url,
    /// type of the source, either tarball or file
    #[argh(option, short = 't', long = "type")]
    ty: Option<SourceType>,
}

fn sanitize_file_name(name: &str) -> String {
    let mut out = String::new();
    let mut chars = name.chars();
    if let Some(c) = chars.next() {
        if c == '.' {
            out.push('_')
        } else {
            out.push(c)
        }
    } else {
        return "source".to_string();
    }
    out.extend(chars.map(|c| match c {
        '0'..='9' | 'a'..='z' | 'A'..='Z' | '+' | '-' | '.' | '_' | '?' | '=' => c,
        _ => '_',
    }));
    out
}

fn refresh_source(source: &Source) -> anyhow::Result<Source> {
    let req = ureq::head(source.url.as_str());
    let req = if source.hash.is_some() {
        let req = if let Some(etag) = &source.etag {
            req.set("If-None-Match", etag)
        } else {
            req
        };
        if let Some(last_modified) = &source.last_modified {
            let time = last_modified.to_rfc2822();
            assert!(time.ends_with(" +0000"));
            let time = &time[..time.len() - 6];
            let time = format!("{} GMT", time);
            req.set("If-Modified-Since", &time)
        } else {
            req
        }
    } else {
        req
    };
    let res = req.call()?;
    if res.status() == 304 {
        println!("\tnot modified");
        return Ok(source.clone());
    }
    let etag = res.header("ETag").and_then(|s| {
        if s.starts_with("W/") {
            None
        } else {
            Some(s.to_string())
        }
    });
    let last_modified = res
        .header("Last-Modified")
        .and_then(|s| DateTime::parse_from_rfc2822(s).ok());
    let filename = res
        .header("Content-Disposition")
        .and_then(|s| {
            mailparse::parse_content_disposition(s)
                .params
                .get("filename")
                .map(|s| s.to_string())
        })
        .or_else(|| {
            source
                .url
                .path_segments()
                .into_iter()
                .flatten()
                .last()
                .map(|s| s.to_string())
        });
    println!("{last_modified:?} {etag:?} {filename:?}");
    let ty = if let Some(ty) = source.ty {
        ty
    } else if let Some(filename) = &filename {
        let filename = std::path::Path::new(&filename);
        let ext = filename.extension().unwrap_or_default();
        let stem = std::path::Path::new(filename.file_stem().unwrap_or_default());
        let ext2 = stem.extension().unwrap_or_default();
        if ext == "zip" || ext == "tgz" || ext2 == "tar" {
            SourceType::Tarball
        } else {
            SourceType::File
        }
    } else {
        SourceType::File
    };
    let mut command = std::process::Command::new("nix-prefetch-url");

    let store_name = filename
        .map(|s| sanitize_file_name(&s))
        .unwrap_or("source".to_owned());
    command.args(["--name", &store_name]);
    if ty == SourceType::Tarball {
        command.arg("--unpack");
    }

    command.arg(source.url.as_str());
    command.stderr(std::process::Stdio::inherit());
    let output = command.output()?;
    println!("{:?}", output.stdout);

    let hash = if output.stdout.ends_with(b"\n") {
        &output.stdout[..output.stdout.len() - 1]
    } else {
        &output.stdout
    };
    let hash = std::ffi::OsStr::from_bytes(hash);
    let hash = std::process::Command::new("nix")
        .args(["hash", "to-sri", "--type", "sha256"])
        .arg(hash)
        .output()?
        .stdout;
    let hash = String::from_utf8(hash)?.trim().to_owned();
    println!("{:?}", hash);
    Ok(Source {
        hash: Some(hash.parse()?),
        url: source.url.clone(),
        last_modified,
        etag,
        ty: Some(ty),
    })
}

impl Command for AddCommand {
    fn execute(self, sources: std::path::PathBuf) -> anyhow::Result<()> {
        let (mut file, mut sources): (_, Sources) = if !sources.exists() {
            let mut file = std::fs::File::create(&sources)?;
            write!(file, "{{}}")?; // Make sure the file is valid JSON even if we fail.
            (file, Default::default())
        } else {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&sources)?;
            let sources = serde_json::from_reader(&file)?;
            (file, sources)
        };
        if sources.sources.contains_key(&self.name) {
            anyhow::bail!("source {} already exists", self.name);
        }
        println!("Adding {}", self.name);
        let source = refresh_source(&Source::new(self.url))?;
        sources.sources.insert(self.name, source);
        file.seek(std::io::SeekFrom::Start(0))?;
        file.set_len(0)?;
        serde_json::to_writer_pretty(file, &sources)?;
        Ok(())
    }
}

/// update sources in the sources file
#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "update")]
struct UpdateCommand {
    /// name of the source
    #[argh(positional)]
    name: Option<String>,
}

impl Command for UpdateCommand {
    fn execute(self, sources: std::path::PathBuf) -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&sources)?;
        let mut sources: Sources = serde_json::from_reader(&file)?;
        let sources_mut: Vec<(&str, &mut Source)> = if let Some(name) = &self.name {
            vec![(
                &name,
                sources
                    .sources
                    .get_mut(name)
                    .with_context(|| format!("source {} does not exist", name))?,
            )]
        } else {
            sources
                .sources
                .iter_mut()
                .map(|(k, v)| (k.as_str(), v))
                .collect()
        };
        for (name, source) in sources_mut {
            println!("Updating {}", name);
            let new_source = refresh_source(source)?;
            source.hash = new_source.hash;
            source.last_modified = new_source.last_modified;
            source.etag = new_source.etag;
        }
        file.seek(std::io::SeekFrom::Start(0))?;
        file.set_len(0)?;
        serde_json::to_writer_pretty(file, &sources)?;
        Ok(())
    }
}

/// delete a source from the sources file
#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "rm")]
struct DeleteCommand {
    /// name of the source
    #[argh(positional)]
    name: String,
}

impl Command for DeleteCommand {
    fn execute(self, sources: std::path::PathBuf) -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&sources)?;
        let mut sources: Sources = serde_json::from_reader(&file)?;
        if sources.sources.remove(&self.name).is_none() {
            anyhow::bail!("source {} does not exist", self.name);
        }
        file.seek(std::io::SeekFrom::Start(0))?;
        file.set_len(0)?;
        serde_json::to_writer_pretty(file, &sources)?;
        Ok(())
    }
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum SubCommands {
    Add(AddCommand),
    Update(UpdateCommand),
    Delete(DeleteCommand),
}

impl Command for SubCommands {
    fn execute(self, sources: std::path::PathBuf) -> anyhow::Result<()> {
        match self {
            SubCommands::Add(cmd) => cmd.execute(sources),
            SubCommands::Update(cmd) => cmd.execute(sources),
            SubCommands::Delete(cmd) => cmd.execute(sources),
        }
    }
}

/// manipulate the sources.json file
#[derive(FromArgs, PartialEq, Debug)]
struct Options {
    #[argh(
        option,
        short = 's',
        default = "std::path::PathBuf::from(\"sources.json\")"
    )]
    /// the sources.json file
    sources: std::path::PathBuf,
    #[argh(subcommand)]
    subcommand: SubCommands,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    which::which("nix-prefetch-url").context("nix-prefetch-url not found")?;
    which::which("nix").context("nix not found")?;

    let opts = argh::from_env::<Options>();
    opts.subcommand.execute(opts.sources)?;
    Ok(())
}
