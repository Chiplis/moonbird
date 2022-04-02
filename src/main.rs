use serde::{Deserialize, Serialize};
use again::RetryPolicy;
use clap::Parser;
use futures::{stream::iter, StreamExt};
use anyhow::{Context, Result, anyhow};
use std::{
  fs::OpenOptions,
  io::Write,
  sync::atomic::{AtomicUsize, Ordering},
  time::Duration
};
use reqwest::header::AUTHORIZATION;
use tokio::{
  fs::{File, remove_file, write, read},
};

#[tokio::main]
async fn main() -> Result<()> {
  let args = Args::parse();

  if args.file.is_none() {
    println!("No name specified, will create audio file with default space name");
  }

  Guest::new(&args.bearer).await?
    .space(&args.space).await?
    .download(args.file, args.concurrency).await?;

  println!("\nDone");
  Ok(())
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
  /// ID of the space to download
  #[clap(short, long)]
  space: String,

  /// Name for the generated audio file
  #[clap(short, long)]
  file: Option<String>,

  /// Maximum allowed amount of concurrent fragment requests while downloading space
  #[clap(short, long, default_value_t = 50)]
  concurrency: usize,

  /// Authentication token to get required metadata
  #[clap(
  short, long,
  default_value = "AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs=1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA"
  )]
  bearer: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Guest {
  bearer_token: String,
  guest_token: String,
}

impl Guest {
  async fn new(bearer: &str) -> Result<Guest> {
    let client = reqwest::Client::new();
    let bearer_token = format!("Bearer {}", bearer);

    let guest_token = client
      .post("https://api.twitter.com/1.1/guest/activate.json")
      .header(AUTHORIZATION, &bearer_token)
      .send().await
      .with_context(|| "Error fetching guest token".to_string() )?
      .json::<serde_json::Value>().await
      .with_context(|| "Guest token responsa was not json".to_string())?
      .get("guest_token")
      .and_then(|f| f.as_str())
      .ok_or_else(|| anyhow!("No guest_token attribute found"))?
      .to_string();

    println!("Guest:\n{:?}", guest_token);
    Ok(Self { bearer_token, guest_token })
  }

  async fn space<'a>(&'a self, id: &str) -> Result<Space<'a>> {
    Space::new(self, id).await
  }

  async fn get(&self, url: &str) -> Result<reqwest::Response> {
    Ok(reqwest::Client::new()
      .get(url)
      .header(AUTHORIZATION, &self.bearer_token)
      .header("X-Guest-Token", &self.guest_token)
      .send().await?)
  }
}

struct Space<'a> {
  guest: &'a Guest,
  attrs: SpaceAttrs,
  name: String,
  admins: String,
}

impl<'a> Space<'a> {
  async fn new(guest: &'a Guest, id: &str) -> Result<Space<'a>> {
    let id = id
      .split('?')
      .collect::<Vec<&str>>()[0]
      .replace("https://", "")
      .replace("twitter.com/i/spaces/", "")
      .replace("/", "");

    let address = format!(
      "https://twitter.com/i/api/graphql/Uv5R_-Chxbn1FEkyUkSW2w/AudioSpaceById?variables=%7B%22id%22%3A%22{}%22%2C%22isMetatagsQuery%22%3Afalse%2C%22withBirdwatchPivots%22%3Afalse%2C%22withDownvotePerspective%22%3Afalse%2C%22withReactionsMetadata%22%3Afalse%2C%22withReactionsPerspective%22%3Afalse%2C%22withReplays%22%3Afalse%2C%22withScheduledSpaces%22%3Afalse%2C%22withSuperFollowsTweetFields%22%3Afalse%2C%22withSuperFollowsUserFields%22%3Afalse%7D",
      id
    );

    println!("{}", address);

    let res = guest.get(&address).await?;

    let attrs = res.json::<SpaceAttrs>().await?;

    let name = attrs.data.audio_space.metadata.title
      .chars()
      .filter(|c| c.is_alphanumeric() || c.is_whitespace() || "—-_".contains(&c.to_string()))
      .collect();

    let admins = attrs.data.audio_space.participants.admins
      .iter()
      .map(|admin| format!("{}{}", admin.display_name, ","))
      .collect();

    Ok(Self{ guest, attrs, name, admins })
  }

  async fn download(&self, name: Option<String>, concurrency: usize) -> Result<()> {
    let stream = self.stream().await?;

    println!("Admins: {}\nTitle: {}\nLocation: {}", self.admins, self.name, stream.location());
    
    let filename = format!("{}.aac", name.as_ref().unwrap_or(&self.name));
    let _ = remove_file(&filename).await;
    File::create(&filename).await?;

    let mut space_file = OpenOptions::new().append(true).open(&filename)?;

    for path in stream.fetch_chunks(concurrency).await? {
      space_file.write_all(&read(&path).await?)?;
      remove_file(&path).await?;
    }

    Ok(())
  }

  async fn stream(&'a self) -> Result<Stream<'a>> {
    Stream::new(self).await
  }
}

struct Stream<'a> {
  space: &'a Space<'a>,
  attrs: StreamAttrs,
}

impl<'a> Stream<'a> {
  pub async fn new(space: &'a Space<'a>) -> Result<Stream<'a>> {
    let address = format!(
      "https://twitter.com/i/api/1.1/live_video_stream/status/{}",
      &space.attrs.data.audio_space.metadata.media_key
    );
    let attrs = space.guest.get(&address).await?.json::<StreamAttrs>().await?;

    Ok(Self{ attrs, space })
  }

  pub async fn fetch_chunks(&self, concurrency: usize) -> Result<Vec<String>> {
    let base_uri = self.location().split("playlist").next()
      .ok_or_else(|| anyhow!("Could not parse base_uri from location"))?;
    let count = AtomicUsize::new(0);

    let chunks = self.chunks().await?;
    let size = chunks.len();

    let futures = chunks.into_iter().enumerate().map(|(index, chunk_name)| {
      let client = reqwest::Client::new();
      let url = format!("{}{}", base_uri, chunk_name);
      let filename = format!("{}_{}", self.space.name, index);
      let policy = RetryPolicy::exponential(Duration::from_secs(1))
        .with_max_retries(5)
        .with_jitter(true);
      let count_ref = &count;

      async move {
        let bytes = policy.retry(|| client.get(&url).send() ).await
          .map_err(|e| anyhow!("Error while downloading chunk #{}:\n{}", index, e))?
          .bytes().await?;

        write(&filename, bytes).await?;
        count_ref.fetch_add(1, Ordering::SeqCst);

        print!("\rChunk {:>8}/{:<8}", index, size);

        Ok(filename)
      }
    });

    iter(futures).buffer_unordered(concurrency)
      .collect::<Vec<Result<String>>>().await
      .into_iter().collect()
  }

  async fn chunks(&self) -> Result<Vec<String>> {
    Ok(self.space.guest.get(self.location()).await?
      .text().await?
      .split('\n')
      .filter(|c| !c.contains('#'))
      .map(str::to_string)
      .collect())
  }

  pub fn location(&'a self) -> &'a str {
    &self.attrs.source.location
  }
}

#[derive(Debug, Deserialize)]
struct SpaceAttrs {
  data: Data,
}

#[derive(Debug, Serialize, Deserialize)]
struct Data {
  #[serde(rename(serialize = "audioSpace", deserialize = "audioSpace"))]
  audio_space: AudioSpace,
}

#[derive(Debug, Serialize, Deserialize)]
struct AudioSpace {
  metadata: Metadata,
  participants: Participants,
}

#[derive(Debug, Serialize, Deserialize)]
struct Metadata {
  media_key: String,
  title: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Participants {
  admins: Vec<Admin>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Admin {
  display_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StreamAttrs {
  source: Source,
}

#[derive(Debug, Serialize, Deserialize)]
struct Source {
  location: String,
}
