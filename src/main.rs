extern crate core;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use again::RetryPolicy;
use clap::Parser;
use futures::{StreamExt};
use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};
use tokio::fs::{File, remove_file, write};
use tokio::io::AsyncReadExt;

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let id = &args.space;

    let bearer = &format!("Bearer {}", args.bearer);

    let name = &args.path.or_else(|| {
        println!("No name specified, will create audio file with default space name");
        None
    });

    let concurrency = args.concurrency;

    download(id, name, true, bearer, concurrency).await;
}

#[derive(Debug, Serialize, Deserialize)]
struct Guest {
    guest_token: String,
}

impl Guest {
    async fn new(bearer: &String) -> Guest {
        let client = reqwest::Client::new();
        let response = client
            .post("https://api.twitter.com/1.1/guest/activate.json")
            .header(AUTHORIZATION, bearer)
            .send()
            .await
            .unwrap()
            .json::<Guest>()
            .await
            .unwrap();
        println!("Guest:\n{:?}", response);
        response
    }
}

impl Space {
    async fn new(guest: &Guest, bearer: &String, id: &String) -> Space {
        let id: &String = &id
            .split("https://twitter.com/i/spaces/")
            .map(str::to_string)
            .collect::<String>()
            .split("?")
            .map(str::to_string)
            .collect::<Vec<String>>()[0];

        let address = format!(
            "https://twitter.com/i/api/graphql/Uv5R_-Chxbn1FEkyUkSW2w/AudioSpaceById?variables=%7B%22id%22%3A%22{}%22%2C%22isMetatagsQuery%22%3Afalse%2C%22withBirdwatchPivots%22%3Afalse%2C%22withDownvotePerspective%22%3Afalse%2C%22withReactionsMetadata%22%3Afalse%2C%22withReactionsPerspective%22%3Afalse%2C%22withReplays%22%3Afalse%2C%22withScheduledSpaces%22%3Afalse%2C%22withSuperFollowsTweetFields%22%3Afalse%2C%22withSuperFollowsUserFields%22%3Afalse%7D",
            id
        );

        println!("{}", address);

        let client = reqwest::Client::new();
        let res = client
            .get(address.to_string())
            .header(AUTHORIZATION, bearer)
            .header("X-Guest-Token", guest.guest_token.clone())
            .send()
            .await
            .unwrap();

        res.json::<Space>().await.unwrap()
    }

    fn admins(&self) -> String {
        self.data.audio_space.participants.admins
            .iter()
            .map(|admin| format!("{}{}", admin.display_name, ","))
            .collect()
    }

    fn name(&self) -> &String {
        &self.data.audio_space.metadata.title
    }

    async fn stream(&self, guest: &Guest, bearer: &String) -> Stream {
        let address = format!(
            "https://twitter.com/i/api/1.1/live_video_stream/status/{}",
            &self.data.audio_space.metadata.media_key
        );
        let client = reqwest::Client::new();
        client
            .get(address)
            .header(AUTHORIZATION, bearer)
            .header("X-Guest-Token", guest.guest_token.clone())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

async fn download(id: &String, name: &Option<String>, info: bool, bearer: &String, concurrency: usize) {

    let guest = &Guest::new(bearer).await;
    let space = Space::new(guest, bearer, id).await;
    let stream = space.stream(&guest, bearer).await;

    let location = stream.source.location.as_str();
    let base_uri: Vec<String> = location.split("playlist").map(str::to_string).collect();
    let space_name: &String = &space
        .name()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();

    if info {
        println!("Admins: {}\nTitle: {}\nLocation: {}", space.admins(), space_name, location);
    }

    let chunks: Vec<String> = stream
        .chunks(guest, bearer)
        .await
        .split("\n")
        .filter(|c| !c.contains("#"))
        .map(str::to_string)
        .collect();
    let size = chunks.len();

    let mut index = 1;

    let count = AtomicUsize::new(0);
    let chunks = chunks
        .iter()
        .map(|chunk| format!("{}{}", base_uri[0], chunk))
        .map(|chunk_url| {
            let f = fetch_url(&space_name, size, index, chunk_url, &count);
            index += 1;
            f
        });

    futures::stream::iter(chunks).buffer_unordered(concurrency).collect::<()>().await;

    let bytes = &mut vec![];
    for i in 1..index {
        let path = format!("{}_{}", &space_name, i);
        File::open(&path).await.unwrap().read_to_end(bytes).await.unwrap();
        remove_file(&path).await.unwrap();
    }

    let name = name.clone().unwrap_or(space_name.clone()) + ".aac";
    write(name, bytes.as_slice()).await.unwrap();
}

async fn fetch_url(space_name: &String, size: usize, index: i32, url: String, count: &AtomicUsize) {
    let policy = RetryPolicy::exponential(Duration::from_secs(1))
        .with_max_retries(5)
        .with_jitter(true);

    let response = policy.retry(|| reqwest::get(&url))
        .await
        .unwrap_or_else(|e| panic!("Error while downloading chunk #{}:\n{}", index, e));

    let bytes = response.bytes().await.unwrap();

    write(format!("{}_{}", space_name, index), bytes.to_vec().as_slice()).await.unwrap();
    count.fetch_add(1, Ordering::SeqCst);
    println!("Chunk #{} Downloaded - {} Remaining", index, size - count.load(Ordering::SeqCst));
}


impl Stream {
    async fn chunks(&self, guest: &Guest, bearer: &String) -> String {
        let client = reqwest::Client::new();
        let s = client.get(&self.source.location)
            .header(AUTHORIZATION, bearer)
            .header("X-Guest-Token", guest.guest_token.clone())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        s
    }
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// ID of the space to download
    #[clap(short, long)]
    space: String,

    /// Name for the generated audio file
    #[clap(short, long)]
    path: Option<String>,

    /// Maximum allowed amount of concurrent fragment requests while downloading space
    #[clap(short, long, default_value_t = 25)]
    concurrency: usize,

    /// Authentication token to get required metadata
    #[clap(
    short, long,
    default_value = "AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs=1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA"
    )]
    bearer: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Space {
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
struct Stream {
    source: Source,
}

#[derive(Debug, Serialize, Deserialize)]
struct Source {
    location: String,
}