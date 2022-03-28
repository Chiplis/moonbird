use std::io::{Cursor};
use std::time::Duration;
use again::RetryPolicy;
use bytes::Bytes;
use clap::Parser;
use futures::{StreamExt};
use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};

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
        let response = client.post("https://api.twitter.com/1.1/guest/activate.json")
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
        let id: &String = &id.split("https://twitter.com/i/spaces/")
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
        let res = client.get(address.to_string())
            .header(AUTHORIZATION, bearer)
            .header("X-Guest-Token", guest.guest_token.clone())
            .send()
            .await
            .unwrap();
        res.json::<Space>().await.unwrap()
    }

    fn admins(&self) -> String {
        self.data.audio_space.participants.admins.iter().map(|admin| format!("{}{}", admin.display_name, ",")).collect()
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
        client.get(address)
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
    let space_name = space.name();

    if info {
        println!("Admins: {}\nTitle: {}\nLocation: {}", space.admins(), space_name, location);
    }

    let chunks = stream.chunks(guest, bearer).await;
    let chunks: Vec<String> = chunks
        .split("\n")
        .filter(|c| !c.contains("#")).
        map(str::to_string).collect();
    let size = chunks.len();

    let mut index = 0;
    let mut fragments = vec![vec![]; size].into_iter();
    let mut fragment_chunks = fragments.as_mut_slice().chunks_exact_mut(1);

    let chunks = chunks
        .iter()
        .map(|chunk| format!("{}{}", base_uri[0], chunk))
        .map(|chunk| {
            let fragment = &mut fragment_chunks.nth(0).unwrap()[0];
            let f = fetch_url(size, fragment, index, chunk);
            index += 1;
            f
        });

    futures::stream::iter(chunks).buffer_unordered(concurrency).collect::<()>().await;

    let name = name.clone().unwrap_or(space_name.clone()) + ".aac";
    let mut file = std::fs::File::create(name).unwrap();
    let bytes = Bytes::from(fragments.flatten().collect::<Vec<u8>>());
    let mut content = Cursor::new(bytes);
    std::io::copy(&mut content, &mut file).unwrap();
}

async fn fetch_url(size: usize, data: &mut Vec<u8>, index: i32, url: String) {
    let policy = RetryPolicy::exponential(Duration::from_secs(1))
        .with_max_retries(5)
        .with_jitter(true);

    let response = policy.retry(|| reqwest::get(url.clone())).await;
    if response.is_err() {
        panic!("Error while downloading chunk #{}, response: {:?}", index + 1, response)
    }
    let response = response.unwrap();

    let bytes = response.bytes().await.unwrap();
    data.append(&mut bytes.to_vec());
    println!("Chunk #{} of {} Downloaded", index + 1, size);
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

    // Name for the generated audio file
    #[clap(short, long)]
    path: Option<String>,

    // Maximum allowed amount of concurrent fragment requests while downloading space
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
    started_at: i64,
    ended_at: String,
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