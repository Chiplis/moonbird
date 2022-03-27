use std::io::{Cursor};
use std::time::Duration;
use again::RetryPolicy;
use bytes::Bytes;
use clap::Parser;
use chashmap::CHashMap;
use futures::{StreamExt};
use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// ID of the space to download
    #[clap(short, long)]
    space: String,

    /// Authentication token to get required metadata
    #[clap(
        short,
        long,
        default_value = "AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs=1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA"
    )]
    bearer: String,

    // Name for the generated audio file
    #[clap(short, long)]
    path: Option<String>
}

#[tokio::main]
async fn main() {

    let args = Args::parse();

    let id = &args.space;

    let bearer = &format!("Bearer {}", args.bearer);

    let name = &args.path.or_else(|| {
        println!("No name specified, will create audio file with default space name");
        None
    });

    download(id, name,true, bearer).await;
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

impl Space {
    async fn new(guest: &Guest, bearer: &String, id: &String) -> Space {
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

    fn name(&self) -> String {
        format!(
            "{}",
            &self.data.audio_space.metadata.title
        )
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

async fn download(id: &String, name: &Option<String>, info: bool, bearer: &String) {
    let guest = &Guest::new(bearer).await;
    let space = Space::new(guest, bearer, id).await;
    let stream = space.stream(&guest, bearer).await;
    let location = stream.source.location.as_str();
    let base: Vec<String> = location.split("playlist").map(str::to_string).collect();
    if info {
        println!(
            "Admins: {}\nTitle: {}\nLocation: {}",
            space.admins(),
            &space.data.audio_space.metadata.title,
            location
        )
    }
    let chunks = stream.chunks(guest, bearer).await;
    let chunks: Vec<String> = chunks.split("\n").filter(|c| !c.contains("#")).map(str::to_string).collect();
    let size = chunks.len();
    println!("Chunks: {}", size);
    let mut index = 0;
    let map = CHashMap::new();
    let chunks = chunks.iter()
        .map(|chunk| format!("{}{}", base[0], chunk))
        .map(|chunk| {
            let f = fetch_url(&map, size, index, chunk);
            index += 1;
            f
        });

    let chunks = futures::stream::iter(chunks).buffer_unordered(20);
    chunks.collect::<Vec<_>>().await;

    let name = name.clone().unwrap_or(space.name()) + ".aac";
    let mut file = std::fs::File::create(name).unwrap();
    let mut bytes: Vec<u8> = vec![];
    for i in 0..size {
        println!("Appending chunk #{} of {}", i, size);
        bytes.append(&mut map.get(&(i as i32)).unwrap().to_vec());
    }
    let mut content = Cursor::new(Bytes::from(bytes));
    std::io::copy(&mut content, &mut file).unwrap();
}

async fn fetch_url(map: &CHashMap<i32, Bytes>, size: usize, index: i32, url: String) {
    let op = || {
        println!("Attempting to download chunk #{}", index);
        reqwest::get(url.clone())
    };
    let policy = RetryPolicy::exponential(Duration::from_secs(1))
        .with_max_retries(5)
        .with_jitter(true);
    let response = policy.retry(op).await;
    if response.is_err() {
        panic!("Error while downloading chunk #{}, response: {:?}", index, response)
    }
    let response = response.unwrap();
    map.insert(index, response.bytes().await.unwrap());
    println!("Finished downloading chunk #{} - {}/{} Downloaded", index, map.len(), size);
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

#[derive(Debug, Serialize, Deserialize)]
struct Stream {
    source: Source,
}

#[derive(Debug, Serialize, Deserialize)]
struct Source {
    location: String,
}
